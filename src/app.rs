// ─── Undo ─────────────────────────────────────────────────────────────────────

use crate::git::{git_branch, git_log};
use crate::storage::{
    is_valid_project_name, list_projects, load_config, load_tasks, load_tasks_from,
    save_config, save_tasks_to, GritConfig, PROJECTS_DIR, TASKS_FILE,
};
use crate::task::{sorted_indices, Status, Subtask, Task, SortMode};
use std::time::{Duration, Instant};

// Recursive row count of a subtask tree: every subtask is one row.
fn count_subtask_rows(subs: &[Subtask]) -> usize {
    subs.iter().map(|s| 1 + count_subtask_rows(&s.subtasks)).sum()
}

// All nested subtask paths of a tree, depth-first in display order.
fn collect_subtask_paths(subs: &[Subtask], prefix: &[usize], out: &mut Vec<Vec<usize>>) {
    for (i, sub) in subs.iter().enumerate() {
        let mut path = prefix.to_vec();
        path.push(i);
        out.push(path.clone());
        collect_subtask_paths(&sub.subtasks, &path, out);
    }
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub tasks: Vec<Task>,
    pub selected: usize,
    pub focused: FocusedRow,
    pub scroll_offset: usize,
}

#[derive(Debug, PartialEq, Clone)]
pub enum Mode {
    View,
    Input,
    Edit,
    EditSubtask,
    Test,
    Search,
    Subtask,
    CommitMsg,
    Help,
    Projects,
    Import,
    ImportMerge,
}

// Which row the cursor is on — a task header or one of its subtasks
#[derive(Debug, Clone, PartialEq)]
pub enum FocusedRow {
    Task(usize),              // display index into visible_indices()
    Subtask(usize, Vec<usize>), // (display index of parent task, path into nested subtasks)
}

pub struct App {
    pub tasks: Vec<Task>,
    pub selected: usize,        // display index (into visible_indices)
    pub focused: FocusedRow,    // which row is highlighted
    pub scroll_offset: usize,
    pub mode: Mode,
    pub input: String,
    pub test_command: String,
    pub undo_stack: Vec<Snapshot>,
    pub redo_stack: Vec<Snapshot>,
    pub sort_mode: SortMode,
    pub search_query: String,
    pub show_git_log: bool,
    pub current_project: String,   // filename without extension; empty = default
    pub projects: Vec<String>,
    pub commit_msg_buf: String,
    pub status_msg: String,        // ephemeral bottom message
    pub import_path: String,       // staged path for the two-step JSON import prompt
    // Subprocess output is cached so rendering doesn't spawn git commands every frame.
    pub branch_cache: (String, Instant),
    pub git_log_cache: (Vec<String>, Instant),
}

impl App {
    pub fn new() -> Self {
        let tasks = load_tasks();
        App {
            tasks,
            selected: 0,
            focused: FocusedRow::Task(0),
            scroll_offset: 0,
            mode: Mode::View,
            input: String::new(),
            test_command: load_config().test_command,
            undo_stack: vec![],
            redo_stack: vec![],
            sort_mode: SortMode::None,
            search_query: String::new(),
            show_git_log: false,
            current_project: String::new(),
            projects: list_projects(),
            commit_msg_buf: String::new(),
            status_msg: String::new(),
            import_path: String::new(),
            branch_cache: (git_branch(), Instant::now()),
            git_log_cache: (git_log(4), Instant::now()),
        }
    }

    fn snapshot_current(&self) -> Snapshot {
        Snapshot {
            tasks: self.tasks.clone(),
            selected: self.selected,
            focused: self.focused.clone(),
            scroll_offset: self.scroll_offset,
        }
    }

    pub fn push_undo(&mut self) {
        self.undo_stack.push(self.snapshot_current());
        // A new action invalidates anything that could be redone
        self.redo_stack.clear();
        if self.undo_stack.len() > 50 {
            self.undo_stack.remove(0);
        }
    }

    fn restore(&mut self, snap: Snapshot) {
        self.tasks = snap.tasks;
        self.selected = snap.selected;
        self.focused = snap.focused;
        self.scroll_offset = snap.scroll_offset;
        self.save();
    }

    pub fn undo(&mut self) {
        if let Some(snap) = self.undo_stack.pop() {
            // Keep the pre-undo state so it can be redone
            self.redo_stack.push(self.snapshot_current());
            if self.redo_stack.len() > 50 {
                self.redo_stack.remove(0);
            }
            self.restore(snap);
            self.status_msg = "Undone.".to_string();
        } else {
            self.status_msg = "Nothing to undo.".to_string();
        }
    }

    pub fn redo(&mut self) {
        if let Some(snap) = self.redo_stack.pop() {
            self.undo_stack.push(self.snapshot_current());
            if self.undo_stack.len() > 50 {
                self.undo_stack.remove(0);
            }
            self.restore(snap);
            self.status_msg = "Redone.".to_string();
        } else {
            self.status_msg = "Nothing to redo.".to_string();
        }
    }

    pub fn save(&mut self) {
        let path = if self.current_project.is_empty() {
            TASKS_FILE.to_string()
        } else {
            format!("{}/{}.md", PROJECTS_DIR, self.current_project)
        };
        if let Err(e) = save_tasks_to(&self.tasks, &path) {
            self.status_msg = format!("Save failed: {e}");
        }
    }

    pub fn load_project(&mut self, name: &str) {
        let name = name.trim();
        if !name.is_empty() && !is_valid_project_name(name) {
            self.status_msg = format!("Invalid project name: {name}");
            return;
        }
        self.current_project = name.to_string();
        let path = if name.is_empty() {
            TASKS_FILE.to_string()
        } else {
            std::fs::create_dir_all(PROJECTS_DIR).ok(); // only create when actually needed
            format!("{}/{}.md", PROJECTS_DIR, name)
        };
        self.tasks = load_tasks_from(&path);
        self.selected = 0;
        self.focused = FocusedRow::Task(0);
        self.scroll_offset = 0;
    }

    // Path of the JSON file for the active project (used by export and import).
    pub fn json_path(&self) -> String {
        if self.current_project.is_empty() {
            "tasks.json".to_string()
        } else {
            format!("{}/{}.json", PROJECTS_DIR, self.current_project)
        }
    }

    // Import tasks from a JSON file. Append dedupes on (description, created_at);
    // Replace discards the current list. Returns (added, skipped). A single undo
    // snapshot is pushed so `u` reverses the whole import.
    pub fn import_tasks(&mut self, path: &str, replace: bool) -> Result<(usize, usize), String> {
        let imported = crate::storage::import_from_json(path)?;
        let imported_len = imported.len();
        self.push_undo();
        if replace {
            self.tasks = imported;
            self.selected = 0;
            self.focused = FocusedRow::Task(0);
        } else {
            let existing: std::collections::HashSet<(String, String)> = self.tasks.iter()
                .map(|t| (t.description.clone(), t.created_at.clone()))
                .collect();
            let mut added = 0;
            for t in imported {
                let key = (t.description.clone(), t.created_at.clone());
                if !existing.contains(&key) {
                    self.tasks.push(t);
                    added += 1;
                }
            }
            let skipped = imported_len - added;
            self.save();
            return Ok((added, skipped));
        }
        self.save();
        Ok((imported_len, 0))
    }

    // Shared reference to the focused subtask (None when a task is focused)
    pub fn focused_subtask(&self, vis: &[usize]) -> Option<&Subtask> {
        let (di, path) = match &self.focused {
            FocusedRow::Subtask(di, path) => (*di, path.clone()),
            _ => return None,
        };
        let real = *vis.get(di)?;
        crate::task::subtask_at(&self.tasks[real], &path)
    }

    // Mutable reference to the focused subtask (None when a task is focused)
    pub fn focused_subtask_mut(&mut self, vis: &[usize]) -> Option<&mut Subtask> {
        let (di, path) = match &self.focused {
            FocusedRow::Subtask(di, path) => (*di, path.clone()),
            _ => return None,
        };
        let real = vis[di];
        crate::task::subtask_mut_at(&mut self.tasks[real], &path)
    }

    pub fn visible_indices(&self) -> Vec<usize> {        let sorted = sorted_indices(&self.tasks, &self.sort_mode);
        if self.search_query.is_empty() {
            sorted
        } else {
            let q = self.search_query.to_lowercase();
            sorted.into_iter().filter(|&i| {
                let t = &self.tasks[i];
                t.description.to_lowercase().contains(&q)
                    || t.tags.iter().any(|tag| tag.to_lowercase().contains(&q))
            }).collect()
        }
    }

    pub fn clamp_selected(&mut self, vis_len: usize) {
        if vis_len == 0 {
            self.selected = 0;
            self.focused = FocusedRow::Task(0);
        } else if self.selected >= vis_len {
            self.selected = vis_len - 1;
            self.focused = FocusedRow::Task(self.selected);
        }
    }

    // Total number of visible rows including subtasks (recursively)
    pub fn total_rows(&self, vis: &[usize]) -> usize {
        vis.iter().map(|&i| 1 + count_subtask_rows(&self.tasks[i].subtasks)).sum()
    }

    // Convert a flat row index to FocusedRow
    pub fn row_index_to_focused(&self, vis: &[usize], row: usize) -> FocusedRow {
        let mut offset = 0;
        for (di, &ri) in vis.iter().enumerate() {
            if row == offset {
                return FocusedRow::Task(di);
            }
            offset += 1;
            let mut paths: Vec<Vec<usize>> = vec![];
            collect_subtask_paths(&self.tasks[ri].subtasks, &[], &mut paths);
            if let Some(p) = paths.get(row - offset) {
                return FocusedRow::Subtask(di, p.clone());
            }
            offset += paths.len();
        }
        FocusedRow::Task(vis.len().saturating_sub(1))
    }

    // Convert FocusedRow to flat row index
    pub fn focused_to_row_index(&self, vis: &[usize]) -> usize {
        let mut offset = 0;
        for (di, &ri) in vis.iter().enumerate() {
            match &self.focused {
                FocusedRow::Task(t) if *t == di => return offset,
                FocusedRow::Subtask(t, path) if *t == di => {
                    let mut paths: Vec<Vec<usize>> = vec![];
                    collect_subtask_paths(&self.tasks[ri].subtasks, &[], &mut paths);
                    // +1 accounts for the task's own row above its subtasks
                    if let Some(pos) = paths.iter().position(|p| p == path) {
                        return offset + 1 + pos;
                    }
                }
                _ => {}
            }
            offset += 1 + count_subtask_rows(&self.tasks[ri].subtasks);
        }
        0
    }

    pub fn move_down(&mut self, vis: &[usize]) {
        let total = self.total_rows(vis);
        if total == 0 { return; }
        let cur = self.focused_to_row_index(vis);
        if cur + 1 < total {
            self.focused = self.row_index_to_focused(vis, cur + 1);
            // keep selected (task-level) in sync
            if let FocusedRow::Task(di) = self.focused {
                self.selected = di;
            } else if let FocusedRow::Subtask(di, _) = self.focused {
                self.selected = di;
            }
        }
    }

    pub fn move_up(&mut self, vis: &[usize]) {
        let cur = self.focused_to_row_index(vis);
        if cur > 0 {
            self.focused = self.row_index_to_focused(vis, cur - 1);
            if let FocusedRow::Task(di) = self.focused {
                self.selected = di;
            } else if let FocusedRow::Subtask(di, _) = self.focused {
                self.selected = di;
            }
        }
    }

    // Set the test command and persist it to .grit.json so it survives restarts.
    pub fn set_test_command(&mut self, cmd: String) {
        self.test_command = cmd;
        let cfg = GritConfig { test_command: self.test_command.clone() };
        if let Err(e) = save_config(&cfg) {
            self.status_msg = format!("Config save failed: {e}");
        }
    }

    // Jump to the first task row (H key)
    pub fn jump_top(&mut self) {
        self.selected = 0;
        self.focused = FocusedRow::Task(0);
        self.scroll_offset = 0;
    }

    // Jump to the last row of the flattened list, including nested subtasks (G key)
    pub fn jump_bottom(&mut self, vis: &[usize]) {
        let total = self.total_rows(vis);
        if total == 0 { return; }
        self.focused = self.row_index_to_focused(vis, total - 1);
        if let FocusedRow::Task(di) = self.focused {
            self.selected = di;
        } else if let FocusedRow::Subtask(di, _) = self.focused {
            self.selected = di;
        }
    }

    pub fn task_counts(&self) -> (usize, usize, usize) {        let pending = self.tasks.iter().filter(|t| t.status == Status::Pending).count();
        let working = self.tasks.iter().filter(|t| t.status == Status::Working).count();
        let done = self.tasks.iter().filter(|t| t.status == Status::Done).count();
        (pending, working, done)
    }

    // Git output is only refreshed every couple of seconds instead of every frame.
    pub fn cached_branch(&mut self) -> String {
        if self.branch_cache.1.elapsed() >= Duration::from_secs(2) {
            self.branch_cache = (git_branch(), Instant::now());
        }
        self.branch_cache.0.clone()
    }

    pub fn cached_git_log(&mut self) -> Vec<String> {
        if self.git_log_cache.1.elapsed() >= Duration::from_secs(2) {
            self.git_log_cache = (git_log(4), Instant::now());
        }
        self.git_log_cache.0.clone()
    }

    pub fn invalidate_git_caches(&mut self) {
        self.branch_cache = (git_branch(), Instant::now());
        self.git_log_cache = (git_log(4), Instant::now());
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::Subtask;
    use std::sync::{Mutex, MutexGuard};

    // chdir is process-wide and tests run in parallel threads, so every test
    // that switches directories must hold this lock until it is done.
    static DIR_LOCK: Mutex<()> = Mutex::new(());

    // Build an App without touching the real task files: temporarily chdir into
    // an empty temp dir. The returned guard keeps other tests from racing on
    // the process cwd; drop it (or call restore_dir) to restore cwd.
    fn app_in_temp_dir() -> (App, std::path::PathBuf, MutexGuard<'static, ()>) {
        let guard = DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let original = std::env::current_dir().unwrap();
        let dir = std::env::temp_dir().join(format!("grit-test-app-{}", std::process::id()));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).unwrap();
        }
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_current_dir(&dir).unwrap();
        (App::new(), original, guard)
    }

    fn restore_dir(original: &std::path::Path) {
        std::env::set_current_dir(original).unwrap();
    }

    #[test]
    fn undo_roundtrip_restores_tasks_and_cursor() {
        let (mut app, original, _lock) = app_in_temp_dir();
        app.tasks.push(Task::new("first".to_string()).unwrap());
        app.tasks.push(Task::new("second".to_string()).unwrap());
        app.selected = 1;
        app.focused = FocusedRow::Task(1);
        app.scroll_offset = 1;

        app.push_undo();
        app.tasks.clear();
        app.selected = 0;
        app.focused = FocusedRow::Task(0);
        app.scroll_offset = 0;

        app.undo();
        assert_eq!(app.tasks.len(), 2);
        assert_eq!(app.tasks[1].description, "second");
        assert_eq!(app.selected, 1);
        assert_eq!(app.focused, FocusedRow::Task(1));
        assert_eq!(app.scroll_offset, 1);

        // Undo with nothing left on the stack is a no-op message
        app.undo();
        assert_eq!(app.status_msg, "Nothing to undo.");
        restore_dir(&original);
    }

    #[test]
    fn push_undo_caps_stack_at_50() {
        let (mut app, original, _lock) = app_in_temp_dir();
        for i in 0..60 {
            app.push_undo();
            app.tasks.push(Task::new(format!("t{i}")).unwrap());
        }
        assert_eq!(app.undo_stack.len(), 50);
        restore_dir(&original);
    }

    #[test]
    fn test_command_persists_to_config() {
        let (mut app, original, _lock) = app_in_temp_dir();
        // Fresh dir → no config → empty command
        assert!(app.test_command.is_empty());

        app.set_test_command("cargo test".into());
        assert_eq!(app.test_command, "cargo test");

        // A brand-new App picks it up from disk
        let app2 = App::new();
        assert_eq!(app2.test_command, "cargo test");

        // Corrupt config tolerated on load
        std::fs::write(".grit.json", "{oops").unwrap();
        let app3 = App::new();
        assert!(app3.test_command.is_empty());
        restore_dir(&original);
    }

    #[test]
    fn undo_redo_roundtrip() {
        let (mut app, original, _lock) = app_in_temp_dir();
        app.tasks.push(Task::new("first".to_string()).unwrap());

        // Mutate, undo, redo → task comes back
        app.push_undo();
        app.tasks.clear();
        app.undo();
        assert_eq!(app.tasks.len(), 1);
        app.redo();
        assert!(app.tasks.is_empty());

        // Redo again is a no-op message
        app.redo();
        assert_eq!(app.status_msg, "Nothing to redo.");
        restore_dir(&original);
    }

    #[test]
    fn redo_restores_cursor_position() {
        let (mut app, original, _lock) = app_in_temp_dir();
        app.tasks.push(Task::new("a".to_string()).unwrap());
        app.tasks.push(Task::new("b".to_string()).unwrap());

        app.selected = 1;
        app.focused = FocusedRow::Task(1);
        app.push_undo();
        app.tasks.remove(1);
        app.selected = 0;
        app.focused = FocusedRow::Task(0);
        app.undo(); // back to 2 tasks, cursor at index 1
        assert_eq!(app.selected, 1);
        app.redo(); // forward again — cursor returns to the pre-undo position (0)
        assert_eq!(app.selected, 0);
        restore_dir(&original);
    }

    #[test]
    fn new_mutation_invalidates_redo() {
        let (mut app, original, _lock) = app_in_temp_dir();
        app.tasks.push(Task::new("a".to_string()).unwrap());

        app.push_undo();
        app.tasks.clear();
        app.undo();
        assert_eq!(app.tasks.len(), 1);

        // Undo succeeded, but then a fresh mutation clears the redo path
        app.push_undo();
        app.tasks.push(Task::new("b".to_string()).unwrap());
        assert!(app.redo_stack.is_empty());
        app.redo();
        assert_eq!(app.status_msg, "Nothing to redo.");
        assert_eq!(app.tasks.len(), 2); // "b" was not lost
        restore_dir(&original);
    }

    #[test]
    fn jump_top_and_bottom_over_nested_tree() {
        let (mut app, original, _lock) = app_in_temp_dir();
        let mut t1 = Task::new("one".to_string()).unwrap();
        let mut sub = Subtask::new("one-a".to_string()).unwrap();
        sub.subtasks.push(Subtask::new("one-a-1".to_string()).unwrap());
        t1.subtasks = vec![sub, Subtask::new("one-b".to_string()).unwrap()];
        app.tasks = vec![t1, Task::new("two".to_string()).unwrap()];
        let vis = app.visible_indices();

        // H → first task
        app.jump_top();
        assert_eq!(app.focused, FocusedRow::Task(0));
        assert_eq!(app.selected, 0);

        // G → deepest row: 0=task1, 1=[0], 2=[0,0], 3=[1], 4=task2 → row 4
        app.jump_bottom(&vis);
        assert_eq!(app.focused, FocusedRow::Task(1));
        assert_eq!(app.selected, 1);

        // Empty list: G is a no-op, H resets safely
        app.tasks.clear();
        let vis_empty = app.visible_indices();
        app.jump_bottom(&vis_empty);
        app.jump_top();
        assert_eq!(app.selected, 0);
        assert_eq!(app.focused, FocusedRow::Task(0));
        restore_dir(&original);
    }

    #[test]
    fn visible_indices_filters_by_description_and_tag() {
        let (mut app, original, _lock) = app_in_temp_dir();
        app.tasks.push(Task::new("fix login #auth".to_string()).unwrap());
        app.tasks.push(Task::new("write docs #meta".to_string()).unwrap());
        app.tasks.push(Task::new("buy milk".to_string()).unwrap());

        // Case-insensitive description match
        app.search_query = "LOGIN".into();
        assert_eq!(app.visible_indices(), vec![0]);

        // Tag match
        app.search_query = "meta".into();
        assert_eq!(app.visible_indices(), vec![1]);

        // Empty query → all
        app.search_query.clear();
        assert_eq!(app.visible_indices(), vec![0, 1, 2]);
        restore_dir(&original);
    }

    #[test]
    fn row_focus_conversion_roundtrip() {
        let (mut app, original, _lock) = app_in_temp_dir();
        let mut t1 = Task::new("one".to_string()).unwrap();
        let mut sub = Subtask::new("one-a".to_string()).unwrap();
        sub.subtasks.push(Subtask::new("one-a-1".to_string()).unwrap());
        t1.subtasks = vec![sub, Subtask::new("one-b".to_string()).unwrap()];
        app.tasks = vec![t1, Task::new("two".to_string()).unwrap()];
        let vis = app.visible_indices(); // [0, 1]

        // Rows: 0=task1, 1=one-a, 2=one-a-1, 3=one-b, 4=task2
        assert_eq!(app.row_index_to_focused(&vis, 0), FocusedRow::Task(0));
        assert_eq!(app.row_index_to_focused(&vis, 1), FocusedRow::Subtask(0, vec![0]));
        assert_eq!(app.row_index_to_focused(&vis, 2), FocusedRow::Subtask(0, vec![0, 0]));
        assert_eq!(app.row_index_to_focused(&vis, 3), FocusedRow::Subtask(0, vec![1]));
        assert_eq!(app.row_index_to_focused(&vis, 4), FocusedRow::Task(1));
        // Out of range clamps to last task
        assert_eq!(app.row_index_to_focused(&vis, 99), FocusedRow::Task(1));

        for row in 0..5 {
            app.focused = app.row_index_to_focused(&vis, row);
            assert_eq!(app.focused_to_row_index(&vis), row);
        }
        assert_eq!(app.total_rows(&vis), 5);
        restore_dir(&original);
    }

    #[test]
    fn move_up_down_across_subtasks() {
        let (mut app, original, _lock) = app_in_temp_dir();
        let mut t1 = Task::new("one".to_string()).unwrap();
        let mut sub = Subtask::new("one-a".to_string()).unwrap();
        sub.subtasks.push(Subtask::new("one-a-1".to_string()).unwrap());
        t1.subtasks.push(sub);
        app.tasks = vec![t1, Task::new("two".to_string()).unwrap()];
        let vis = app.visible_indices();

        // Rows: 0=task1, 1=one-a, 2=one-a-1, 3=task2
        app.focused = FocusedRow::Task(0);
        app.move_down(&vis);
        assert_eq!(app.focused, FocusedRow::Subtask(0, vec![0]));
        app.move_down(&vis);
        assert_eq!(app.focused, FocusedRow::Subtask(0, vec![0, 0]));
        app.move_down(&vis);
        assert_eq!(app.focused, FocusedRow::Task(1));
        app.move_down(&vis); // at the end, no-op
        assert_eq!(app.focused, FocusedRow::Task(1));
        app.move_up(&vis);
        assert_eq!(app.focused, FocusedRow::Subtask(0, vec![0, 0]));
        app.move_up(&vis);
        assert_eq!(app.focused, FocusedRow::Subtask(0, vec![0]));
        app.move_up(&vis);
        assert_eq!(app.focused, FocusedRow::Task(0));
        app.move_up(&vis); // at the top, no-op
        assert_eq!(app.focused, FocusedRow::Task(0));
        restore_dir(&original);
    }

    #[test]
    fn focused_subtask_helpers_reach_deep_nesting() {
        let (mut app, original, _lock) = app_in_temp_dir();
        let mut t = Task::new("task".to_string()).unwrap();
        let mut sub = Subtask::new("child".to_string()).unwrap();
        sub.subtasks.push(Subtask::new("grand".to_string()).unwrap());
        t.subtasks.push(sub);
        app.tasks = vec![t];
        let vis = app.visible_indices();

        app.focused = FocusedRow::Subtask(0, vec![0, 0]);
        assert_eq!(app.focused_subtask(&vis).unwrap().description, "grand");
        app.focused_subtask_mut(&vis).unwrap().status = crate::task::SubStatus::Done;
        assert_eq!(app.tasks[0].subtasks[0].subtasks[0].status, crate::task::SubStatus::Done);

        // Task focus → helpers return None
        app.focused = FocusedRow::Task(0);
        assert!(app.focused_subtask(&vis).is_none());
        assert!(app.focused_subtask_mut(&vis).is_none());

        // Out-of-range path → None, no panic
        app.focused = FocusedRow::Subtask(0, vec![5]);
        assert!(app.focused_subtask(&vis).is_none());
        restore_dir(&original);
    }

    #[test]
    fn task_counts() {
        let (mut app, original, _lock) = app_in_temp_dir();
        let mut a = Task::new("a".to_string()).unwrap();
        a.status = crate::task::Status::Working;
        let b = Task::new("b".to_string()).unwrap();
        let mut c = Task::new("c".to_string()).unwrap();
        c.status = crate::task::Status::Done;
        let mut d = Task::new("d".to_string()).unwrap();
        d.status = crate::task::Status::Done;
        app.tasks = vec![a, b, c, d];
        assert_eq!(app.task_counts(), (1, 1, 2));
        restore_dir(&original);
    }

    fn write_json_file(name: &str, tasks: &[Task]) -> String {
        let dir = std::env::temp_dir().join("grit-test-import");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        crate::storage::export_to_json(tasks, path.to_str().unwrap()).unwrap();
        path.to_str().unwrap().to_string()
    }

    #[test]
    fn import_append_dedupes_and_returns_counts() {
        let (mut app, original, _lock) = app_in_temp_dir();
        app.tasks.push(Task::new("existing one".to_string()).unwrap());

        // incoming: one duplicate of "existing one", two new tasks
        let mut dup = Task::new("existing one".to_string()).unwrap();
        dup.created_at = app.tasks[0].created_at.clone();
        let incoming = vec![
            dup,
            Task::new("brand new".to_string()).unwrap(),
            Task::new("another new #tag".to_string()).unwrap(),
        ];
        let path = write_json_file("append.json", &incoming);

        let (added, skipped) = app.import_tasks(&path, false).unwrap();
        assert_eq!((added, skipped), (2, 1));
        assert_eq!(app.tasks.len(), 3);
        assert_eq!(app.tasks[1].description, "brand new");
        assert_eq!(app.tasks[2].description, "another new");

        // The whole import is one undo step
        app.undo();
        assert_eq!(app.tasks.len(), 1);
        restore_dir(&original);
    }

    #[test]
    fn import_replace_swaps_the_list() {
        let (mut app, original, _lock) = app_in_temp_dir();
        app.tasks.push(Task::new("old task".to_string()).unwrap());

        let incoming = vec![Task::new("fresh".to_string()).unwrap()];
        let path = write_json_file("replace.json", &incoming);

        let (added, skipped) = app.import_tasks(&path, true).unwrap();
        assert_eq!((added, skipped), (1, 0));
        assert_eq!(app.tasks.len(), 1);
        assert_eq!(app.tasks[0].description, "fresh");

        // Replace is also undoable — the old list comes back
        app.undo();
        assert_eq!(app.tasks.len(), 1);
        assert_eq!(app.tasks[0].description, "old task");
        restore_dir(&original);
    }

    #[test]
    fn import_bad_file_is_an_error_and_keeps_tasks() {
        let (mut app, original, _lock) = app_in_temp_dir();
        app.tasks.push(Task::new("keep me".to_string()).unwrap());

        let err = app.import_tasks("/nonexistent/import.json", false).unwrap_err();
        assert!(err.contains("Cannot read"), "got: {err}");
        assert_eq!(app.tasks.len(), 1);
        // No undo snapshot was pushed on failure
        assert!(app.undo_stack.is_empty());
        restore_dir(&original);
    }
}
