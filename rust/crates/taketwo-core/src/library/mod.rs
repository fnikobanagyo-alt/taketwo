//! The library: profiles and generations as JSON documents in SQLite, the same file and schema as
//! VoiceForge's backend/app/db.py, and the folder layout of backend/app/config.py.
//! Startup repairs (main.py), settings (settings.rs) and the one-time copy (copy.rs) live here too.

mod copy;
mod settings;

pub use copy::copy_library_once;
pub use settings::{SETTING_RULES, checked, default_settings, get_settings, save_settings};

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

pub type Doc = Map<String, Value>;
type Clock = Box<dyn Fn() -> f64 + Send + Sync>;

pub const DB_FILE: &str = "voiceforge.db";
pub const SETTINGS_FILE: &str = "settings.json";
pub const SUBFOLDERS: [&str; 4] = ["profiles", "generations", "designs", "transcripts"];
/// Fields the user edits from the UI; a running narration never writes them back (db.py GEN_META_KEYS).
pub const GEN_META_KEYS: [&str; 2] = ["title", "favorite"];
pub const PURGE_AFTER_S: f64 = 7.0 * 86400.0;
pub const INTERRUPTED_MESSAGE: &str =
    "Stopped when the app closed. Finished parts are kept; press Resume to finish the rest.";
pub const ANALYSIS_INTERRUPTED: &str = "Analysis was interrupted. Press Re-analyze.";

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS profiles (id TEXT PRIMARY KEY, created REAL, doc TEXT);
    CREATE TABLE IF NOT EXISTS generations (id TEXT PRIMARY KEY, created REAL, profile_id TEXT, doc TEXT);
    ";

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// True when `id` is one plain folder name, so joining it can never leave the library.
pub fn is_plain_name(id: &str) -> bool {
    let mut c = Path::new(id).components();
    !id.contains(['/', '\\']) && matches!((c.next(), c.next()), (Some(Component::Normal(_)), None))
}

fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// The value as Python's sqlite3 would bind it (float, str, None, bool as int).
fn sql_value(v: Option<&Value>) -> Result<rusqlite::types::Value, String> {
    use rusqlite::types::Value as S;
    Ok(match v {
        None | Some(Value::Null) => S::Null,
        Some(Value::Bool(b)) => S::Integer(*b as i64),
        Some(Value::Number(n)) => n.as_i64().map(S::Integer).unwrap_or_else(|| S::Real(n.as_f64().unwrap_or(0.0))),
        Some(Value::String(s)) => S::Text(s.clone()),
        Some(other) => return Err(format!("cannot store {other} in a column")),
    })
}

fn parse_doc(text: &str) -> Result<Doc, String> {
    // shortcut: Python's json also reads NaN/Infinity; serde_json refuses them. Upgrade if a real library has them.
    match serde_json::from_str(text).map_err(err)? {
        Value::Object(m) => Ok(m),
        _ => Err("stored document is not an object".into()),
    }
}

pub struct Library {
    dir: PathBuf,
    db: Mutex<Connection>,
    clock: Clock,
}

impl Library {
    /// Open (or create) the library in `dir`: the folders of config.py and `voiceforge.db` with db.py's schema.
    pub fn open(dir: &Path) -> Result<Library, String> {
        for d in std::iter::once(dir.to_path_buf()).chain(SUBFOLDERS.iter().map(|s| dir.join(s))) {
            std::fs::create_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        let db = Connection::open(dir.join(DB_FILE)).map_err(err)?;
        db.execute_batch(SCHEMA).map_err(err)?;
        Ok(Library { dir: dir.to_path_buf(), db: Mutex::new(db), clock: Box::new(now) })
    }

    /// Replace the wall clock (seconds since 1970), so tests are exact.
    pub fn with_clock(mut self, clock: impl Fn() -> f64 + Send + Sync + 'static) -> Library {
        self.clock = Box::new(clock);
        self
    }

    /// One reading of the library clock (pipeline.py's `time.time()`; tests pin it with `with_clock`).
    pub fn now(&self) -> f64 {
        (self.clock)()
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
    pub fn profiles_dir(&self) -> PathBuf {
        self.dir.join("profiles")
    }
    pub fn generations_dir(&self) -> PathBuf {
        self.dir.join("generations")
    }
    pub fn designs_dir(&self) -> PathBuf {
        self.dir.join("designs")
    }
    pub fn transcripts_dir(&self) -> PathBuf {
        self.dir.join("transcripts")
    }
    pub fn settings_path(&self) -> PathBuf {
        self.dir.join(SETTINGS_FILE)
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn put(&self, table: &str, doc: &mut Doc) -> Result<(), String> {
        let created = (self.clock)(); // db.py evaluates time.time() for setdefault even when `created` exists
        doc.entry("created").or_insert(created.into());
        doc.insert("updated".into(), (self.clock)().into());
        let id = sql_value(doc.get("id"))?;
        let created = sql_value(doc.get("created"))?;
        let text = serde_json::to_string(doc).map_err(err)?;
        let conn = self.conn();
        if table == "generations" {
            let pid = sql_value(doc.get("profile_id"))?;
            conn.execute(
                "INSERT OR REPLACE INTO generations (id,created,profile_id,doc) VALUES (?,?,?,?)",
                params![id, created, pid, text],
            )
        } else {
            conn.execute("INSERT OR REPLACE INTO profiles (id,created,doc) VALUES (?,?,?)", params![id, created, text])
        }
        .map_err(err)?;
        Ok(())
    }

    fn get(conn: &Connection, table: &str, id: &str) -> Result<Option<Doc>, String> {
        let text: Option<String> = conn
            .query_row(&format!("SELECT doc FROM {table} WHERE id=?"), [id], |r| r.get(0))
            .optional()
            .map_err(err)?;
        text.map(|t| parse_doc(&t)).transpose()
    }

    fn list(&self, table: &str) -> Result<Vec<Doc>, String> {
        let conn = self.conn();
        let mut st = conn.prepare(&format!("SELECT doc FROM {table} ORDER BY created DESC")).map_err(err)?;
        let rows = st.query_map([], |r| r.get::<_, String>(0)).map_err(err)?;
        rows.map(|t| parse_doc(&t.map_err(err)?)).collect()
    }

    fn delete(&self, table: &str, id: &str) -> Result<(), String> {
        self.conn().execute(&format!("DELETE FROM {table} WHERE id=?"), [id]).map_err(err)?;
        Ok(())
    }

    fn update_doc(conn: &Connection, id: &str, doc: &Doc) -> Result<(), String> {
        let text = serde_json::to_string(doc).map_err(err)?;
        conn.execute("UPDATE generations SET doc=? WHERE id=?", params![text, id]).map_err(err)?;
        Ok(())
    }

    pub fn put_profile(&self, doc: &mut Doc) -> Result<(), String> {
        self.put("profiles", doc)
    }
    pub fn get_profile(&self, id: &str) -> Result<Option<Doc>, String> {
        Self::get(&self.conn(), "profiles", id)
    }
    pub fn list_profiles(&self) -> Result<Vec<Doc>, String> {
        self.list("profiles")
    }
    pub fn delete_profile(&self, id: &str) -> Result<(), String> {
        self.delete("profiles", id)
    }

    /// Also writes the `profile_id` column.
    pub fn put_generation(&self, doc: &mut Doc) -> Result<(), String> {
        self.put("generations", doc)
    }
    pub fn get_generation(&self, id: &str) -> Result<Option<Doc>, String> {
        Self::get(&self.conn(), "generations", id)
    }
    pub fn list_generations(&self) -> Result<Vec<Doc>, String> {
        self.list("generations")
    }
    pub fn delete_generation(&self, id: &str) -> Result<(), String> {
        self.delete("generations", id)
    }

    /// Worker save in one locked read-modify-write. False, writing nothing, when the row is gone, so a stale
    /// worker never recreates a deleted narration. Title, favourite and a soft delete made meanwhile are kept.
    pub fn put_generation_live(&self, doc: &mut Doc) -> Result<bool, String> {
        let id = doc.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
        let conn = self.conn();
        let Some(cur) = Self::get(&conn, "generations", &id)? else { return Ok(false) };
        for k in GEN_META_KEYS.iter().chain(["deleted_at"].iter()) {
            if let Some(v) = cur.get(*k) {
                doc.insert((*k).into(), v.clone());
            }
        }
        doc.insert("updated".into(), (self.clock)().into());
        Self::update_doc(&conn, &id, doc)?;
        Ok(true)
    }

    /// Soft delete (hidden, restorable) or restore. Files stay until `purge_deleted_generations`.
    pub fn set_generation_deleted(&self, id: &str, deleted: bool) -> Result<Option<Doc>, String> {
        let conn = self.conn();
        let Some(mut doc) = Self::get(&conn, "generations", id)? else { return Ok(None) };
        if deleted {
            doc.insert("deleted_at".into(), (self.clock)().into());
        } else {
            doc.shift_remove("deleted_at");
        }
        Self::update_doc(&conn, id, &doc)?;
        Ok(Some(doc))
    }

    /// Ids of soft-deleted narrations older than the cut-off, removed from the database (the caller removes files).
    pub fn purge_deleted_generations(&self, older_than_s: f64) -> Result<Vec<String>, String> {
        let cut = (self.clock)() - older_than_s;
        let mut gone = Vec::new();
        for g in self.list_generations()? {
            // Python: `g.get("deleted_at") and g["deleted_at"] < cut`; 0 and missing are never purged.
            let at = g.get("deleted_at").and_then(Value::as_f64).filter(|t| *t != 0.0);
            if at.is_some_and(|t| t < cut) {
                gone.push(g.get("id").and_then(Value::as_str).unwrap_or_default().to_string());
            }
        }
        for id in &gone {
            self.delete_generation(id)?;
        }
        Ok(gone)
    }

    /// Apply UI metadata edits (`GEN_META_KEYS` only) to the stored narration, atomically.
    pub fn patch_generation_meta(&self, id: &str, fields: &Doc) -> Result<Option<Doc>, String> {
        let conn = self.conn();
        let Some(mut doc) = Self::get(&conn, "generations", id)? else { return Ok(None) };
        for k in GEN_META_KEYS {
            if let Some(v) = fields.get(k) {
                doc.insert(k.into(), v.clone());
            }
        }
        doc.insert("updated".into(), (self.clock)().into());
        Self::update_doc(&conn, id, &doc)?;
        Ok(Some(doc))
    }
}

fn status_is(doc: &Doc, any: &[&str]) -> bool {
    doc.get("status").and_then(Value::as_str).is_some_and(|s| any.contains(&s))
}

/// The startup repairs of backend/app/main.py, in its order. Returns the purged narration ids.
/// The transcripts repair is not ported (Speech to Text is outside 1.0).
pub fn startup_repairs(lib: &Library) -> Result<Vec<String>, String> {
    for mut g in lib.list_generations()? {
        if status_is(&g, &["queued", "running"]) {
            g.insert("status".into(), "interrupted".into());
            g.insert("message".into(), INTERRUPTED_MESSAGE.into());
            for c in g.get_mut("chunks").and_then(Value::as_array_mut).into_iter().flatten() {
                if c.get("status").and_then(Value::as_str) == Some("running") {
                    c["status"] = "pending".into();
                }
            }
            lib.put_generation(&mut g)?;
        }
    }
    let gone = lib.purge_deleted_generations(PURGE_AFTER_S)?;
    for id in gone.iter().filter(|id| is_plain_name(id)) {
        let _ = std::fs::remove_dir_all(lib.generations_dir().join(id)); // ignore_errors=True, as Python
    }
    for mut p in lib.list_profiles()? {
        if status_is(&p, &["queued", "analyzing"]) {
            p.insert("status".into(), "error".into());
            p.insert("error".into(), ANALYSIS_INTERRUPTED.into());
            lib.put_profile(&mut p)?;
        }
    }
    Ok(gone)
}
