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
    ReloadPrompt,    // task file changed on disk — reload or keep mine?
    Filter,          // structured filter prompt (f)
    CommitTemplate,  // edit commit-message template (m)
    PatchBrowser,    // browse/apply/delete revert patches (r)
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
    // Exact on-disk content as of our last write/load; used to detect
    // external edits of the task file (e.g. in an editor).
    pub last_written: String,
    pub mode: Mode,
    pub input: String,
    pub test_command: String,
    pub commit_template: String,   // placeholders: {task} {status} {date}
    pub auto_tcr: bool,            // run tests+commit/revert automatically on file changes
    pub undo_stack: Vec<Snapshot>,
    pub redo_stack: Vec<Snapshot>,
    pub sort_mode: SortMode,
    pub search_query: String,
    pub filter_query: String,      // structured filter: #tag, s:status, p:priority, words
    pub show_git_log: bool,
    pub current_project: String,   // filename without extension; empty = default
    pub projects: Vec<String>,
    pub commit_msg_buf: String,
    pub status_msg: String,        // ephemeral bottom message
    pub import_path: String,       // staged path for the two-step JSON import prompt
    // Subprocess output is cached so rendering doesn't spawn git commands every frame.
    pub branch_cache: (String, Instant),
    pub git_log_cache: (Vec<String>, Instant),
    // Auto-TCR: background test runs triggered by working-tree changes.
    pub tcr_running: bool,
    pub tcr_rx: Option<std::sync::mpsc::Receiver<(bool, String)>>,
    pub last_tree_sig: Option<String>,   // last seen `git status --porcelain` output
    pub last_poll: Instant,              // throttle for disk/tree polling
    pub tcr_output_tail: Vec<String>,    // tail of failed test output (panel)
    pub show_tcr_output: bool,
    // Patch browser state (revert patches saved by a failed auto-TCR).
    pub patches: Vec<String>,
    pub patch_selected: usize,
}

impl App {
    pub fn new() -> Self {
        let tasks = load_tasks();
        let cfg = load_config();
        App {
            tasks,
            selected: 0,
            focused: FocusedRow::Task(0),
            scroll_offset: 0,
            mode: Mode::View,
            input: String::new(),
            test_command: cfg.test_command,
            commit_template: cfg.commit_template,
            auto_tcr: cfg.auto_tcr,
            undo_stack: vec![],
            redo_stack: vec![],
            sort_mode: SortMode::None,
            search_query: String::new(),
            filter_query: String::new(),
            show_git_log: false,
            current_project: String::new(),
            projects: list_projects(),
            commit_msg_buf: String::new(),
            status_msg: String::new(),
            import_path: String::new(),
            last_written: crate::storage::file_content(TASKS_FILE),
            branch_cache: (git_branch(), Instant::now()),
            git_log_cache: (git_log(4), Instant::now()),
            tcr_running: false,
            tcr_rx: None,
            last_tree_sig: None,
            last_poll: Instant::now(),
            tcr_output_tail: vec![],
            show_tcr_output: false,
            patches: vec![],
            patch_selected: 0,
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

    // Path of the active task file (default tasks.md or projects/<name>.md).
    pub fn tasks_path(&self) -> String {
        if self.current_project.is_empty() {
            TASKS_FILE.to_string()
        } else {
            format!("{}/{}.md", PROJECTS_DIR, self.current_project)
        }
    }

    pub fn save(&mut self) {
        let path = self.tasks_path();
        if let Err(e) = save_tasks_to(&self.tasks, &path) {
            self.status_msg = format!("Save failed: {e}");
        }
        self.last_written = crate::storage::file_content(&path);
    }

    // True when the task file on disk no longer matches what we last wrote.
    pub fn detect_external_edit(&self) -> bool {
        crate::storage::file_content(&self.tasks_path()) != self.last_written
    }

    // Discard the in-memory list and take the on-disk version. An undo
    // snapshot is pushed first so `u` restores the in-memory version.
    pub fn reload_from_disk(&mut self) {
        self.push_undo();
        self.tasks = load_tasks_from(&self.tasks_path());
        self.selected = 0;
        self.focused = FocusedRow::Task(0);
        self.scroll_offset = 0;
        self.last_written = crate::storage::file_content(&self.tasks_path());
    }

    // Overwrite the external edit with our in-memory version.
    pub fn keep_my_version(&mut self) {
        self.save();
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
        self.last_written = crate::storage::file_content(&path);
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
        // Structured filter first (metadata + words), then free-text search,
        // so both compose with AND semantics.
        let filter = crate::task::TaskFilter::parse(&self.filter_query);
        let filtered: Vec<usize> = if filter.is_empty() {
            sorted
        } else {
            sorted.into_iter().filter(|&i| filter.matches(&self.tasks[i])).collect()
        };
        if self.search_query.is_empty() {
            filtered
        } else {
            let q = self.search_query.to_lowercase();
            filtered.into_iter().filter(|&i| {
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

    // Persist every config field at once so saving one never wipes another.
    fn persist_config(&mut self) {
        let cfg = GritConfig {
            test_command: self.test_command.clone(),
            commit_template: self.commit_template.clone(),
            auto_tcr: self.auto_tcr,
        };
        if let Err(e) = save_config(&cfg) {
            self.status_msg = format!("Config save failed: {e}");
        }
    }

    // Set the test command and persist it to .grit.json so it survives restarts.
    pub fn set_test_command(&mut self, cmd: String) {
        self.test_command = cmd;
        self.persist_config();
    }

    // Set the commit-message template and persist it.
    pub fn set_commit_template(&mut self, tpl: String) {
        self.commit_template = tpl;
        self.persist_config();
    }

    // Build the commit message from the template for the focused task.
    // With no focused task there is nothing meaningful to generate, so a
    // fixed fallback is used instead (custom one-off messages go through `c`).
    pub fn build_commit_message(&self, vis: &[usize]) -> String {
        let tpl = if self.commit_template.trim().is_empty() {
            "TCR: {task}"
        } else {
            self.commit_template.as_str()
        };
        let ctx = match &self.focused {
            FocusedRow::Task(di) => vis.get(*di).map(|&real| &self.tasks[real]),
            FocusedRow::Subtask(di, _) => vis.get(*di).map(|&real| &self.tasks[real]),
        };
        let Some(task) = ctx else {
            return "TCR: tests passed".to_string();
        };
        tpl.replace("{task}", &task.description)
            .replace("{status}", task.status.label())
            .replace("{date}", &crate::task::current_date())
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

    // ─── Auto-TCR ────────────────────────────────────────────────────────────

    pub fn toggle_auto_tcr(&mut self) {
        self.auto_tcr = !self.auto_tcr;
        // Re-baseline so toggling on never fires immediately on old dirt.
        self.last_tree_sig = None;
        self.persist_config();
        self.status_msg = if self.auto_tcr {
            "Auto-TCR on: tests run+commit/revert when files change.".to_string()
        } else {
            "Auto-TCR off.".to_string()
        };
    }

    // Called (throttled) from the event loop: watches the working tree and
    // starts a TCR run when files change after saving in the editor.
    pub fn maybe_start_auto_tcr(&mut self) {
        if self.tcr_running { return; }
        let Some(sig) = crate::git::working_tree_signature() else {
            self.last_tree_sig = None; // not a repo (yet)
            return;
        };
        if self.last_tree_sig.as_deref() == Some(sig.as_str()) { return; }
        let is_baseline = self.last_tree_sig.is_none();
        self.last_tree_sig = Some(sig.clone());
        // Baseline poll and a clean tree never trigger a run.
        if is_baseline || sig.trim().is_empty() { return; }
        self.start_tcr_run();
    }

    fn start_tcr_run(&mut self) {
        let cmd = self.test_command.clone();
        if cmd.trim().is_empty() {
            self.status_msg = "Auto-TCR skipped: no test command set (press T).".to_string();
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.tcr_rx = Some(rx);
        self.tcr_running = true;
        self.status_msg = "TCR running…".to_string();
        std::thread::spawn(move || {
            let result = crate::git::run_test_command_captured(&cmd);
            let _ = tx.send(result);
        });
    }

    // Non-blocking collection of a finished background TCR run.
    pub fn poll_tcr_result(&mut self) {
        let Some(rx) = &self.tcr_rx else { return };
        let mut result = None;
        while let Ok(r) = rx.try_recv() { result = Some(r); }
        let Some((passed, output)) = result else { return };
        self.tcr_running = false;
        self.tcr_rx = None;
        if passed {
            self.save();
            let vis = self.visible_indices();
            let msg = self.build_commit_message(&vis);
            match crate::git::commit_tasks(&msg) {
                Ok(_) => {
                    self.invalidate_git_caches();
                    self.status_msg = format!("Auto-TCR: committed \"{msg}\"");
                }
                Err(e) => self.status_msg = format!("Auto-TCR: tests passed, not committed: {e}"),
            }
        } else {
            // Capture the diff BEFORE reverting, so nothing is truly lost.
            let diff = crate::git::git_diff_all();
            let patch = if diff.trim().is_empty() {
                None
            } else {
                crate::git::save_patch(&diff).ok()
            };
            crate::git::revert_worktree();
            self.tcr_output_tail = output.lines()
                .rev().take(15).collect::<Vec<_>>().into_iter().rev()
                .map(|s| s.to_string()).collect();
            self.show_tcr_output = true;
            self.status_msg = match &patch {
                Some(p) => format!("Auto-TCR: tests FAILED — reverted; patch saved: {}", p.display()),
                None => "Auto-TCR: tests FAILED — nothing to revert".to_string(),
            };
        }
        // The tree just changed (commit or revert) — re-baseline so the next
        // poll doesn't mistake our own action for new editor changes.
        self.last_tree_sig = crate::git::working_tree_signature();
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{Priority, Subtask};
    use crate::testutil::TestEnv;

    // Build an App without touching the real task files: each test gets its own
    // temp dir as the process cwd. Dropping the TestEnv restores the cwd and
    // deletes the dir — even if the test panics.
    fn app_in_temp_env() -> (App, TestEnv) {
        let env = TestEnv::new("grit-test-app");
        (App::new(), env)
    }

    #[test]
    fn undo_roundtrip_restores_tasks_and_cursor() {
        let (mut app, _env) = app_in_temp_env();
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
    }

    #[test]
    fn push_undo_caps_stack_at_50() {
        let (mut app, _env) = app_in_temp_env();
        for i in 0..60 {
            app.push_undo();
            app.tasks.push(Task::new(format!("t{i}")).unwrap());
        }
        assert_eq!(app.undo_stack.len(), 50);
    }

    #[test]
    fn test_command_persists_to_config() {
        let (mut app, _env) = app_in_temp_env();
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
    }

    #[test]
    fn undo_redo_roundtrip() {
        let (mut app, _env) = app_in_temp_env();
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
    }

    #[test]
    fn redo_restores_cursor_position() {
        let (mut app, _env) = app_in_temp_env();
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
    }

    #[test]
    fn new_mutation_invalidates_redo() {
        let (mut app, _env) = app_in_temp_env();
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
    }

    #[test]
    fn jump_top_and_bottom_over_nested_tree() {
        let (mut app, _env) = app_in_temp_env();
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
    }

    #[test]
    fn visible_indices_filters_by_description_and_tag() {
        let (mut app, _env) = app_in_temp_env();
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
    }

    #[test]
    fn row_focus_conversion_roundtrip() {
        let (mut app, _env) = app_in_temp_env();
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
    }

    #[test]
    fn move_up_down_across_subtasks() {
        let (mut app, _env) = app_in_temp_env();
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
    }

    #[test]
    fn focused_subtask_helpers_reach_deep_nesting() {
        let (mut app, _env) = app_in_temp_env();
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
    }

    #[test]
    fn task_counts() {
        let (mut app, _env) = app_in_temp_env();
        let mut a = Task::new("a".to_string()).unwrap();
        a.status = crate::task::Status::Working;
        let b = Task::new("b".to_string()).unwrap();
        let mut c = Task::new("c".to_string()).unwrap();
        c.status = crate::task::Status::Done;
        let mut d = Task::new("d".to_string()).unwrap();
        d.status = crate::task::Status::Done;
        app.tasks = vec![a, b, c, d];
        assert_eq!(app.task_counts(), (1, 1, 2));
    }

    #[test]
    fn commit_message_substitutes_template_placeholders() {
        let (mut app, _env) = app_in_temp_env();
        let mut t = Task::new("fix login bug".to_string()).unwrap();
        t.status = Status::Working;
        app.tasks.push(t);
        app.commit_template = "fix: {task} [{status}] on {date}".to_string();
        app.focused = FocusedRow::Task(0);

        let vis = app.visible_indices();
        let msg = app.build_commit_message(&vis);
        assert_eq!(
            msg,
            format!("fix: fix login bug [working] on {}", crate::task::current_date())
        );
    }

    #[test]
    fn commit_message_blank_template_defaults_to_tcr_task() {
        let (mut app, _env) = app_in_temp_env();
        app.tasks.push(Task::new("buy milk".to_string()).unwrap());
        app.commit_template.clear(); // blank → default "TCR: {task}"
        app.focused = FocusedRow::Task(0);

        let vis = app.visible_indices();
        assert_eq!(app.build_commit_message(&vis), "TCR: buy milk");
    }

    #[test]
    fn commit_message_without_focused_task_uses_fallback() {
        let (mut app, _env) = app_in_temp_env();
        app.commit_template = "fix: {task}".to_string();
        app.tasks.push(Task::new("buy milk".to_string()).unwrap());

        // Cursor past the end of the list — no task to attach to
        app.focused = FocusedRow::Task(99);
        let vis = app.visible_indices();
        assert_eq!(app.build_commit_message(&vis), "TCR: tests passed");

        // Empty list entirely
        app.tasks.clear();
        app.focused = FocusedRow::Task(0);
        let vis = app.visible_indices();
        assert_eq!(app.build_commit_message(&vis), "TCR: tests passed");
    }

    #[test]
    fn commit_message_for_subtask_focus_uses_parent_task() {
        let (mut app, _env) = app_in_temp_env();
        let mut t = Task::new("parent task".to_string()).unwrap();
        t.subtasks.push(Subtask::new("child step".to_string()).unwrap());
        app.tasks.push(t);
        app.commit_template = "TCR: {task}".to_string();
        app.focused = FocusedRow::Subtask(0, vec![0]);

        let vis = app.visible_indices();
        assert_eq!(app.build_commit_message(&vis), "TCR: parent task");
    }

    #[test]
    fn set_test_command_preserves_other_config_fields() {
        let (mut app, _env) = app_in_temp_env();
        app.set_commit_template("x: {task}".into());
        app.set_test_command("cargo test".into());

        let cfg = load_config();
        assert_eq!(cfg.commit_template, "x: {task}", "template must survive a test-command save");
        assert_eq!(cfg.test_command, "cargo test");
        assert!(!cfg.auto_tcr);

        // And the template itself round-trips into a fresh App
        let app2 = App::new();
        assert_eq!(app2.commit_template, "x: {task}");
    }

    #[test]
    fn external_edit_detected_only_after_disk_content_changes() {
        let (mut app, _env) = app_in_temp_env();
        // Fresh dir, no file yet → nothing external happened
        assert!(!app.detect_external_edit());

        app.tasks.push(Task::new("my task".to_string()).unwrap());
        app.save();
        assert!(!app.detect_external_edit(), "file matches what we wrote");

        // Someone edits tasks.md outside the app
        std::fs::write("tasks.md", "- [ ] external edit\n").unwrap();
        assert!(app.detect_external_edit());
    }

    #[test]
    fn reload_from_disk_pulls_external_tasks_and_keeps_undo_snapshot() {
        let (mut app, _env) = app_in_temp_env();
        app.tasks.push(Task::new("my task".to_string()).unwrap());
        app.save();
        std::fs::write("tasks.md", "- [ ] external task\n").unwrap();

        app.reload_from_disk();
        assert_eq!(app.tasks.len(), 1);
        assert_eq!(app.tasks[0].description, "external task");
        assert!(!app.detect_external_edit(), "baseline updated after reload");
        // The in-memory version is recoverable with `u`
        app.undo();
        assert_eq!(app.tasks.len(), 1);
        assert_eq!(app.tasks[0].description, "my task");
    }

    #[test]
    fn keep_my_version_overwrites_external_edit() {
        let (mut app, _env) = app_in_temp_env();
        app.tasks.push(Task::new("my task".to_string()).unwrap());
        app.save();
        std::fs::write("tasks.md", "- [ ] external edit\n").unwrap();
        assert!(app.detect_external_edit());

        app.keep_my_version();
        let on_disk = std::fs::read_to_string("tasks.md").unwrap();
        assert!(on_disk.contains("my task"), "disk: {on_disk}");
        assert!(!on_disk.contains("external edit"));
        assert!(!app.detect_external_edit());
    }

    #[test]
    fn visible_indices_applies_filter_and_search_together() {
        let (mut app, _env) = app_in_temp_env();
        app.tasks.push(Task::new("fix login #auth".to_string()).unwrap());           // 0 pending
        let mut docs = Task::new("write docs #meta".to_string()).unwrap();
        docs.status = Status::Working;
        app.tasks.push(docs);                                                       // 1 working
        let mut milk = Task::new("buy milk".to_string()).unwrap();
        milk.priority = Priority::High;
        app.tasks.push(milk);                                                       // 2 pending high

        // Status filter
        app.filter_query = "s:pending".into();
        assert_eq!(app.visible_indices(), vec![0, 2]);

        // Tag filter
        app.filter_query = "#meta".into();
        assert_eq!(app.visible_indices(), vec![1]);

        // Status AND priority combined
        app.filter_query = "s:pending p:high".into();
        assert_eq!(app.visible_indices(), vec![2]);

        // Filter and search compose (AND)
        app.filter_query = "s:pending".into();
        app.search_query = "milk".into();
        assert_eq!(app.visible_indices(), vec![2]);

        // No filter → search alone, as before
        app.filter_query.clear();
        assert_eq!(app.visible_indices(), vec![2]);
    }

    // The auto-TCR tests run `sh -c true/false`, which only exists on unix.
    #[cfg(unix)]
    fn run_git(args: &[&str]) {
        let st = std::process::Command::new("git").args(args).status().expect("git must be available");
        assert!(st.success(), "git {args:?} failed");
    }

    #[test]
    fn auto_tcr_flag_persists_and_survives_other_config_saves() {
        let (mut app, _env) = app_in_temp_env();
        assert!(!app.auto_tcr);
        app.toggle_auto_tcr();
        assert!(app.auto_tcr);

        // A fresh App picks the flag up from disk…
        let app2 = App::new();
        assert!(app2.auto_tcr);
        // …and saving a different field must not wipe it
        let mut app3 = App::new();
        app3.set_test_command("cargo test".into());
        assert!(load_config().auto_tcr);
    }

    #[test]
    #[cfg(unix)]
    fn auto_tcr_runs_on_working_tree_change_and_commits_on_pass() {
        let (mut app, _env) = app_in_temp_env();
        // A real repo on a non-main branch (commit_tasks refuses main/master)
        run_git(&["init", "-b", "feature"]);
        run_git(&["config", "user.email", "t@t"]);
        run_git(&["config", "user.name", "t"]);
        std::fs::write("code.txt", "one\n").unwrap();
        run_git(&["add", "."]);
        run_git(&["commit", "-m", "init"]);

        app.set_test_command("true".into()); // `sh -c true` always passes
        app.toggle_auto_tcr();

        // First poll only sets the baseline — a clean tree never triggers a run
        app.maybe_start_auto_tcr();
        assert!(!app.tcr_running, "clean tree must not trigger TCR");

        // A file save (dirty tree) triggers the run
        std::fs::write("code.txt", "one\ntwo\n").unwrap();
        app.maybe_start_auto_tcr();
        assert!(app.tcr_running);

        assert!(
            crate::testutil::wait_until(std::time::Duration::from_secs(2), || {
                app.poll_tcr_result();
                !app.tcr_running
            }),
            "background TCR run must finish within 2s"
        );
        let count = std::process::Command::new("git")
            .args(["rev-list", "--count", "HEAD"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&count.stdout).trim(), "2", "expected one TCR commit on top of init");

        // Regression: after the commit the tree differs from the pre-run
        // snapshot — a follow-up poll must NOT mistake that for new changes.
        app.maybe_start_auto_tcr();
        assert!(!app.tcr_running, "must not refire without new working-tree changes");
    }

    #[test]
    #[cfg(unix)]
    fn auto_tcr_failure_reverts_tree_and_saves_patch() {
        let (mut app, _env) = app_in_temp_env();
        run_git(&["init", "-b", "feature"]);
        run_git(&["config", "user.email", "t@t"]);
        run_git(&["config", "user.name", "t"]);
        std::fs::write("code.txt", "one\n").unwrap();
        run_git(&["add", "."]);
        run_git(&["commit", "-m", "init"]);

        app.set_test_command("false".into()); // `sh -c false` always fails
        app.toggle_auto_tcr();

        app.maybe_start_auto_tcr(); // baseline
        std::fs::write("code.txt", "one\ntwo\n").unwrap();
        app.maybe_start_auto_tcr();
        assert!(app.tcr_running);

        assert!(
            crate::testutil::wait_until(std::time::Duration::from_secs(2), || {
                app.poll_tcr_result();
                !app.tcr_running
            }),
            "background TCR run must finish within 2s"
        );

        // The tree was reverted to the committed state…
        assert_eq!(std::fs::read_to_string("code.txt").unwrap(), "one\n");
        // …the failure output panel is shown…
        assert!(app.show_tcr_output);
        // …and the lost diff is recoverable as exactly one patch
        let patches = crate::git::list_patches();
        assert_eq!(patches.len(), 1, "patches: {patches:?}");
        crate::git::delete_patch(&patches[0]).unwrap();
        assert!(crate::git::list_patches().is_empty());
    }

    fn write_json_file(name: &str, tasks: &[Task]) -> String {
        // Relative to the test's temp cwd, so it is cleaned up with the env.
        let dir = std::path::Path::new("imports");
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        crate::storage::export_to_json(tasks, path.to_str().unwrap()).unwrap();
        path.to_str().unwrap().to_string()
    }

    #[test]
    fn import_append_dedupes_and_returns_counts() {
        let (mut app, _env) = app_in_temp_env();
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
    }

    #[test]
    fn import_replace_swaps_the_list() {
        let (mut app, _env) = app_in_temp_env();
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
    }

    #[test]
    fn import_bad_file_is_an_error_and_keeps_tasks() {
        let (mut app, _env) = app_in_temp_env();
        app.tasks.push(Task::new("keep me".to_string()).unwrap());

        let err = app.import_tasks("/nonexistent/import.json", false).unwrap_err();
        assert!(err.contains("Cannot read"), "got: {err}");
        assert_eq!(app.tasks.len(), 1);
        // No undo snapshot was pushed on failure
        assert!(app.undo_stack.is_empty());
    }
}
