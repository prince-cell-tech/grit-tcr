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

// Snapshot of the working tree state (empty string = clean). None when the
// directory isn't a git repo.
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
