// ─── Git / TCR ───────────────────────────────────────────────────────────────

use crate::storage::{CONFIG_FILE, PROJECTS_DIR, TASKS_FILE};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const PATCHES_DIR: &str = ".grit/patches";

pub fn run_test_command(command: &str) -> bool {
    let command = command.trim();
    if command.is_empty() { return false; }
    // Run through a shell so test commands can use `&&`, pipes, quotes, etc.
    #[cfg(unix)]
    let status = Command::new("sh").arg("-c").arg(command).status();
    #[cfg(windows)]
    let status = Command::new("cmd").args(["/C"]).arg(command).status();
    status.map(|s| s.success()).unwrap_or(false)
}

// Captured-output variant for auto-TCR: the UI stays up, so output can't be
// dumped to the screen — it's collected and shown in a panel instead.
pub fn run_test_command_captured(command: &str) -> (bool, String) {
    let command = command.trim();
    if command.is_empty() { return (false, String::new()); }
    #[cfg(unix)]
    let out = Command::new("sh").arg("-c").arg(command).output();
    #[cfg(windows)]
    let out = Command::new("cmd").args(["/C"]).arg(command).output();
    match out {
        Ok(o) => {
            let text = format!("{}{}",
                String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
            (o.status.success(), text)
        }
        Err(e) => (false, format!("failed to run test command: {e}")),
    }
}

// Snapshot of the working tree state (empty string = clean). Outside a git
// repo git prints nothing, so the signature is also empty — callers must not
// read that as "changed".
pub fn working_tree_signature() -> Option<String> {
    Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
}

// Revert all tracked working-tree changes (same as the manual `t` failure path).
pub fn revert_worktree() {
    let _ = Command::new("git").args(["restore", "."]).status();
}

// Captured diff of tracked working-tree changes (untracked files are left
// alone by the revert too, so nothing user-created is ever destroyed).
pub fn git_diff_all() -> String {
    Command::new("git")
        .args(["diff"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

// Save a diff as a recoverable patch; the name is unique even within one second.
pub fn save_patch(diff: &str) -> io::Result<PathBuf> {
    std::fs::create_dir_all(PATCHES_DIR)?;
    let ts = crate::task::current_datetime_compact();
    let mut path = Path::new(PATCHES_DIR).join(format!("{ts}.patch"));
    let mut n = 0;
    while path.exists() {
        n += 1;
        path = Path::new(PATCHES_DIR).join(format!("{ts}-{n}.patch"));
    }
    std::fs::write(&path, diff)?;
    Ok(path)
}

// Saved patch filenames, newest first.
pub fn list_patches() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(PATCHES_DIR)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map(|x| x == "patch").unwrap_or(false))
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names.reverse();
    names
}

pub fn apply_patch(name: &str) -> Result<(), String> {
    let status = Command::new("git")
        .args(["apply", &format!("{PATCHES_DIR}/{name}")])
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("git apply failed on {name} — possible conflict with current code"))
    }
}

pub fn delete_patch(name: &str) -> io::Result<()> {
    std::fs::remove_file(Path::new(PATCHES_DIR).join(name))
}

pub fn commit_tasks(message: &str) -> Result<(), String> {
    let branch = git_branch();
    if branch == "main" || branch == "master" {
        return Err(format!(
            "Refusing to commit: currently on '{}'. Switch to a feature branch first.",
            branch
        ));
    }
    let add = Command::new("git").args(["add", "-A"]).status().map_err(|e| e.to_string())?;
    if !add.success() { return Err("git add failed".to_string()); }
    // Keep the task-manager's own files out of commits — only code changes belong in them.
    let mut reset_args: Vec<&str> = vec!["reset", "-q", "--"];
    for p in [TASKS_FILE, "tasks.json", CONFIG_FILE, PROJECTS_DIR, ".grit"] {
        if std::path::Path::new(p).exists() {
            reset_args.push(p);
        }
    }
    if reset_args.len() > 3 {
        let _ = Command::new("git").args(&reset_args).status();
    }
    let commit = Command::new("git").args(["commit", "-m", message]).status().map_err(|e| e.to_string())?;
    if !commit.success() { return Err("git commit failed".to_string()); }
    Ok(())
}

pub fn git_branch() -> String {
    Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "?".to_string())
}

pub fn git_log(n: usize) -> Vec<String> {
    Command::new("git")
        .args(["log", "--oneline", &format!("-{n}")])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.to_string())
                .collect()
        })
        .unwrap_or_default()
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestEnv;

    fn init_repo(branch: &str) {
        for args in [
            vec!["init", "-q", "-b", branch],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
        ] {
            let st = Command::new("git").args(&args).status().expect("git must be available");
            assert!(st.success(), "git {args:?} failed");
        }
    }

    fn commit_file(name: &str, content: &str, msg: &str) {
        std::fs::write(name, content).unwrap();
        assert!(Command::new("git").args(["add", name]).status().unwrap().success());
        assert!(Command::new("git")
            .args(["commit", "-q", "-m", msg])
            .status()
            .unwrap()
            .success());
    }

    fn patch_name(path: &Path) -> String {
        path.file_name().unwrap().to_string_lossy().to_string()
    }

    #[cfg(unix)]
    #[test]
    fn empty_test_command_is_neither_pass_nor_fail_with_output() {
        let _env = TestEnv::new("grit-test-git-cmd");
        assert!(!run_test_command("   "));
        assert_eq!(run_test_command_captured(""), (false, String::new()));
    }

    #[cfg(unix)]
    #[test]
    fn nonexistent_test_command_reports_failure_with_output() {
        let _env = TestEnv::new("grit-test-git-cmd");
        assert!(!run_test_command("definitely-not-a-command-xyz"));
        let (passed, out) = run_test_command_captured("definitely-not-a-command-xyz");
        assert!(!passed);
        assert!(
            out.contains("not found") || out.contains("failed to run"),
            "expected shell error text, got: {out:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn captured_command_collects_stdout_and_stderr() {
        let _env = TestEnv::new("grit-test-git-cmd");
        let (passed, out) = run_test_command_captured("echo out; echo err >&2");
        assert!(passed);
        assert!(out.contains("out") && out.contains("err"), "got: {out:?}");
    }

    #[test]
    fn working_tree_signature_tracks_clean_and_dirty_states() {
        let _env = TestEnv::new("grit-test-git-sig");

        // Outside a repo git reports nothing, so the signature is empty (not
        // "changed") — auto-TCR must not fire there.
        assert_eq!(working_tree_signature().as_deref(), Some(""));

        init_repo("feature");
        assert_eq!(working_tree_signature().as_deref(), Some(""), "fresh repo is clean");

        std::fs::write("dirty.txt", "x").unwrap();
        let sig = working_tree_signature().expect("signature");
        assert!(sig.contains("dirty.txt"), "dirty tree: {sig:?}");
    }

    #[test]
    fn save_patch_never_overwrites_an_existing_patch() {
        let _env = TestEnv::new("grit-test-git-save");

        let a = save_patch("diff a").unwrap();
        let b = save_patch("diff b").unwrap();

        assert_ne!(a, b, "same-second patches must get distinct names");
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "diff a");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "diff b");
    }

    #[test]
    fn list_patches_returns_only_patch_files_newest_first() {
        let _env = TestEnv::new("grit-test-git-list");
        std::fs::create_dir_all(PATCHES_DIR).unwrap();
        for n in ["20260101000000.patch", "20260103000000.patch", "20260102000000.patch"] {
            std::fs::write(Path::new(PATCHES_DIR).join(n), "x").unwrap();
        }
        std::fs::write(Path::new(PATCHES_DIR).join("README.txt"), "x").unwrap();

        assert_eq!(
            list_patches(),
            vec![
                "20260103000000.patch".to_string(),
                "20260102000000.patch".to_string(),
                "20260101000000.patch".to_string(),
            ]
        );
    }

    #[test]
    fn list_patches_without_a_directory_is_empty() {
        let _env = TestEnv::new("grit-test-git-list-none");
        assert!(list_patches().is_empty());
    }

    #[test]
    fn apply_patch_succeeds_on_clean_context_and_errors_on_conflict() {
        let _env = TestEnv::new("grit-test-git-apply");
        init_repo("feature");
        commit_file("code.txt", "one\n", "init");

        // Capture the one→two change as a patch, then restore the old content.
        std::fs::write("code.txt", "two\n").unwrap();
        let patch = save_patch(&git_diff_all()).unwrap();
        let name = patch_name(&patch);
        revert_worktree();
        assert_eq!(std::fs::read_to_string("code.txt").unwrap(), "one\n");

        // Clean context: applies.
        apply_patch(&name).expect("patch should apply to the original content");
        assert_eq!(std::fs::read_to_string("code.txt").unwrap(), "two\n");

        // Conflicting context: refuses with a useful message.
        revert_worktree();
        std::fs::write("code.txt", "three\n").unwrap();
        let err = apply_patch(&name).unwrap_err();
        assert!(err.contains("possible conflict"), "got: {err}");
    }

    #[test]
    fn delete_patch_on_a_missing_file_is_an_error() {
        let _env = TestEnv::new("grit-test-git-del");
        assert!(delete_patch("nope.patch").is_err());
    }

    #[test]
    fn git_diff_all_includes_tracked_changes_only() {
        let _env = TestEnv::new("grit-test-git-diff");
        init_repo("feature");
        commit_file("code.txt", "one\n", "init");

        std::fs::write("code.txt", "two\n").unwrap();
        std::fs::write("untracked.txt", "new\n").unwrap();

        let diff = git_diff_all();
        assert!(diff.contains("code.txt"), "diff: {diff}");
        assert!(
            !diff.contains("untracked.txt"),
            "untracked files are deliberately excluded: {diff}"
        );
    }

    #[test]
    fn revert_worktree_restores_tracked_changes_and_keeps_untracked_files() {
        let _env = TestEnv::new("grit-test-git-revert");
        init_repo("feature");
        commit_file("code.txt", "one\n", "init");
        std::fs::write("code.txt", "two\n").unwrap();
        std::fs::write("untracked.txt", "keep\n").unwrap();

        revert_worktree();

        assert_eq!(std::fs::read_to_string("code.txt").unwrap(), "one\n");
        assert_eq!(std::fs::read_to_string("untracked.txt").unwrap(), "keep\n");
    }

    #[test]
    fn commit_tasks_refuses_main_and_master() {
        let _env = TestEnv::new("grit-test-git-main");
        init_repo("main");
        commit_file("code.txt", "one\n", "init");

        let err = commit_tasks("TCR: test").unwrap_err();
        assert!(err.contains("Refusing to commit"), "got: {err}");
        assert!(err.contains("main"), "got: {err}");
    }

    #[test]
    fn commit_tasks_commits_code_but_excludes_task_manager_files() {
        let _env = TestEnv::new("grit-test-git-commit");
        init_repo("feature");
        commit_file("code.txt", "one\n", "init");

        std::fs::write("code.txt", "two\n").unwrap();
        std::fs::write(TASKS_FILE, "- [ ] task\n").unwrap();
        std::fs::write(CONFIG_FILE, "{}").unwrap();
        std::fs::create_dir_all(PROJECTS_DIR).unwrap();
        std::fs::write(Path::new(PROJECTS_DIR).join("work.md"), "- [ ] p\n").unwrap();

        commit_tasks("TCR: test").unwrap();

        let out = Command::new("git")
            .args(["show", "--name-only", "--pretty=format:", "HEAD"])
            .output()
            .unwrap();
        let files = String::from_utf8_lossy(&out.stdout);
        assert!(files.contains("code.txt"), "committed files: {files}");
        assert!(!files.contains(TASKS_FILE), "committed files: {files}");
        assert!(!files.contains(CONFIG_FILE), "committed files: {files}");
        assert!(!files.contains(PROJECTS_DIR), "committed files: {files}");
    }

    #[test]
    fn git_log_is_newest_first_and_respects_the_limit() {
        let _env = TestEnv::new("grit-test-git-log");
        init_repo("feature");
        commit_file("a.txt", "1\n", "first");
        commit_file("b.txt", "2\n", "second");

        let one = git_log(1);
        assert_eq!(one.len(), 1);
        assert!(one[0].contains("second"), "got: {one:?}");
        assert_eq!(git_log(5).len(), 2);
    }

    #[test]
    fn git_log_outside_a_repo_is_empty() {
        let _env = TestEnv::new("grit-test-git-log-none");
        assert!(git_log(5).is_empty());
    }
}
