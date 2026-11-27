//! Voice creation and upkeep (step S17), ported from backend/app/main.py: `_run_analysis`, `_preview_job`,
//! `queue_preview`, the start-up previews, `create_profile`, `patch_profile`, `reanalyze`, `_refresh_voice`,
//! `upgrade_all`, `refresh_profile` and `delete_profile`. Without a renderer the paths that start a job answer 501.

use super::*;
use axum::extract::{FromRequest, Multipart};
use std::path::Path as FsPath;
use taketwo_core::reference;
use taketwo_core::render::preview::{self, Preview};

const ANALYZING: &str = "Analyzing voice";
const PREVIEWING: &str = "Making voice preview";
const UPDATING: &str = "Updating voice";
const NOT_FOUND: &str = "Not found";

fn put(lib: &Library, p: &mut Doc) -> Result<(), JobError> {
    lib.put_profile(p).map_err(|e| JobError::new("RuntimeError", e))
}

fn get(lib: &Library, pid: &str) -> Result<Option<Doc>, JobError> {
    lib.get_profile(pid).map_err(|e| JobError::new("RuntimeError", e))
}

fn drop_preview(lib: &Library, p: &mut Doc) -> Result<(), JobError> {
    preview::drop_preview(&lib.profiles_dir(), p).map_err(|e| JobError::new("RuntimeError", e))
}

/// main.py `_run_analysis`: the analyze job, then a preview queued behind it.
pub(super) fn run_analysis(st: &Arc<AppState>, r: Arc<dyn Renderer>, pid: &str, src: PathBuf, denoise: bool, window: f64, studio: bool) -> Value {
    let (st2, pid2) = (st.clone(), pid.to_string());
    let work: worker::Work = Box::new(move |cancel| {
        let lib = &st2.lib;
        let Some(mut p) = get(lib, &pid2)? else { return Err(JobError::new("TypeError", "'NoneType' object does not support item assignment")) };
        p.insert("status".into(), "analyzing".into());
        drop_preview(lib, &mut p)?;
        put(lib, &mut p)?;
        let language = p.get("language").filter(|l| py::truthy(l)).map(py::display).unwrap_or_else(|| "en".into());
        let pdir = lib.profiles_dir().join(&pid2);
        let mut analysis = None;
        let w = &st2.worker;
        let res = r.voice_tools(cancel, &mut |m| {
            let mut progress = |x: f64, msg: &str| w.progress(x, msg);
            analysis = Some(reference::analyze(&src, &pdir, Some(&language), denoise, window, &mut progress, studio, m)?);
            Ok(())
        });
        let failed = match (&res, &analysis) {
            (Err(e), _) => Some(e.clone()),
            // a stop: Python's analyze runs to the end; here the models stop, so the voice is marked as failed
            (Ok(()), None) => Some(JobError::new("RuntimeError", "The analysis was stopped.")),
            _ => None,
        };
        if let Some(e) = failed {
            if let Some(mut p) = get(lib, &pid2)? {
                p.insert("status".into(), "error".into());
                p.insert("error".into(), e.message.as_str().into());
                put(lib, &mut p)?;
            }
            return Err(e);
        }
        let a = analysis.expect("checked above");
        let Some(mut p) = get(lib, &pid2)? else { return Ok(()) }; // deleted meanwhile
        let utts = a.get("utterances").and_then(Value::as_array).cloned().unwrap_or_default();
        let dur: f64 = utts.iter().fold(0.0, |s, u| s + u["dur"].as_f64().unwrap_or(0.0));
        let speech = if dur == 0.0 { 1.0 } else { dur };
        let words: i64 = utts.iter().map(|u| u["n_words"].as_i64().unwrap_or(0)).sum();
        p.remove("noise_baseline");
        p.remove("pitch_hz"); // re-measured from the new sample before the next narration
        let fields = [
            ("status", Value::from("ready")), ("error", Value::Null), ("duration", a["duration"].clone()), ("snr_db", a["snr_db"].clone()),
            ("denoised", a["denoised"].clone()), ("self_similarity", a["self_similarity"].clone()),
            ("usable_speech_seconds", a["usable_speech_seconds"].clone()),
            ("speech_rate_wps", taketwo_core::checks::py_round(words as f64 / speech, 3).into()), ("variants", a["variants"].clone()),
            ("active_variant", 0.into()), ("ref_text_override", json!({})), ("tips", a["tips"].clone()), ("transcript", a["transcript"].clone()),
            ("studio", a["studio"].clone()), ("room_echo_db", a["room_echo_db"].clone()), ("room_echo_after_db", a["room_echo_after_db"].clone()),
            ("pipeline_version", PIPELINE_VERSION.into()),
        ];
        for (k, v) in fields {
            p.insert(k.into(), v);
        }
        put(lib, &mut p)
    });
    let job = st.worker.submit("analyze", work, Some(pid), ANALYZING, None, "unknown");
    queue_preview(st, pid); // runs right after the analysis in the same queue
    job
}

fn preview_work(st: Arc<AppState>, r: Arc<dyn Renderer>, pid: String) -> worker::Work {
    Box::new(move |cancel| {
        let out = r.preview(&st.lib, &pid, cancel)?;
        if out == Preview::Stale && get(&st.lib, &pid)?.is_some() {
            // the voice changed while this preview was being made: make one from the new settings
            let next = preview_work(st.clone(), r.clone(), pid.clone());
            st.worker.submit("preview", next, Some(&format!("{pid}:preview")), PREVIEWING, None, "unknown");
        }
        Ok(())
    })
}

/// main.py `queue_preview`: one preview job per voice at a time (none without a renderer).
pub(super) fn queue_preview(st: &Arc<AppState>, pid: &str) -> Option<Value> {
    let r = st.renderer.clone()?;
    let rf = format!("{pid}:preview");
    if st.worker.active_for(&rf).is_some() {
        return None;
    }
    Some(st.worker.submit("preview", preview_work(st.clone(), r, pid.to_string()), Some(&rf), PREVIEWING, None, "unknown"))
}

/// The start-up loop: a preview for each ready voice without `preview.wav`. Unlike main.py, a job is queued only
/// when the voice model files are there; otherwise each job would end at once with nothing to do.
pub fn queue_startup_previews(st: &Arc<AppState>) -> Result<usize, String> {
    let Some(r) = st.renderer.clone() else { return Ok(0) };
    if !r.can_preview() {
        return Ok(0);
    }
    let mut n = 0;
    for p in st.lib.list_profiles()? {
        let id = p.get("id").map(py::display).unwrap_or_default();
        if p.get("status").and_then(Value::as_str) == Some("ready") && is_plain_name(&id) && !st.lib.profiles_dir().join(&id).join("preview.wav").exists() {
            n += queue_preview(st, &id).is_some() as usize;
        }
    }
    Ok(n)
}

/// `Path(name).suffix.lower() or ".wav"`, kept only when it is a plain extension (letters and digits).
fn upload_ext(filename: &str) -> String {
    let name = filename.rsplit(['/', '\\']).next().unwrap_or("");
    match name.rfind('.') {
        Some(i) if i > 0 && i < name.len() - 1 => {
            let ext = &name[i..];
            if ext.len() <= 12 && ext[1..].bytes().all(|b| b.is_ascii_alphanumeric()) { ext.to_ascii_lowercase() } else { ".wav".into() }
        }
        _ => ".wav".into(),
    }
}

/// main.py `_probe_duration`: ffprobe's duration rounded to 2 places, or None.
fn probe_duration(path: &FsPath) -> Option<f64> {
    let out = std::process::Command::new(std::env::var_os("TAKETWO_FFPROBE").unwrap_or_else(|| "ffprobe".into()))
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse::<f64>().ok().filter(|d| d.is_finite()).map(|d| taketwo_core::checks::py_round(d, 2))
}

fn missing(name: &str) -> Value {
    json!({"type": "missing", "loc": ["body", name], "msg": "Field required", "input": null})
}

/// `POST /api/profiles` (multipart: name, language = "en", denoise = false, studio = true, file).
pub(super) async fn create_profile(State(st): St, req: Request) -> Response {
    let mut fields: HashMap<String, String> = HashMap::new();
    let mut upload: Option<(String, PathBuf)> = None;
    let tmp_dir = st.lib.profiles_dir();
    if let Ok(mut mp) = Multipart::from_request(req, &()).await {
        loop {
            let field = match mp.next_field().await {
                Ok(Some(f)) => f,
                Ok(None) => break,
                Err(e) => return detail(400, &format!("There was an error parsing the body: {e}")),
            };
            let name = field.name().unwrap_or("").to_string();
            if name == "file" && field.file_name().is_some() && upload.is_none() {
                // streamed into a hidden part file under profiles/, moved into the voice folder once it exists
                let filename = field.file_name().unwrap_or("").to_string();
                if std::fs::create_dir_all(&tmp_dir).is_err() {
                    return ise(format!("{}: cannot create", tmp_dir.display()));
                }
                let part = tmp_dir.join(format!(".upload-{}.part", st.worker.new_id()));
                let mut f = match tokio::fs::File::create(&part).await {
                    Ok(f) => f,
                    Err(e) => return ise(format!("{}: {e}", part.display())),
                };
                let mut field = field;
                loop {
                    match field.chunk().await {
                        Ok(Some(b)) => {
                            if let Err(e) = tokio::io::AsyncWriteExt::write_all(&mut f, &b).await {
                                let _ = std::fs::remove_file(&part);
                                return ise(format!("{}: {e}", part.display()));
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            let _ = std::fs::remove_file(&part);
                            return detail(400, &format!("There was an error parsing the body: {e}"));
                        }
                    }
                }
                upload = Some((filename, part));
            } else {
                // the last value of a repeated field wins, as Starlette's form
                fields.insert(name, field.text().await.unwrap_or_default());
            }
        }
    }
    // FastAPI treats an empty form value as not sent (the default, or "Field required")
    fields.retain(|_, v| !v.is_empty());
    let cleanup = |u: &Option<(String, PathBuf)>| {
        if let Some((_, p)) = u {
            let _ = std::fs::remove_file(p);
        }
    };
    let mut errors = Vec::new();
    let name = fields.get("name").cloned();
    if name.is_none() {
        errors.push(missing("name"));
    }
    let language = fields.get("language").cloned().unwrap_or_else(|| "en".into());
    let flag = |k: &str, default: bool, errors: &mut Vec<Value>| match fields.get(k) {
        None => default,
        Some(t) => parse_bool(t).unwrap_or_else(|| {
            errors.push(json!({"type": "bool_parsing", "loc": ["body", k], "msg": "Input should be a valid boolean, unable to interpret input", "input": t}));
            default
        }),
    };
    let denoise = flag("denoise", false, &mut errors);
    let studio = flag("studio", true, &mut errors);
    if upload.is_none() {
        errors.push(missing("file"));
    }
    if !errors.is_empty() {
        cleanup(&upload);
        return invalid(errors);
    }
    let Some(r) = st.renderer.clone() else {
        cleanup(&upload);
        return not_yet(); // nothing is written without a renderer
    };
    let (filename, part) = upload.expect("checked above");
    let pid: String = st.worker.new_id().chars().take(12).collect();
    let pdir = st.lib.profiles_dir().join(&pid);
    let run = || -> Result<Response, String> {
        std::fs::create_dir_all(&pdir).map_err(|e| format!("{}: {e}", pdir.display()))?;
        let fname = if filename.is_empty() { "upload.wav".to_string() } else { filename.clone() };
        let src = pdir.join(format!("original{}", upload_ext(&fname)));
        std::fs::rename(&part, &src).map_err(|e| format!("{}: {e}", src.display()))?;
        let dur = probe_duration(&src);
        let name = name.clone().unwrap_or_default();
        let mut p: Doc = json!({
            "id": pid, "name": if py::strip(&name).is_empty() { "Untitled voice" } else { py::strip(&name) }, "language": language,
            "status": "queued", "original_file": src.file_name().map(|f| f.to_string_lossy().into_owned()), "original_duration": dur,
            "truncated": dur.is_some_and(|d| d != 0.0 && d > reference::MAX_REFERENCE_SECONDS),
        })
        .as_object()
        .expect("an object")
        .clone();
        st.lib.put_profile(&mut p)?;
        p.insert("studio".into(), studio.into());
        st.lib.put_profile(&mut p)?;
        let job = run_analysis(&st, r, &pid, src, denoise, 12.0, studio);
        p.insert("job".into(), job);
        Ok(ok_json(Value::Object(p)))
    };
    run().unwrap_or_else(|e| {
        let _ = std::fs::remove_file(&part);
        ise(e)
    })
}

/// `GET /api/profiles/{pid}`: the voice with its active job.
pub(super) async fn get_profile(State(st): St, Path(pid): Path<String>) -> Response {
    match st.lib.get_profile(&pid) {
        Ok(Some(mut p)) => {
            p.insert("job".into(), st.worker.active_for(&pid).unwrap_or(Value::Null));
            ok_json(Value::Object(p))
        }
        Ok(None) => detail(404, NOT_FOUND),
        Err(e) => ise(e),
    }
}

/// `PATCH /api/profiles/{pid}` (ProfilePatch: name, description, active_variant, ref_text, default_engine).
pub(super) async fn patch_profile(State(st): St, Path(pid): Path<String>, body: Bytes) -> Response {
    let mut f = match Fields::parse(&body) {
        Ok(f) => f,
        Err(e) => return invalid(vec![e]),
    };
    let name = f.text("name", false);
    let description = f.text("description", false);
    let active = match f.body.get("active_variant") {
        None | Some(Value::Null) => None,
        Some(_) => f.int("active_variant"),
    };
    let ref_text = f.text("ref_text", false);
    let default_engine = f.text("default_engine", false);
    if let Some(r) = finish_422(Vec::new(), Some(&f)) {
        return r;
    }
    let mut p = match st.lib.get_profile(&pid) {
        Ok(Some(p)) => p,
        Ok(None) => return detail(404, NOT_FOUND),
        Err(e) => return ise(e),
    };
    if let Some(n) = name {
        if py::strip(&n).is_empty() {
            return detail(400, "Name cannot be empty");
        }
        p.insert("name".into(), py::strip(&n).chars().take(60).collect::<String>().into());
    }
    if let Some(d) = description {
        p.insert("description".into(), py::strip(&d).chars().take(300).collect::<String>().into());
    }
    let mut stale = false;
    if let Some(av) = active {
        let n = p.get("variants").and_then(Value::as_array).map_or(0, Vec::len) as i64;
        if !(0 <= av && av < n) {
            return detail(400, "Bad variant");
        }
        let cur = p.get("active_variant").and_then(Value::as_f64).unwrap_or(0.0);
        stale |= av as f64 != cur;
        p.insert("active_variant".into(), av.into());
    }
    if let Some(t) = ref_text {
        let key = py::display(p.get("active_variant").unwrap_or(&Value::from(0)));
        if !p.get("ref_text_override").is_some_and(Value::is_object) {
            p.insert("ref_text_override".into(), json!({})); // shortcut: Python's setdefault fails on a stored null
        }
        p["ref_text_override"].as_object_mut().expect("set above").insert(key, py::strip(&t).into());
        stale = true;
    }
    if let Some(e) = default_engine {
        stale |= p.get("default_engine").and_then(Value::as_str) != Some(e.as_str());
        p.insert("default_engine".into(), e.into());
    }
    if stale {
        // what the voice sounds like changed -> re-make its preview
        if let Err(e) = preview::drop_preview(&st.lib.profiles_dir(), &mut p) {
            return ise(e);
        }
    }
    if let Err(e) = st.lib.put_profile(&mut p) {
        return ise(e);
    }
    if stale && p.get("status").and_then(Value::as_str) == Some("ready") {
        queue_preview(&st, &pid);
    }
    ok_json(Value::Object(p))
}

/// `POST /api/profiles/{pid}/reanalyze` (Reanalyze: denoise = false, target_window = 12.0, studio = None).
pub(super) async fn reanalyze(State(st): St, Path(pid): Path<String>, body: Bytes) -> Response {
    let mut f = match Fields::parse(&body) {
        Ok(f) => f,
        Err(e) => return invalid(vec![e]),
    };
    let denoise = f.flag("denoise").unwrap_or(false);
    let window = f.float("target_window", false).unwrap_or(12.0);
    let studio = f.flag("studio");
    if let Some(r) = finish_422(Vec::new(), Some(&f)) {
        return r;
    }
    let mut p = match st.lib.get_profile(&pid) {
        Ok(Some(p)) => p,
        Ok(None) => return detail(404, NOT_FOUND),
        Err(e) => return ise(e),
    };
    let Some(r) = st.renderer.clone() else { return not_yet() };
    let Some(orig) = p.get("original_file").map(py::display) else { return ise("KeyError: 'original_file'".into()) };
    p.insert("status".into(), "queued".into());
    if let Err(e) = st.lib.put_profile(&mut p) {
        return ise(e);
    }
    let studio_mode = studio.unwrap_or_else(|| p.get("studio").is_some_and(py::truthy));
    let window = if window > 25.0 { 25.0 } else if window < 6.0 { 6.0 } else { window }; // max(6.0, min(25.0, w))
    ok_json(run_analysis(&st, r, &pid, st.lib.profiles_dir().join(&pid).join(orig), denoise, window, studio_mode))
}

/// main.py `_refresh_voice`: re-cut the clips, keep every choice; a fresh preview follows.
fn refresh_voice(st: &Arc<AppState>, r: Arc<dyn Renderer>, pid: &str) -> Value {
    let (st2, pid2) = (st.clone(), pid.to_string());
    let work: worker::Work = Box::new(move |cancel| {
        let lib = &st2.lib;
        let Some(mut p) = get(lib, &pid2)? else { return Err(JobError::new("AttributeError", "'NoneType' object has no attribute 'get'")) };
        st2.worker.progress(0.1, "Re-cutting reference clips at natural pauses");
        let pdir = lib.profiles_dir().join(&pid2);
        let mut report = None;
        r.voice_tools(cancel, &mut |m| {
            report = Some(reference::refresh_clips(&pdir, &mut p, m)?);
            Ok(())
        })?;
        let Some(report) = report else { return Ok(()) }; // stopped
        p.insert("pipeline_version".into(), PIPELINE_VERSION.into());
        p.insert("refresh_report".into(), report);
        p.remove("noise_baseline");
        drop_preview(lib, &mut p)?;
        put(lib, &mut p)?;
        queue_preview(&st2, &pid2);
        Ok(())
    });
    st.worker.submit("refresh", work, Some(pid), UPDATING, None, "unknown")
}

/// `POST /api/profiles/{pid}/refresh`.
pub(super) async fn refresh_profile(State(st): St, Path(pid): Path<String>) -> Response {
    let p = match st.lib.get_profile(&pid) {
        Ok(Some(p)) => p,
        Ok(None) => return detail(404, NOT_FOUND),
        Err(e) => return ise(e),
    };
    let clean = is_plain_name(&pid) && st.lib.profiles_dir().join(&pid).join("clean.wav").exists();
    if p.get("status").and_then(Value::as_str) != Some("ready") || !clean {
        return detail(400, "This voice isn't ready to update.");
    }
    if st.worker.active_for(&pid).is_some() {
        return detail(409, "This voice is busy.");
    }
    match st.renderer.clone() {
        Some(r) => ok_json(refresh_voice(&st, r, &pid)),
        None => not_yet(),
    }
}

/// `POST /api/profiles/upgrade-all` (UpgradeAll: mode = "refresh", studio = true, only_outdated = true).
pub(super) async fn upgrade_all(st: Arc<AppState>, body: Bytes) -> Response {
    let Some(r) = st.renderer.clone() else { return not_yet() };
    let mut f = match Fields::parse(&body) {
        Ok(f) => f,
        Err(e) => return invalid(vec![e]),
    };
    let mode = match f.get("mode", false) {
        None => "refresh".to_string(),
        Some(Value::String(m)) => m,
        Some(v) => {
            f.fail("string_type", "mode", "Input should be a valid string", &v);
            String::new()
        }
    };
    let studio = f.flag("studio").unwrap_or(true);
    let only_outdated = f.flag("only_outdated").unwrap_or(true);
    if let Some(resp) = finish_422(Vec::new(), Some(&f)) {
        return resp;
    }
    let profiles = match st.lib.list_profiles() {
        Ok(p) => p,
        Err(e) => return ise(e),
    };
    let mut queued = Vec::new();
    for mut p in profiles {
        let id = p.get("id").map(py::display).unwrap_or_default();
        if p.get("status").and_then(Value::as_str) != Some("ready") || !p.get("original_file").is_some_and(py::truthy) || !is_plain_name(&id) {
            continue;
        }
        if only_outdated && p.get("pipeline_version").and_then(Value::as_i64).unwrap_or(0) >= PIPELINE_VERSION {
            continue;
        }
        if mode == "full" {
            p.insert("studio".into(), studio.into());
            p.insert("status".into(), "queued".into());
            if let Err(e) = st.lib.put_profile(&mut p) {
                return ise(e);
            }
            let src = st.lib.profiles_dir().join(&id).join(py::display(&p["original_file"]));
            run_analysis(&st, r.clone(), &id, src, false, 12.0, studio);
        } else {
            refresh_voice(&st, r.clone(), &id);
        }
        queued.push(p.get("name").cloned().unwrap_or(Value::Null));
    }
    ok_json(json!({"queued": queued}))
}

/// `DELETE /api/profiles/{pid}`: the voice's jobs are cancelled first, then the document and the folder go.
pub(super) async fn delete_profile(State(st): St, Path(pid): Path<String>) -> Response {
    if let Err(r) = item_id_ok(&pid) {
        return r;
    }
    if st.renderer.is_none() {
        return not_yet(); // a renderer-less engine (in-process tests) writes nothing
    }
    for rf in [pid.clone(), format!("{pid}:preview")] {
        if let Some(j) = st.worker.active_for(&rf) {
            st.worker.cancel(j["id"].as_str().unwrap_or_default());
        }
    }
    if let Err(e) = st.lib.delete_profile(&pid) {
        return ise(e);
    }
    let _ = std::fs::remove_dir_all(st.lib.profiles_dir().join(&pid));
    ok_json(json!({"ok": true}))
}

#[cfg(test)]
mod tests {
    use super::upload_ext;

    #[test]
    fn upload_extension_as_pathlib_with_plain_characters_only() {
        assert_eq!(upload_ext("Voice.MP3"), ".mp3");
        assert_eq!(upload_ext("a.tar.gz"), ".gz");
        assert_eq!(upload_ext("noext"), ".wav");
        assert_eq!(upload_ext(".wav"), ".wav");
        assert_eq!(upload_ext("a."), ".wav");
        assert_eq!(upload_ext("dir/x.m4a"), ".m4a");
        assert_eq!(upload_ext(r"x.\..\evil"), ".wav");
        assert_eq!(upload_ext("x.w/v"), ".wav");
    }
}
