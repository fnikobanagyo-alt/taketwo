//! A lock left by an engine that died is taken over. A real child process takes the lock and exits
//! without releasing it; the OS frees it, and the next acquire succeeds.

use std::process::Command;
use taketwo_core::paths::{EngineLock, LOCK_FILE};

const HOLD_ENV: &str = "TAKETWO_TEST_HOLD_LOCK";

/// Runs only as the child: take the lock, then exit while still holding it.
#[test]
#[ignore = "helper run by stale_lock_from_a_dead_process_is_taken_over"]
fn helper_take_lock_and_die() {
    let Some(path) = std::env::var_os(HOLD_ENV) else { return };
    let lock = EngineLock::acquire(path.as_ref()).unwrap();
    std::mem::forget(lock);
    std::process::exit(0);
}

#[test]
fn stale_lock_from_a_dead_process_is_taken_over() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join(LOCK_FILE);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "helper_take_lock_and_die", "--ignored", "--test-threads=1"])
        .env(HOLD_ENV, &path)
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let child_pid = child.id();
    assert!(child.wait().unwrap().success());
    // The dead child really held it: its pid is in the file.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), child_pid.to_string());

    let lock = EngineLock::acquire(&path).unwrap();
    drop(lock); // Windows blocks reads of an exclusively locked file
    assert_eq!(std::fs::read_to_string(&path).unwrap(), std::process::id().to_string());
}
