// ─── Test helpers ────────────────────────────────────────────────────────────

// Shared helpers for tests that need isolated directories. Only compiled
// for tests — never part of the shipped binary.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

// chdir is process-global, so tests that change the working directory must
// hold this lock for as long as they depend on the cwd.
static DIR_LOCK: Mutex<()> = Mutex::new(());

// Distinguishes TempDirs created within one process.
static COUNTER: AtomicUsize = AtomicUsize::new(0);

pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    // A uniquely named directory under the system temp dir. The name includes
    // the pid and a counter so concurrent test processes can never collide.
    pub fn new(prefix: &str) -> TempDir {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("{}-{}-{}", prefix, std::process::id(), n));
        std::fs::create_dir_all(&path)
            .unwrap_or_else(|e| panic!("failed to create temp dir {}: {e}", path.display()));
        TempDir { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn join(&self, rel: &str) -> PathBuf {
        self.path.join(rel)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // Best effort: a test may already have removed it, and panicking in a
        // Drop during unwinding would abort the whole run.
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

// A TempDir plus exclusive ownership of the process cwd. On Drop the original
// cwd is restored and the directory removed — even when a test panics.
pub struct TestEnv {
    dir: TempDir,
    original: PathBuf,
    _lock: MutexGuard<'static, ()>,
}

impl TestEnv {
    // Locks the cwd, creates a fresh directory and chdirs into it.
    pub fn new(prefix: &str) -> TestEnv {
        // Recover from a previous test panicking while holding the lock,
        // otherwise one failure would poison every later test.
        let lock = DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let original = std::env::current_dir().expect("failed to read cwd");
        let dir = TempDir::new(prefix);
        std::env::set_current_dir(dir.path())
            .unwrap_or_else(|e| panic!("failed to chdir to {}: {e}", dir.path().display()));
        TestEnv { dir, original, _lock: lock }
    }

    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    // The directory the process was in before the switch.
    pub fn original(&self) -> &Path {
        &self.original
    }

    // Restore the original cwd now, without releasing the cwd lock. Tests that
    // assert on the restored state use this to avoid racing other cwd-switching
    // tests; Drop restores it anyway.
    pub fn restore_cwd(&mut self) {
        std::env::set_current_dir(&self.original)
            .unwrap_or_else(|e| panic!("failed to restore cwd to {}: {e}", self.original.display()));
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        // Restore the cwd before the TempDir field is dropped and removes it.
        let _ = std::env::set_current_dir(&self.original);
    }
}

// Poll `cond` until it returns true or `timeout` elapses. Returns whether the
// condition became true. Replaces fixed sleeps around background work.
pub fn wait_until(timeout: std::time::Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if cond() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn temp_dirs_get_unique_paths_that_exist() {
        let a = TempDir::new("grit-test-util");
        let b = TempDir::new("grit-test-util");

        assert_ne!(a.path(), b.path(), "two TempDirs must never share a path");
        assert!(a.path().is_dir());
        assert!(b.path().is_dir());
        assert!(a.join("child.txt").starts_with(a.path()));
    }

    #[test]
    fn temp_dir_is_removed_when_dropped() {
        let path = {
            let dir = TempDir::new("grit-test-util-drop");
            dir.join("file.txt");
            std::fs::write(dir.join("file.txt"), "x").unwrap();
            dir.path().to_path_buf()
        };

        assert!(!path.exists(), "dropping a TempDir must remove its directory");
    }

    #[test]
    fn test_env_changes_cwd_and_restores_it() {
        let mut env = TestEnv::new("grit-test-env");
        let original = env.original().to_path_buf();

        assert_eq!(std::env::current_dir().unwrap(), env.dir());
        assert_ne!(std::env::current_dir().unwrap(), original);

        // Assert while the cwd lock is still held: after Drop another test may
        // legitimately chdir, so a post-drop assertion would race.
        env.restore_cwd();
        assert_eq!(std::env::current_dir().unwrap(), original, "cwd must be restored");
    }

    #[test]
    fn wait_until_returns_true_when_condition_becomes_true() {
        let mut calls = 0;
        let ok = wait_until(Duration::from_millis(500), || {
            calls += 1;
            calls >= 3
        });

        assert!(ok);
        assert_eq!(calls, 3, "must stop polling once the condition holds");
    }

    #[test]
    fn wait_until_gives_up_after_timeout() {
        let ok = wait_until(Duration::from_millis(50), || false);
        assert!(!ok);
    }
}
