// ─── File / storage ──────────────────────────────────────────────────────────

use crate::task::{parse_due_date, parse_priority, parse_tags, Priority, Status, SubStatus, Subtask, Task};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fs, io};

pub const TASKS_FILE: &str = "tasks.md";
pub const PROJECTS_DIR: &str = "projects";
pub const CONFIG_FILE: &str = ".grit.json";

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct GritConfig {
    #[serde(default)]
    pub test_command: String,
}

// Load .grit.json from the repo root. Missing or corrupt file → defaults.
pub fn load_config() -> GritConfig {
    load_config_from(CONFIG_FILE)
}

pub fn load_config_from(path: &str) -> GritConfig {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_config(cfg: &GritConfig) -> io::Result<()> {
    save_config_to(cfg, CONFIG_FILE)
}

pub fn save_config_to(cfg: &GritConfig, path: &str) -> io::Result<()> {
    fs::write(path, serde_json::to_string_pretty(cfg)?)
}

pub fn load_tasks() -> Vec<Task> {
    load_tasks_from(TASKS_FILE)
}

// Leading whitespace width: spaces count 1, a tab counts 2.
fn line_indent(line: &str) -> usize {
    let mut n = 0;
    for c in line.chars() {
        match c {
            ' ' => n += 1,
            '\t' => n += 2,
            _ => break,
        }
    }
    n
}

// Parse the content of an indented "- ..." line into (status, description).
fn parse_subtask_line(trimmed: &str) -> (SubStatus, String) {
    if trimmed.starts_with("- [x]") && trimmed.len() > 6 {
        (SubStatus::Done, trimmed[6..].trim().to_string())
    } else if trimmed.starts_with("- [") && trimmed.len() > 6 {
        // "- [ ] " or any other marker → pending
        (SubStatus::Pending, trimmed[6..].trim().to_string())
    } else if trimmed.starts_with("- ") {
        // plain "- desc" with no marker → pending
        (SubStatus::Pending, trimmed[2..].trim().to_string())
    } else {
        (SubStatus::Pending, String::new())
    }
}

// Attach a new subtask under `parent_path` (empty = direct child of the task)
// and return its full path. Falls back to the task root if the path is somehow
// inconsistent — paths only ever refer to subtasks added during this parse.
fn push_subtask(task: &mut Task, parent_path: &[usize], status: SubStatus, desc: String) -> Vec<usize> {
    let new_sub = Subtask { description: desc, status, subtasks: vec![] };
    let mut path = parent_path.to_vec();
    match crate::task::subtask_mut_at(task, &path) {
        Some(parent) => {
            parent.subtasks.push(new_sub);
            path.push(parent.subtasks.len() - 1);
        }
        None => {
            task.subtasks.push(new_sub);
            path = vec![task.subtasks.len() - 1];
        }
    }
    path
}

pub fn load_tasks_from(path: &str) -> Vec<Task> {
    let content = fs::read_to_string(path).unwrap_or_default();
    let mut tasks: Vec<Task> = vec![];
    let mut current_task: Option<Task> = None;
    // Path of the most recently seen subtask at each nesting level:
    // stack[0] = last level-0 subtask, stack[1] = last level-1 subtask, ...
    let mut stack: Vec<Vec<usize>> = vec![];

    for line in content.lines() {
        // Use the raw line for indent detection so leading spaces are preserved
        let indent = line_indent(line);
        let trimmed = line.trim();

        if indent == 0 && trimmed.starts_with("- [") {
            // Top-level task
            if let Some(t) = current_task.take() {
                tasks.push(t);
            }
            stack.clear();
            let status = if trimmed.starts_with("- [x]") {
                Status::Done
            } else if trimmed.starts_with("- [~]") {
                Status::Working
            } else {
                Status::Pending
            };
            // "- [X] " is exactly 6 chars
            let raw_desc = if trimmed.len() > 6 { trimmed[6..].trim() } else { "" };
            let (desc, tags) = parse_tags(raw_desc);
            let (desc, due) = parse_due_date(&desc);
            let (desc, priority) = parse_priority(&desc);
            current_task = Some(Task {
                description: desc.trim().to_string(),
                status,
                priority,
                tags,
                due_date: due,
                created_at: String::new(),
                subtasks: vec![],
            });
        } else if indent > 0 && trimmed.starts_with("- ") {
            // Subtask line at nesting level indent/2 - 1 (indent 2 → level 0).
            // Deeper indents attach under the last subtask of that level.
            if let Some(ref mut t) = current_task {
                let (status, sub_desc) = parse_subtask_line(trimmed);
                if !sub_desc.is_empty() {
                    let level = ((indent / 2).saturating_sub(1)).min(stack.len());
                    let parent_path: Vec<usize> = if level == 0 {
                        vec![]
                    } else {
                        stack[level - 1].clone()
                    };
                    let new_path = push_subtask(t, &parent_path, status, sub_desc);
                    stack.truncate(level);
                    stack.push(new_path);
                }
            }
        } else if trimmed.starts_with("created:") {
            if let Some(ref mut t) = current_task {
                t.created_at = trimmed[8..].trim().to_string();
            }
        }
    }
    if let Some(t) = current_task {
        tasks.push(t);
    }
    tasks
}

pub fn save_tasks_to(tasks: &[Task], path: &str) -> io::Result<()> {
    // Don't create the file if it doesn't exist yet and there's nothing to save
    if tasks.is_empty() && !std::path::Path::new(path).exists() {
        return Ok(());
    }
    let mut content = String::from("# 📋 Task List\n\n");

    let groups = [
        ("## 🚧 Working", Status::Working, "~"),
        ("## 📋 Pending", Status::Pending, " "),
        ("## ✅ Done", Status::Done, "x"),
    ];

    for (header, status, marker) in &groups {
        let group: Vec<_> = tasks.iter().filter(|t| &t.status == status).collect();
        if group.is_empty() { continue; }
        content.push_str(header);
        content.push('\n');
        for task in group {
            let tags_str = task.tags.iter().map(|t| format!(" #{t}")).collect::<String>();
            let due_str = task.due_date.as_deref().map(|d| format!(" due:{d}")).unwrap_or_default();
            let pri_str = match task.priority {
                Priority::High => " p:high",
                Priority::Medium => "",
                Priority::Low => " p:low",
            };
            content.push_str(&format!(
                "- [{}] {}{}{}{}\n",
                marker, task.description, tags_str, due_str, pri_str
            ));
            if !task.created_at.is_empty() {
                content.push_str(&format!("  created: {}\n", task.created_at));
            }
            write_subtasks(&mut content, &task.subtasks, 2);
        }
        content.push('\n');
    }

    fs::write(path, content)?;
    Ok(())
}

// Write a subtask tree: each level is indented 2 more spaces than its parent.
fn write_subtasks(content: &mut String, subs: &[Subtask], indent: usize) {
    for sub in subs {
        let marker = if sub.status == SubStatus::Done { "x" } else { " " };
        content.push_str(&format!("{}- [{}] {}\n", " ".repeat(indent), marker, sub.description));
        write_subtasks(content, &sub.subtasks, indent + 2);
    }
}

pub fn export_to_json(tasks: &[Task], path: &str) -> io::Result<()> {
    let json = serde_json::to_string_pretty(tasks)
        .map_err(io::Error::other)?;
    fs::write(path, json)?;
    Ok(())
}

// Parse a tasks.json file (the same format export_to_json writes).
// Returns an error message suitable for display in the status bar.
pub fn import_from_json(path: &str) -> Result<Vec<Task>, String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("Cannot read {path}: {e}"))?;
    let value: Value = serde_json::from_str(&content)
        .map_err(|e| format!("Invalid JSON: {e}"))?;
    let items = match value {
        Value::Array(items) => items,
        _ => return Err("Expected a JSON array of tasks".to_string()),
    };
    let mut tasks = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let task: Task = serde_json::from_value(item.clone())
            .map_err(|e| format!("Task {}: {}", i + 1, e))?;
        tasks.push(task);
    }
    Ok(tasks)
}

pub fn list_projects() -> Vec<String> {
    fs::read_dir(PROJECTS_DIR)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map(|x| x == "md").unwrap_or(false))
                .map(|e| e.file_name().to_string_lossy().trim_end_matches(".md").to_string())
                .collect()
        })
        .unwrap_or_default()
}

// Reject anything that could escape the projects/ directory or break the filename.
pub fn is_valid_project_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('/')
        && !name.contains('\\')
        && name != "."
        && name != ".."
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::Status;

    #[test]
    fn project_name_validation() {
        assert!(is_valid_project_name("work"));
        assert!(is_valid_project_name("side-project"));
        assert!(!is_valid_project_name(""));
        assert!(!is_valid_project_name("."));
        assert!(!is_valid_project_name(".."));
        assert!(!is_valid_project_name("a/b"));
        assert!(!is_valid_project_name("../escape"));
        assert!(!is_valid_project_name("a\\b"));
    }

    #[test]
    fn roundtrip_save_and_load() {
        let mut tasks = vec![];
        let mut t = Task::new("write tests #meta p:high".to_string()).unwrap();
        t.status = Status::Working;
        t.created_at = "2026-01-01".to_string();
        t.subtasks.push(Subtask::new("unit tests".to_string()).unwrap());
        t.subtasks[0].status = SubStatus::Done;
        t.subtasks.push(Subtask::new("integration tests".to_string()).unwrap());
        tasks.push(t);

        let dir = std::env::temp_dir().join("grit-test-roundtrip");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tasks.md");
        save_tasks_to(&tasks, path.to_str().unwrap()).unwrap();

        let loaded = load_tasks_from(path.to_str().unwrap());
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].description, "write tests");
        assert_eq!(loaded[0].status, Status::Working);
        assert_eq!(loaded[0].priority, Priority::High);
        assert_eq!(loaded[0].tags, vec!["meta"]);
        assert_eq!(loaded[0].created_at, "2026-01-01");
        assert_eq!(loaded[0].subtasks.len(), 2);
        assert_eq!(loaded[0].subtasks[0].status, SubStatus::Done);
        assert_eq!(loaded[0].subtasks[1].status, SubStatus::Pending);
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn load_missing_file_returns_empty() {
        assert!(load_tasks_from("/nonexistent/grit-tasks.md").is_empty());
    }

    #[test]
    fn load_empty_and_malformed_markdown() {
        let dir = std::env::temp_dir().join("grit-test-malformed");
        fs::create_dir_all(&dir).unwrap();

        // Empty file
        let path = dir.join("empty.md");
        fs::write(&path, "").unwrap();
        assert!(load_tasks_from(path.to_str().unwrap()).is_empty());

        // Garbage lines are ignored
        let path = dir.join("garbage.md");
        fs::write(&path, "hello world\n## not a task\n").unwrap();
        assert!(load_tasks_from(path.to_str().unwrap()).is_empty());

        // Task with no created: line keeps an empty created_at
        let path = dir.join("nocreated.md");
        fs::write(&path, "- [ ] plain task\n").unwrap();
        let loaded = load_tasks_from(path.to_str().unwrap());
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].created_at, "");
        assert_eq!(loaded[0].status, Status::Pending);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn markdown_metadata_roundtrip() {
        let dir = std::env::temp_dir().join("grit-test-meta");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tasks.md");
        fs::write(&path, "- [~] fix bug #bug #auth due:2030-01-01 p:low\n  created: 2025-01-01\n  - [x] step one\n  - plain step\n").unwrap();
        let loaded = load_tasks_from(path.to_str().unwrap());
        assert_eq!(loaded.len(), 1);
        let t = &loaded[0];
        assert_eq!(t.status, Status::Working);
        assert_eq!(t.tags, vec!["bug", "auth"]);
        assert_eq!(t.due_date.as_deref(), Some("2030-01-01"));
        assert_eq!(t.priority, Priority::Low);
        assert_eq!(t.created_at, "2025-01-01");
        assert_eq!(t.subtasks.len(), 2);
        assert_eq!(t.subtasks[0].status, SubStatus::Done);
        assert_eq!(t.subtasks[1].status, SubStatus::Pending);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn nested_subtask_markdown_roundtrip() {
        let dir = std::env::temp_dir().join("grit-test-nested");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tasks.md");

        let mut t = Task::new("main task".to_string()).unwrap();
        let mut child = Subtask::new("child".to_string()).unwrap();
        let mut grand = Subtask::new("grand".to_string()).unwrap();
        grand.status = SubStatus::Done;
        let mut grand2 = Subtask::new("grand2".to_string()).unwrap();
        grand2.subtasks.push(Subtask::new("great-grand".to_string()).unwrap());
        child.subtasks = vec![grand, grand2];
        t.subtasks.push(child);

        save_tasks_to(&[t], path.to_str().unwrap()).unwrap();

        let loaded = load_tasks_from(path.to_str().unwrap());
        assert_eq!(loaded.len(), 1);
        let t2 = &loaded[0];
        assert_eq!(t2.description, "main task");
        assert_eq!(t2.subtasks.len(), 1);
        let c2 = &t2.subtasks[0];
        assert_eq!(c2.description, "child");
        assert_eq!(c2.subtasks.len(), 2);
        assert_eq!(c2.subtasks[0].description, "grand");
        assert_eq!(c2.subtasks[0].status, SubStatus::Done);
        assert_eq!(c2.subtasks[0].subtasks.len(), 0);
        assert_eq!(c2.subtasks[1].description, "grand2");
        assert_eq!(c2.subtasks[1].subtasks.len(), 1);
        assert_eq!(c2.subtasks[1].subtasks[0].description, "great-grand");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn nested_subtask_legacy_flat_file_parses() {
        // A hand-written file in the OLD format (all subtasks at 2-space indent)
        let dir = std::env::temp_dir().join("grit-test-legacy");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tasks.md");
        fs::write(&path, "- [ ] main\n  - [x] first\n  - [ ] second\n").unwrap();
        let loaded = load_tasks_from(path.to_str().unwrap());
        assert_eq!(loaded.len(), 1);
        let t = &loaded[0];
        assert_eq!(t.subtasks.len(), 2);
        assert_eq!(t.subtasks[0].status, SubStatus::Done);
        assert_eq!(t.subtasks[1].status, SubStatus::Pending);
        assert!(t.subtasks[0].subtasks.is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn nested_subtask_skipped_indent_clamps_to_parent() {
        // A file that jumps straight to a deep indent with no parent subtask —
        // the line should attach to the closest available parent (task root).
        let dir = std::env::temp_dir().join("grit-test-skip-indent");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tasks.md");
        fs::write(&path, "- [ ] main\n        - [ ] weird deep line\n  - [ ] normal\n").unwrap();
        let loaded = load_tasks_from(path.to_str().unwrap());
        assert_eq!(loaded.len(), 1);
        let t = &loaded[0];
        assert_eq!(t.subtasks.len(), 2); // clamped to root, then normal sibling
        assert_eq!(t.subtasks[0].description, "weird deep line");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn json_export_import_roundtrip() {
        let mut tasks = vec![];
        let mut t = Task::new("export me #json due:2030-05-05".to_string()).unwrap();
        t.created_at = "2026-01-01".to_string();
        t.subtasks.push(Subtask::new("sub".to_string()).unwrap());
        tasks.push(t);

        let dir = std::env::temp_dir().join("grit-test-json");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tasks.json");
        export_to_json(&tasks, path.to_str().unwrap()).unwrap();

        let imported = import_from_json(path.to_str().unwrap()).unwrap();
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].description, "export me");
        assert_eq!(imported[0].tags, vec!["json"]);
        assert_eq!(imported[0].due_date.as_deref(), Some("2030-05-05"));
        assert_eq!(imported[0].created_at, "2026-01-01");
        assert_eq!(imported[0].subtasks.len(), 1);
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn json_import_error_cases() {
        let dir = std::env::temp_dir().join("grit-test-json-err");
        fs::create_dir_all(&dir).unwrap();

        // Missing file
        assert!(import_from_json(dir.join("nope.json").to_str().unwrap()).is_err());

        // Not an array
        let path = dir.join("object.json");
        fs::write(&path, "{\"description\": \"hi\"}").unwrap();
        assert!(import_from_json(path.to_str().unwrap()).is_err());

        // Array but wrong shape
        let path = dir.join("wrong.json");
        fs::write(&path, "[{\"wrong\": true}]").unwrap();
        assert!(import_from_json(path.to_str().unwrap()).is_err());

        // Broken JSON
        let path = dir.join("broken.json");
        fs::write(&path, "[{").unwrap();
        assert!(import_from_json(path.to_str().unwrap()).is_err());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn config_roundtrip_and_corrupt_tolerance() {
        // Uses absolute paths so the test never touches the process cwd
        // (which would race with the cwd-switching tests in app.rs).
        let dir = std::env::temp_dir().join("grit-test-config");
        fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join(".grit.json");
        let config_str = config_path.to_str().unwrap();

        // Missing file → defaults
        let cfg = load_config_from(config_str);
        assert!(cfg.test_command.is_empty());

        // Save then load
        save_config_to(&GritConfig { test_command: "cargo test".into() }, config_str).unwrap();
        assert!(config_path.exists());
        let cfg = load_config_from(config_str);
        assert_eq!(cfg.test_command, "cargo test");

        // Corrupt file → defaults, no panic
        fs::write(&config_path, "{not json").unwrap();
        let cfg = load_config_from(config_str);
        assert!(cfg.test_command.is_empty());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn save_empty_tasks_does_not_create_file() {
        let dir = std::env::temp_dir().join("grit-test-empty-save");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tasks.md");
        save_tasks_to(&[], path.to_str().unwrap()).unwrap();
        assert!(!path.exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
