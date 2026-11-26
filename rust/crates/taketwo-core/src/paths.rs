//! Where TakeTwo keeps its files, and the isolation rules from rust/PLAN.md ("Isolation built into the programs").
//!
//! Every function takes its base folders as parameters, so tests run on temporary folders and never
//! reach Franz's real app-data folders. Only `live_app_data_root` looks at the real home folder.

use std::ffi::OsStr;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

/// Ports Franz's running app may use (8765 included as a margin). Isolated runs never bind these.
pub const LIVE_RESERVED_PORTS: RangeInclusive<u16> = 8765..=8785;
/// Ports a live engine publishes in the address file, as `_publish_address` in backend/app/main.py.
pub const LIVE_PUBLISH_PORTS: RangeInclusive<u16> = 8766..=8785;
/// Dev ports; 8796 and 8797 are busy serving pages on this Mac.
pub const DEV_PORTS: RangeInclusive<u16> = 8790..=8799;
pub const DEV_PORTS_BUSY: [u16; 2] = [8796, 8797];

/// App-data folder names. The address file is written under both while other apps still look for VoiceForge.
pub const APP_NAMES: [&str; 2] = ["TakeTwo", "VoiceForge"];
pub const ADDRESS_FILE: &str = "engine.json";
pub const LOCK_FILE: &str = "engine.lock";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// `TAKETWO_DATA=<dir>`: library, lock file and address file all under `data`.
    Isolated { data: PathBuf },
    /// `--live` without `TAKETWO_DATA`: Franz's real library.
    Live,
}

impl Mode {
    /// Decide the mode from the value of `TAKETWO_DATA` and whether `--live` was given.
    /// An empty `TAKETWO_DATA` counts as unset. Both together is refused, so it can never end up live by mistake.
    pub fn resolve(taketwo_data: Option<&OsStr>, live_flag: bool) -> Result<Mode, String> {
        match (taketwo_data.filter(|d| !d.is_empty()), live_flag) {
            (Some(_), true) => Err("TAKETWO_DATA and --live were both given; use one of them".into()),
            (Some(d), false) => Ok(Mode::Isolated { data: PathBuf::from(d) }),
            (None, true) => Ok(Mode::Live),
            (None, false) => Err(
                "refusing to start: set TAKETWO_DATA=<folder> for an isolated run, or pass --live to use the real library"
                    .into(),
            ),
        }
    }

    /// Read `TAKETWO_DATA` from the environment and `--live` from the arguments.
    pub fn from_env_and_args(args: &[String]) -> Result<Mode, String> {
        let data = std::env::var_os("TAKETWO_DATA");
        Mode::resolve(data.as_deref(), args.iter().any(|a| a == "--live"))
    }

    pub fn is_live(&self) -> bool {
        matches!(self, Mode::Live)
    }

    /// The one-time copy from VoiceForge's `data/` runs only in live mode.
    pub fn library_copy_enabled(&self) -> bool {
        self.is_live()
    }

    /// The library folder: `TAKETWO_DATA` itself when isolated (same meaning as Python's data folder),
    /// `<app data>/TakeTwo/library` when live.
    pub fn library_dir(&self, app_data_root: &Path) -> PathBuf {
        match self {
            Mode::Isolated { data } => data.clone(),
            Mode::Live => live_library_dir(app_data_root),
        }
    }

    pub fn lock_file(&self, app_data_root: &Path) -> PathBuf {
        match self {
            Mode::Isolated { data } => data.join(LOCK_FILE),
            Mode::Live => app_data_root.join(APP_NAMES[0]).join(LOCK_FILE),
        }
    }

    /// Where the address file lives: under the data folder when isolated, under both app-data names when live.
    /// `publish_address` only ever writes in live mode.
    pub fn address_files(&self, app_data_root: &Path) -> Vec<PathBuf> {
        match self {
            Mode::Isolated { data } => vec![data.join(ADDRESS_FILE)],
            Mode::Live => APP_NAMES.iter().map(|n| app_data_root.join(n).join(ADDRESS_FILE)).collect(),
        }
    }

    /// Ports to try when none is given: 8766-8785 live, 8790-8799 (without the busy ones) isolated.
    pub fn default_ports(&self) -> Vec<u16> {
        match self {
            Mode::Live => LIVE_PUBLISH_PORTS.collect(),
            Mode::Isolated { .. } => DEV_PORTS.filter(|p| !DEV_PORTS_BUSY.contains(p)).collect(),
        }
    }

    /// Refuse Franz's ports outside live mode.
    pub fn check_port(&self, port: u16) -> Result<(), String> {
        if !self.is_live() && LIVE_RESERVED_PORTS.contains(&port) {
            return Err(format!(
                "port {port} is reserved for the live app (8765-8785); isolated runs use 8790-8795, 8798, 8799"
            ));
        }
        Ok(())
    }
}

/// The real app-data root: `~/Library/Application Support` on Mac, `%LOCALAPPDATA%` on Windows.
/// Never call this from tests.
pub fn live_app_data_root() -> Option<PathBuf> {
    dirs::data_local_dir()
}

/// Mac `~/Library/Application Support/TakeTwo/library`, Windows `%LOCALAPPDATA%\TakeTwo\library`. Computed only.
pub fn live_library_dir(app_data_root: &Path) -> PathBuf {
    app_data_root.join(APP_NAMES[0]).join("library")
}

/// The address file text, the same JSON shape as Python's `json.dumps({"url", "pid", "started"})`.
pub fn address_json(host: &str, port: u16, pid: u32, started: f64) -> String {
    // `{:?}` prints a float the way Python's repr does (1.0 stays "1.0").
    format!("{{\"url\": \"http://{host}:{port}\", \"pid\": {pid}, \"started\": {started:?}}}")
}

/// Write the address file the way `_publish_address` does: live mode only, port 8766-8785 only,
/// write errors ignored (discovery falls back to scanning). Returns how many files were written.
pub fn publish_address(mode: &Mode, files: &[PathBuf], host: &str, port: u16, pid: u32, started: f64) -> usize {
    if !mode.is_live() || !LIVE_PUBLISH_PORTS.contains(&port) {
        return 0;
    }
    let info = address_json(host, port, pid, started);
    files
        .iter()
        .filter(|f| {
            f.parent().is_some_and(|p| std::fs::create_dir_all(p).is_ok()) && std::fs::write(f, &info).is_ok()
        })
        .count()
}

/// `TAKETWO_MAX_TAKES=<n>`: the hard cap on synthesis calls for one engine run. Unset or empty means no cap;
/// anything that is not a whole number of at least 1 is an error, never "no cap".
pub fn parse_max_takes(value: Option<&OsStr>) -> Result<Option<u32>, String> {
    let Some(v) = value.filter(|v| !v.is_empty()) else { return Ok(None) };
    match v.to_str().and_then(|s| s.trim().parse::<u32>().ok()) {
        Some(n) if n >= 1 => Ok(Some(n)),
        _ => Err(format!("TAKETWO_MAX_TAKES must be a whole number of at least 1, got {v:?}")),
    }
}

/// One engine at a time: an OS file lock on `engine.lock`, held for as long as this value lives.
/// The OS drops the lock when the process dies, so a lock file left by a dead process is simply taken over.
#[derive(Debug)]
pub struct EngineLock {
    _file: File,
    pub path: PathBuf,
}

impl EngineLock {
    pub fn acquire(path: &Path) -> Result<EngineLock, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                let holder = std::fs::read_to_string(path).unwrap_or_default();
                return Err(format!(
                    "another TakeTwo engine is running for this library (lock {}, pid {})",
                    path.display(),
                    holder.trim()
                ));
            }
            Err(TryLockError::Error(e)) => return Err(format!("cannot lock {}: {e}", path.display())),
        }
        // The pid is for people reading the file; the lock itself is what counts.
        file.set_len(0).and_then(|_| write!(file, "{}", std::process::id())).map_err(|e| e.to_string())?;
        Ok(EngineLock { _file: file, path: path.to_path_buf() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn isolated(dir: &Path) -> Mode {
        Mode::Isolated { data: dir.to_path_buf() }
    }

    #[test]
    fn taketwo_data_means_isolated_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let mode = Mode::resolve(Some(tmp.path().as_os_str()), false).unwrap();
        assert_eq!(mode, isolated(tmp.path()));
        assert!(!mode.library_copy_enabled());
    }

    #[test]
    fn isolated_mode_keeps_library_lock_and_address_under_the_data_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("fake-app-data");
        let data = tmp.path().join("data");
        let mode = isolated(&data);
        assert_eq!(mode.library_dir(&root), data);
        assert_eq!(mode.lock_file(&root), data.join("engine.lock"));
        assert_eq!(mode.address_files(&root), vec![data.join("engine.json")]);
        assert!(!root.exists());
    }

    #[test]
    fn without_taketwo_data_it_refuses_unless_live() {
        assert!(Mode::resolve(None, false).unwrap_err().contains("refusing to start"));
        assert!(Mode::resolve(Some(OsStr::new("")), false).is_err(), "empty TAKETWO_DATA counts as unset");
        assert_eq!(Mode::resolve(None, true).unwrap(), Mode::Live);
        assert_eq!(Mode::resolve(Some(OsStr::new("")), true).unwrap(), Mode::Live);
    }

    #[test]
    fn taketwo_data_with_live_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(Mode::resolve(Some(tmp.path().as_os_str()), true).is_err());
    }

    #[test]
    fn live_flag_is_read_from_the_arguments() {
        // Only the argument half is checked here; the environment is left alone so tests can't interfere.
        let args: Vec<String> = vec!["taketwo-engine".into(), "--live".into()];
        assert!(args.iter().any(|a| a == "--live"));
        assert_eq!(Mode::resolve(None, args.iter().any(|a| a == "--live")).unwrap(), Mode::Live);
    }

    #[test]
    fn isolated_mode_refuses_franz_ports_at_the_boundaries() {
        let tmp = tempfile::tempdir().unwrap();
        let mode = isolated(tmp.path());
        assert!(mode.check_port(8764).is_ok());
        assert!(mode.check_port(8765).is_err());
        assert!(mode.check_port(8766).is_err());
        assert!(mode.check_port(8785).is_err());
        assert!(mode.check_port(8786).is_ok());
        assert!(mode.check_port(8790).is_ok());
        assert!(Mode::Live.check_port(8766).is_ok());
    }

    #[test]
    fn default_ports_are_dev_ports_when_isolated() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(isolated(tmp.path()).default_ports(), vec![8790, 8791, 8792, 8793, 8794, 8795, 8798, 8799]);
        let live = Mode::Live.default_ports();
        assert_eq!((live[0], *live.last().unwrap(), live.len()), (8766, 8785, 20));
    }

    #[test]
    fn address_file_is_written_only_in_live_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("app-data");
        let mode = isolated(&tmp.path().join("data"));
        let files = mode.address_files(&root);
        assert_eq!(publish_address(&mode, &files, "127.0.0.1", 8766, 42, 1.5), 0);
        assert!(!files[0].exists());
        // Even handed live paths, isolated mode writes nothing.
        let live_files = Mode::Live.address_files(&root);
        assert_eq!(publish_address(&mode, &live_files, "127.0.0.1", 8766, 42, 1.5), 0);
        assert!(!root.exists());
    }

    #[test]
    fn live_address_file_goes_under_both_names_only_for_ports_8766_to_8785() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let files = Mode::Live.address_files(root);
        assert_eq!(files, vec![root.join("TakeTwo/engine.json"), root.join("VoiceForge/engine.json")]);
        for port in [8765, 8786, 8790] {
            assert_eq!(publish_address(&Mode::Live, &files, "127.0.0.1", port, 42, 1.5), 0, "port {port}");
        }
        assert!(!files[0].exists() && !files[1].exists());
        assert_eq!(publish_address(&Mode::Live, &files, "127.0.0.1", 8785, 42, 1.5), 2);
        assert_eq!(publish_address(&Mode::Live, &files, "127.0.0.1", 8766, 4242, 1790000000.25), 2);
        let want = r#"{"url": "http://127.0.0.1:8766", "pid": 4242, "started": 1790000000.25}"#;
        for f in &files {
            assert_eq!(std::fs::read_to_string(f).unwrap(), want);
        }
    }

    #[test]
    fn address_json_matches_python_float_repr() {
        assert_eq!(
            address_json("127.0.0.1", 8770, 1, 1790000000.0),
            r#"{"url": "http://127.0.0.1:8770", "pid": 1, "started": 1790000000.0}"#
        );
    }

    #[test]
    fn live_library_location_is_computed_not_created() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("app-data");
        assert_eq!(live_library_dir(&root), root.join("TakeTwo").join("library"));
        assert_eq!(Mode::Live.library_dir(&root), root.join("TakeTwo").join("library"));
        assert!(Mode::Live.library_copy_enabled());
        assert!(!root.exists());
    }

    #[test]
    fn second_lock_on_the_same_folder_fails_and_frees_on_drop() {
        let tmp = tempfile::tempdir().unwrap();
        let path = isolated(tmp.path()).lock_file(tmp.path());
        let first = EngineLock::acquire(&path).unwrap();
        let err = EngineLock::acquire(&path).unwrap_err();
        assert!(err.contains("another TakeTwo engine"), "{err}");
        drop(first);
        // Read only after the lock is gone: Windows blocks reads of an exclusively locked file.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), std::process::id().to_string());
        EngineLock::acquire(&path).unwrap();
    }

    #[test]
    fn locks_in_different_folders_do_not_clash() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let _la = EngineLock::acquire(&a.path().join(LOCK_FILE)).unwrap();
        let _lb = EngineLock::acquire(&b.path().join(LOCK_FILE)).unwrap();
    }

    // The stale-lock case lives in tests/stale_lock.rs: it spawns a child process, and a fork from this
    // binary could briefly share another test's lock descriptor.

    #[test]
    fn max_takes_is_a_positive_whole_number() {
        assert_eq!(parse_max_takes(None).unwrap(), None);
        assert_eq!(parse_max_takes(Some(OsStr::new(""))).unwrap(), None);
        assert_eq!(parse_max_takes(Some(OsStr::new("20"))).unwrap(), Some(20));
        for bad in ["0", "-1", "abc", "2.5", "20 takes"] {
            assert!(parse_max_takes(Some(&OsString::from(bad))).is_err(), "{bad}");
        }
    }
}
