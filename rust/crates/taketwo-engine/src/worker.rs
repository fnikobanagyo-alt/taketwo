//! One background thread runs every render, ported from backend/app/worker.py: a queue, the jobs map,
//! a cancel flag per job, the 32-event list and the bounded diagnostics. Job documents keep the keys
//! and the key order Python's `submit` builds.

use serde_json::{Map, Value, json};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// What a job raised: the exception class name (`error_type`) and its text.
#[derive(Debug, Clone, PartialEq)]
pub struct JobError {
    pub error_type: String,
    pub message: String,
}

impl JobError {
    pub fn new(error_type: &str, message: impl Into<String>) -> JobError {
        JobError { error_type: error_type.into(), message: message.into() }
    }
}

/// The job's work, given its cancel check (true once cancel was asked; the first call acknowledges it).
pub type Work = Box<dyn FnOnce(&dyn Fn() -> bool) -> Result<(), JobError> + Send>;
/// Runs when a job is cancelled before it starts.
pub type OnCancel = Box<dyn FnOnce() + Send>;
/// 32 hex digits, as `uuid.uuid4().hex`; job ids are its first 10.
pub type Ids = Arc<dyn Fn() -> String + Send + Sync>;

pub const JOB_LIMIT: usize = 20;
pub const EVENT_LIMIT: usize = 32;

pub fn wall_clock() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// Random 32 hex digits from the standard library's per-process random hasher keys.
pub fn random_ids() -> Ids {
    use std::hash::{BuildHasher, Hasher};
    let keys = std::collections::hash_map::RandomState::new();
    let n = std::sync::atomic::AtomicU64::new(0);
    Arc::new(move || {
        let mut out = String::new();
        for _ in 0..2 {
            let mut h = keys.build_hasher();
            h.write_u64(n.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
            h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
            out.push_str(&format!("{:016x}", h.finish()));
        }
        out
    })
}

struct Job {
    /// The public keys plus `cancel`, in Python's order.
    doc: Map<String, Value>,
    events: Vec<Value>,
    phase_clock: Option<Instant>,
    work: Option<Work>,
    on_cancel: Option<OnCancel>,
}

impl Job {
    fn str(&self, k: &str) -> &str {
        self.doc.get(k).and_then(Value::as_str).unwrap_or_default()
    }
    fn active(&self) -> bool {
        matches!(self.str("status"), "queued" | "running")
    }
    fn created(&self) -> f64 {
        self.doc.get("created").and_then(Value::as_f64).unwrap_or(0.0)
    }
    fn cancel_flag(&self) -> bool {
        self.doc.get("cancel").and_then(Value::as_bool).unwrap_or(false)
    }
    fn set(&mut self, k: &str, v: impl Into<Value>) {
        self.doc.insert(k.into(), v.into());
    }

    /// worker.py `_event`.
    fn event(&mut self, event: &str) {
        let now = wall_clock();
        let phase = self.doc.get("phase").cloned().unwrap_or(Value::Null);
        self.events.push(json!({"at": now, "event": event, "phase": phase}));
        if ["started", "phase_started", "phase_finished", "finished"].contains(&event) {
            self.set("last_progress_at", now);
        }
        let extra = self.events.len().saturating_sub(EVENT_LIMIT);
        self.events.drain(..extra);
    }
}

#[derive(Default)]
struct Inner {
    /// Insertion order, as Python's `jobs` dict.
    jobs: Vec<Job>,
    queue: VecDeque<String>,
    /// The job the thread is running now, if any.
    current: Option<String>,
    stop: bool,
}

impl Inner {
    fn job(&mut self, id: &str) -> Option<&mut Job> {
        self.jobs.iter_mut().find(|j| j.str("id") == id)
    }
}

struct Shared {
    inner: Mutex<Inner>,
    wake: Condvar,
    ids: Ids,
    started_at: f64,
}

#[derive(Clone)]
pub struct Worker {
    shared: Arc<Shared>,
}

fn public(j: &Job) -> Value {
    Value::Object(j.doc.iter().filter(|(k, _)| !k.starts_with('_')).map(|(k, v)| (k.clone(), v.clone())).collect())
}

impl Worker {
    /// Start the one worker thread.
    pub fn start(ids: Ids) -> Worker {
        let shared = Arc::new(Shared { inner: Mutex::new(Inner::default()), wake: Condvar::new(), ids, started_at: wall_clock() });
        let s = shared.clone();
        std::thread::Builder::new().name("taketwo-worker".into()).spawn(move || run_loop(&s)).expect("worker thread");
        Worker { shared }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.shared.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// A fresh 32-hex id from the same source as job ids (create uses its first 12 for a narration).
    pub fn new_id(&self) -> String {
        (self.shared.ids)()
    }

    /// worker.py `submit`; returns the public job.
    pub fn submit(&self, kind: &str, work: Work, r#ref: Option<&str>, label: &str, on_cancel: Option<OnCancel>, source: &str) -> Value {
        let id: String = self.new_id().chars().take(10).collect();
        let created = wall_clock();
        let mut doc = Map::new();
        for (k, v) in [
            ("id", Value::from(id.as_str())),
            ("kind", kind.into()),
            ("ref", r#ref.map(Value::from).unwrap_or(Value::Null)),
            ("label", label.into()),
            ("status", "queued".into()),
            ("progress", 0.0.into()),
            ("message", "Queued".into()),
            ("error", Value::Null),
            ("created", created.into()),
            ("cancel", false.into()),
            ("source", (if ["api", "mcp", "desktop"].contains(&source) { source } else { "unknown" }).into()),
            ("phase", "queued".into()),
            ("phase_started_at", created.into()),
            ("last_progress_at", Value::Null),
            ("cancel_requested_at", Value::Null),
            ("cancel_acknowledged_at", Value::Null),
        ] {
            doc.insert(k.into(), v);
        }
        let mut job = Job { doc, events: Vec::new(), phase_clock: Some(Instant::now()), work: Some(work), on_cancel };
        job.event("submitted");
        let answer = public(&job);
        let mut inner = self.lock();
        inner.jobs.push(job);
        inner.queue.push_back(id);
        self.shared.wake.notify_all();
        answer
    }

    /// worker.py `cancel`: a queued job turns `cancelled` at once; the loop acknowledges it when it gets there.
    pub fn cancel(&self, id: &str) {
        let mut inner = self.lock();
        if let Some(j) = inner.job(id).filter(|j| j.active()) {
            if !j.cancel_flag() {
                j.set("cancel_requested_at", wall_clock());
                j.event("cancel_requested");
            }
            j.set("cancel", true);
            if j.str("status") == "queued" {
                j.set("status", "cancelled");
            }
        }
    }

    /// worker.py `active_for`: the queued or running job for this narration.
    pub fn active_for(&self, r#ref: &str) -> Option<Value> {
        let inner = self.lock();
        inner.jobs.iter().find(|j| j.doc.get("ref").and_then(Value::as_str) == Some(r#ref) && j.active()).map(public)
    }

    /// `worker.jobs.get(id)` as the public job.
    pub fn get(&self, id: &str) -> Option<Value> {
        self.lock().jobs.iter().find(|j| j.str("id") == id).map(public)
    }

    /// main.py `_active_jobs`: queued and running, in the order the worker takes them.
    pub fn active_jobs(&self) -> Vec<Value> {
        let inner = self.lock();
        let mut v: Vec<&Job> = inner.jobs.iter().filter(|j| j.active()).collect();
        v.sort_by(|a, b| a.created().total_cmp(&b.created()));
        v.into_iter().map(public).collect()
    }

    /// No job queued or running (the idle exit waits for this).
    pub fn idle(&self) -> bool {
        let inner = self.lock();
        inner.current.is_none() && !inner.jobs.iter().any(Job::active)
    }

    /// Set the cancel flag of the running job (shutdown).
    pub fn cancel_running(&self) {
        let running = self.lock().current.clone();
        if let Some(id) = running {
            self.cancel(&id);
        }
    }

    /// Ask the thread to stop after the job it runs now; true when it went idle within `wait`.
    pub fn stop(&self, wait: Duration) -> bool {
        self.lock().stop = true;
        self.shared.wake.notify_all();
        let until = Instant::now() + wait;
        while Instant::now() < until {
            if self.lock().current.is_none() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// worker.py `phase`: annotate a blocking step of the job the thread runs now.
    pub fn phase<T>(&self, name: &str, f: impl FnOnce() -> T) -> T {
        let id = self.lock().current.clone();
        let Some(id) = id else { return f() };
        let previous = {
            let mut inner = self.lock();
            let Some(j) = inner.job(&id) else { return f() };
            let prev = (j.doc.get("phase").cloned(), j.doc.get("phase_started_at").cloned(), j.phase_clock);
            j.set("phase", name);
            j.set("phase_started_at", wall_clock());
            j.phase_clock = Some(Instant::now());
            j.event("phase_started");
            prev
        };
        let out = f();
        let mut inner = self.lock();
        if let Some(j) = inner.job(&id) {
            j.event("phase_finished");
            j.set("phase", previous.0.unwrap_or(Value::Null));
            j.set("phase_started_at", previous.1.unwrap_or(Value::Null));
            j.phase_clock = previous.2;
        }
        out
    }

    /// worker.py `diagnostics`. `source_sha256` is the Rust engine's build stamp.
    pub fn diagnostics(&self, build_stamp: &str) -> Value {
        const FIELDS: [&str; 14] = ["id", "kind", "ref", "source", "status", "created", "started", "finished", "phase",
            "phase_started_at", "last_progress_at", "cancel_requested_at", "cancel_acknowledged_at", "error_type"];
        let inner = self.lock();
        let rank = |j: &Job| match j.str("status") {
            "running" => 2,
            "queued" => 1,
            _ => 0,
        };
        let mut recent: Vec<&Job> = inner.jobs.iter().collect();
        // sorted(..., reverse=True) keeps equal keys in their original order, as a stable descending sort does.
        recent.sort_by(|a, b| (rank(b), b.created()).partial_cmp(&(rank(a), a.created())).unwrap_or(std::cmp::Ordering::Equal));
        let jobs: Vec<Value> = recent
            .into_iter()
            .take(JOB_LIMIT)
            .map(|j| {
                let mut e: Map<String, Value> = FIELDS.iter().map(|k| (k.to_string(), j.doc.get(*k).cloned().unwrap_or(Value::Null))).collect();
                let elapsed = j.phase_clock.map(|c| (c.elapsed().as_secs_f64() * 1000.0).round() / 1000.0);
                e.insert("phase_elapsed_seconds".into(), elapsed.map(Value::from).unwrap_or(Value::Null));
                e.insert("events".into(), Value::Array(j.events.clone()));
                Value::Object(e)
            })
            .collect();
        json!({"runtime": {"pid": std::process::id(), "started_at": self.shared.started_at, "source_sha256": build_stamp},
               "jobs": jobs, "job_limit": JOB_LIMIT, "event_limit": EVENT_LIMIT})
    }
}

/// worker.py `_cancel_requested`.
fn cancel_requested(shared: &Shared, id: &str) -> bool {
    let mut inner = shared.inner.lock().unwrap_or_else(|p| p.into_inner());
    let Some(j) = inner.job(id) else { return false };
    if j.cancel_flag() && j.doc.get("cancel_acknowledged_at").is_none_or(Value::is_null) {
        j.set("cancel_acknowledged_at", wall_clock());
        j.event("cancel_acknowledged");
    }
    j.cancel_flag()
}

/// worker.py `_loop` (the idle hooks are not ported: the engine's idle exit lives in serve).
fn run_loop(shared: &Shared) {
    let lock = || shared.inner.lock().unwrap_or_else(|p| p.into_inner());
    loop {
        let (id, work, on_cancel) = {
            let mut inner = lock();
            loop {
                if inner.stop {
                    return;
                }
                if let Some(id) = inner.queue.pop_front() {
                    let j = inner.job(&id).expect("queued job is in the map");
                    let (w, c) = (j.work.take(), j.on_cancel.take());
                    inner.current = Some(id.clone());
                    break (id, w, c);
                }
                inner = shared.wake.wait(inner).unwrap_or_else(|p| p.into_inner());
            }
        };
        if cancel_requested(shared, &id) {
            if let Some(j) = lock().job(&id) {
                j.set("status", "cancelled");
            }
            if let Some(f) = on_cancel {
                f();
            }
            let mut inner = lock();
            if let Some(j) = inner.job(&id) {
                j.set("finished", wall_clock());
                j.set("phase", "cancelled");
                j.phase_clock = None;
                j.event("finished");
            }
            inner.current = None;
            continue;
        }
        {
            let mut inner = lock();
            let j = inner.job(&id).expect("running job is in the map");
            let now = wall_clock();
            j.set("status", "running");
            j.set("started", now);
            j.set("phase", "processing");
            j.set("phase_started_at", now);
            j.phase_clock = Some(Instant::now());
            j.event("started");
        }
        let cancel = || cancel_requested(shared, &id);
        let result = match work {
            Some(w) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| w(&cancel)))
                .unwrap_or_else(|_| Err(JobError::new("RuntimeError", "the job panicked"))),
            None => Ok(()),
        };
        let cancelled = result.is_ok() && cancel_requested(shared, &id);
        let mut inner = lock();
        if let Some(j) = inner.job(&id) {
            match result {
                Ok(()) => {
                    j.set("status", if cancelled { "cancelled" } else { "done" });
                    j.set("progress", 1.0);
                }
                Err(e) => {
                    eprintln!("taketwo-engine: job {id} failed: {}: {}", e.error_type, e.message);
                    let msg = if e.message.is_empty() { e.error_type.clone() } else { e.message };
                    j.set("status", "error");
                    j.set("error_type", e.error_type);
                    j.set("error", msg.as_str());
                    j.set("message", msg);
                }
            }
            let status = j.doc["status"].clone();
            j.set("finished", wall_clock());
            j.set("phase", status);
            j.phase_clock = None;
            j.event("finished");
        }
        inner.current = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counter() -> Ids {
        let n = std::sync::atomic::AtomicU64::new(0);
        Arc::new(move || {
            let n = n.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            format!("{n:010x}{n:022x}")
        })
    }

    fn wait_until(w: &Worker, id: &str, status: &str) -> Value {
        for _ in 0..500 {
            let j = w.get(id).unwrap();
            if j["status"] == status && j.get("finished").is_some_and(|f| !f.is_null()) {
                return j;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("job {id} never reached {status}: {:?}", w.get(id));
    }

    #[test]
    fn job_keys_in_python_order() {
        let w = Worker::start(counter());
        let j = w.submit("generate", Box::new(|_| Ok(())), Some("g1"), "Generating", None, "app");
        let keys: Vec<&str> = j.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["id", "kind", "ref", "label", "status", "progress", "message", "error", "created", "cancel", "source",
            "phase", "phase_started_at", "last_progress_at", "cancel_requested_at", "cancel_acknowledged_at"]);
        assert_eq!((j["id"].as_str(), j["source"].as_str()), (Some("0000000001"), Some("unknown")));
        let done = wait_until(&w, "0000000001", "done");
        let keys: Vec<&str> = done.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(&keys[16..], ["started", "finished"]);
        assert_eq!((done["phase"].as_str(), done["progress"].as_f64()), (Some("done"), Some(1.0)));
        let failed = w.submit("generate", Box::new(|_| Err(JobError::new("ValueError", ""))), None, "", None, "mcp");
        let f = wait_until(&w, failed["id"].as_str().unwrap(), "error");
        let keys: Vec<&str> = f.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(&keys[16..], ["started", "error_type", "finished"]);
        assert_eq!((f["error"].as_str(), f["message"].as_str()), (Some("ValueError"), Some("ValueError")));
        let d = w.diagnostics("stamp");
        assert_eq!(d["jobs"][0]["id"], f["id"], "newest first");
        assert_eq!(d["jobs"][0]["events"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn phase_annotates_the_running_job_and_events_are_bounded() {
        let w = Worker::start(counter());
        let w2 = w.clone();
        let j = w.submit("generate", Box::new(move |_| {
            for _ in 0..20 {
                w2.phase("tts", || ());
            }
            Ok(())
        }), None, "", None, "api");
        let id = j["id"].as_str().unwrap().to_string();
        wait_until(&w, &id, "done");
        let d = w.diagnostics("stamp");
        let ev = d["jobs"][0]["events"].as_array().unwrap();
        assert_eq!(ev.len(), EVENT_LIMIT);
        assert_eq!(ev.last().unwrap()["event"], "finished");
        assert!(w.phase("outside", || 7) == 7, "a no-op outside a job");
    }
}
