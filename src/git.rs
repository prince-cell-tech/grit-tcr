// ─── Git / TCR ───────────────────────────────────────────────────────────────

use crate::storage::{CONFIG_FILE, PROJECTS_DIR, TASKS_FILE};
use std::process::Command;

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
    for p in [TASKS_FILE, "tasks.json", CONFIG_FILE, PROJECTS_DIR] {
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
