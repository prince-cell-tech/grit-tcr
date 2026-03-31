use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::io::stdout;
use std::process::Command;
use std::{fs, io, time::Duration};
use tui::layout::{Constraint, Direction, Layout};
use tui::style::{Color, Modifier, Style};
use tui::text::{Span, Spans};
use tui::widgets::{Block, Borders, List, ListItem, Paragraph};
use tui::{Terminal, backend::CrosstermBackend};

// ─── Data types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
enum Status {
    Pending,
    Working,
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
enum Priority {
    High,
    Medium,
    Low,
}

impl Priority {
    fn label(&self) -> &str {
        match self {
            Priority::High => "!!",
            Priority::Medium => "! ",
            Priority::Low => "  ",
        }
    }
    fn color(&self) -> Color {
        match self {
            Priority::High => Color::Red,
            Priority::Medium => Color::Yellow,
            Priority::Low => Color::Blue,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
enum SubStatus {
    Pending,
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Subtask {
    description: String,
    status: SubStatus,
}

impl Subtask {
    fn new(description: String) -> Option<Self> {
        let d = description.trim().to_string();
        if d.is_empty() { None } else { Some(Subtask { description: d, status: SubStatus::Pending }) }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Task {
    description: String,
    status: Status,
    priority: Priority,
    tags: Vec<String>,
    due_date: Option<String>, // stored as YYYY-MM-DD string
    created_at: String,
    subtasks: Vec<Subtask>,
}

impl Task {
    fn new(description: String) -> Option<Self> {
        if description.trim().is_empty() {
            return None;
        }
        let (desc, tags) = parse_tags(&description);
        let (desc, due) = parse_due_date(&desc);
        let (desc, priority) = parse_priority(&desc);
        Some(Task {
            description: desc.trim().to_string(),
            status: Status::Pending,
            priority,
            tags,
            due_date: due,
            created_at: current_date(),
            subtasks: vec![],
        })

    }
}

fn parse_tags(input: &str) -> (String, Vec<String>) {
    let mut tags = vec![];
    let mut rest = String::new();
    for word in input.split_whitespace() {
        if word.starts_with('#') {
            tags.push(word[1..].to_string());
        } else {
            if !rest.is_empty() { rest.push(' '); }
            rest.push_str(word);
        }
    }
    (rest, tags)
}

fn parse_due_date(input: &str) -> (String, Option<String>) {
    let mut due = None;
    let mut rest = String::new();
    for word in input.split_whitespace() {
        if word.starts_with("due:") {
            due = Some(word[4..].to_string());
        } else {
            if !rest.is_empty() { rest.push(' '); }
            rest.push_str(word);
        }
    }
    (rest, due)
}

fn parse_priority(input: &str) -> (String, Priority) {
    let mut priority = Priority::Medium;
    let mut rest = String::new();
    for word in input.split_whitespace() {
        match word {
            "p:high" | "p:h" => priority = Priority::High,
            "p:low" | "p:l" => priority = Priority::Low,
            "p:medium" | "p:m" => priority = Priority::Medium,
            _ => {
                if !rest.is_empty() { rest.push(' '); }
                rest.push_str(word);
            }
        }
    }
    (rest, priority)
}

fn current_date() -> String {
    // Simple: use `date` command
    Command::new("date")
        .arg("+%Y-%m-%d")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

fn is_overdue(due: &str) -> bool {
    let today = current_date();
    due < today.as_str()
}

// ─── Undo ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct Snapshot {
    tasks: Vec<Task>,
    selected: usize,
}

// ─── Sort ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum SortMode {
    None,
    Status,
    Priority,
    DueDate,
    Created,
}

impl SortMode {
    fn label(&self) -> &str {
        match self {
            SortMode::None => "Default",
            SortMode::Status => "Status",
            SortMode::Priority => "Priority",
            SortMode::DueDate => "Due Date",
            SortMode::Created => "Created",
        }
    }
    fn next(&self) -> SortMode {
        match self {
            SortMode::None => SortMode::Status,
            SortMode::Status => SortMode::Priority,
            SortMode::Priority => SortMode::DueDate,
            SortMode::DueDate => SortMode::Created,
            SortMode::Created => SortMode::None,
        }
    }
}

fn sorted_indices(tasks: &[Task], mode: &SortMode) -> Vec<usize> {
    let mut indices: Vec<usize> = (0..tasks.len()).collect();
    match mode {
        SortMode::None => {}
        SortMode::Status => {
            indices.sort_by_key(|&i| match tasks[i].status {
                Status::Working => 0,
                Status::Pending => 1,
                Status::Done => 2,
            });
        }
        SortMode::Priority => {
            indices.sort_by_key(|&i| match tasks[i].priority {
                Priority::High => 0,
                Priority::Medium => 1,
                Priority::Low => 2,
            });
        }
        SortMode::DueDate => {
            indices.sort_by_key(|&i| tasks[i].due_date.clone().unwrap_or_else(|| "9999".to_string()));
        }
        SortMode::Created => {
            indices.sort_by_key(|&i| tasks[i].created_at.clone());
        }
    }
    indices
}

// ─── File / git ───────────────────────────────────────────────────────────────

const TASKS_FILE: &str = "tasks.md";
const PROJECTS_DIR: &str = "projects";

fn load_tasks() -> Vec<Task> {
    load_tasks_from(TASKS_FILE)
}

fn load_tasks_from(path: &str) -> Vec<Task> {
    let content = fs::read_to_string(path).unwrap_or_default();
    let mut tasks: Vec<Task> = vec![];
    let mut current_task: Option<Task> = None;

    for line in content.lines() {
        // Use the raw line for indent detection so leading spaces are preserved
        let trimmed = line.trim();
        let is_subtask = line.starts_with("  - ") || line.starts_with("\t- ");

        if !is_subtask && trimmed.starts_with("- [") {
            // Top-level task
            if let Some(t) = current_task.take() {
                tasks.push(t);
            }
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
        } else if is_subtask {
            // Subtask line — trimmed is now "- [x] desc" or "- [ ] desc" or "- plain"
            if let Some(ref mut t) = current_task {
                if trimmed.starts_with("- [x]") && trimmed.len() > 6 {
                    let sub_desc = trimmed[6..].trim().to_string();
                    if !sub_desc.is_empty() {
                        t.subtasks.push(Subtask { description: sub_desc, status: SubStatus::Done });
                    }
                } else if trimmed.starts_with("- [") && trimmed.len() > 6 {
                    // "- [ ] " or any other marker → pending
                    let sub_desc = trimmed[6..].trim().to_string();
                    if !sub_desc.is_empty() {
                        t.subtasks.push(Subtask { description: sub_desc, status: SubStatus::Pending });
                    }
                } else if trimmed.starts_with("- ") {
                    // plain "- desc" with no marker → pending
                    let sub_desc = trimmed[2..].trim().to_string();
                    if !sub_desc.is_empty() {
                        t.subtasks.push(Subtask { description: sub_desc, status: SubStatus::Pending });
                    }
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

fn save_tasks(tasks: &[Task]) {
    save_tasks_to(tasks, TASKS_FILE);
}

fn save_tasks_to(tasks: &[Task], path: &str) {
    // Don't create the file if it doesn't exist yet and there's nothing to save
    if tasks.is_empty() && !std::path::Path::new(path).exists() {
        return;
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
            for sub in &task.subtasks {
                let sub_marker = if sub.status == SubStatus::Done { "x" } else { " " };
                content.push_str(&format!("  - [{}] {}\n", sub_marker, sub.description));
            }
        }
        content.push('\n');
    }

    fs::write(path, content).expect("Failed to write file");
}

fn export_to_json(tasks: &[Task]) {
    let json = serde_json::to_string_pretty(tasks).expect("Failed to serialize tasks");
    fs::write("tasks.json", json).expect("Failed to write JSON file");
}

fn run_test_command(command: &str) -> bool {
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() { return false; }
    Command::new(parts[0])
        .args(&parts[1..])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn commit_tasks(message: &str) -> Result<(), String> {
    let branch = git_branch();
    if branch == "main" || branch == "master" {
        return Err(format!(
            "Refusing to commit: currently on '{}'. Switch to a feature branch first.",
            branch
        ));
    }
    let add = Command::new("git").args(["add", "-A"]).status().map_err(|e| e.to_string())?;
    if !add.success() { return Err("git add failed".to_string()); }
    let commit = Command::new("git").args(["commit", "-m", message]).status().map_err(|e| e.to_string())?;
    if !commit.success() { return Err("git commit failed".to_string()); }
    Ok(())
}

fn git_branch() -> String {
    Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "?".to_string())
}

fn git_log(n: usize) -> Vec<String> {
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

fn list_projects() -> Vec<String> {
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

// ─── App state ────────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Clone)]
enum Mode {
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
}

// Which row the cursor is on — a task header or one of its subtasks
#[derive(Debug, Clone, PartialEq)]
enum FocusedRow {
    Task(usize),           // display index into visible_indices()
    Subtask(usize, usize), // (display index of parent task, subtask index)
}

struct App {
    tasks: Vec<Task>,
    selected: usize,        // display index (into visible_indices)
    focused: FocusedRow,    // which row is highlighted
    scroll_offset: usize,
    mode: Mode,
    input: String,
    test_command: String,
    undo_stack: Vec<Snapshot>,
    sort_mode: SortMode,
    search_query: String,
    show_git_log: bool,
    current_project: String,   // filename without extension; empty = default
    projects: Vec<String>,
    commit_msg_buf: String,
    status_msg: String,        // ephemeral bottom message
}

impl App {
    fn new() -> Self {
        let tasks = load_tasks();
        App {
            tasks,
            selected: 0,
            focused: FocusedRow::Task(0),
            scroll_offset: 0,
            mode: Mode::View,
            input: String::new(),
            test_command: String::from(" "),
            undo_stack: vec![],
            sort_mode: SortMode::None,
            search_query: String::new(),
            show_git_log: false,
            current_project: String::new(),
            projects: list_projects(),
            commit_msg_buf: String::new(),
            status_msg: String::new(),
        }
    }

    fn push_undo(&mut self) {
        self.undo_stack.push(Snapshot {
            tasks: self.tasks.clone(),
            selected: self.selected,
        });
        if self.undo_stack.len() > 50 {
            self.undo_stack.remove(0);
        }
    }

    fn undo(&mut self) {
        if let Some(snap) = self.undo_stack.pop() {
            self.tasks = snap.tasks;
            self.selected = snap.selected;
            self.save();
            self.status_msg = "Undone.".to_string();
        } else {
            self.status_msg = "Nothing to undo.".to_string();
        }
    }

    fn save(&self) {
        let path = if self.current_project.is_empty() {
            TASKS_FILE.to_string()
        } else {
            format!("{}/{}.md", PROJECTS_DIR, self.current_project)
        };
        save_tasks_to(&self.tasks, &path);
    }

    fn load_project(&mut self, name: &str) {
        self.current_project = name.to_string();
        let path = if name.is_empty() {
            TASKS_FILE.to_string()
        } else {
            fs::create_dir_all(PROJECTS_DIR).ok(); // only create when actually needed
            format!("{}/{}.md", PROJECTS_DIR, name)
        };
        self.tasks = load_tasks_from(&path);
        self.selected = 0;
        self.focused = FocusedRow::Task(0);
        self.scroll_offset = 0;
    }

    fn visible_indices(&self) -> Vec<usize> {
        let sorted = sorted_indices(&self.tasks, &self.sort_mode);
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

    fn clamp_selected(&mut self, vis_len: usize) {
        if vis_len == 0 {
            self.selected = 0;
            self.focused = FocusedRow::Task(0);
        } else if self.selected >= vis_len {
            self.selected = vis_len - 1;
            self.focused = FocusedRow::Task(self.selected);
        }
    }

    // Total number of visible rows including subtasks
    fn total_rows(&self, vis: &[usize]) -> usize {
        vis.iter().map(|&i| 1 + self.tasks[i].subtasks.len()).sum()
    }

    // Convert a flat row index to FocusedRow
    fn row_index_to_focused(&self, vis: &[usize], row: usize) -> FocusedRow {
        let mut offset = 0;
        for (di, &ri) in vis.iter().enumerate() {
            if row == offset {
                return FocusedRow::Task(di);
            }
            offset += 1;
            let subs = self.tasks[ri].subtasks.len();
            if row < offset + subs {
                return FocusedRow::Subtask(di, row - offset);
            }
            offset += subs;
        }
        FocusedRow::Task(vis.len().saturating_sub(1))
    }

    // Convert FocusedRow to flat row index
    fn focused_to_row_index(&self, vis: &[usize]) -> usize {
        let mut offset = 0;
        for (di, &ri) in vis.iter().enumerate() {
            match &self.focused {
                FocusedRow::Task(t) if *t == di => return offset,
                FocusedRow::Subtask(t, si) if *t == di => return offset + 1 + si,
                _ => {}
            }
            offset += 1 + self.tasks[ri].subtasks.len();
        }
        0
    }

    fn move_down(&mut self, vis: &[usize]) {
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

    fn move_up(&mut self, vis: &[usize]) {
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

    fn task_counts(&self) -> (usize, usize, usize) {
        let pending = self.tasks.iter().filter(|t| t.status == Status::Pending).count();
        let working = self.tasks.iter().filter(|t| t.status == Status::Working).count();
        let done = self.tasks.iter().filter(|t| t.status == Status::Done).count();
        (pending, working, done)
    }
}

// ─── Main ─────────────────────────────────────────────────────────────────────

fn main() -> Result<(), Box<dyn Error>> {
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = run_app(&mut terminal);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("Error: {:?}", err);
    }
    Ok(())
}

// ─── UI rendering ─────────────────────────────────────────────────────────────

fn run_app(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>) -> Result<(), Box<dyn Error>> {
    let mut app = App::new();

    loop {
        terminal.draw(|f| {
            let size = f.size();

            // Adjust scroll so selected row stays visible
            let vis = app.visible_indices();
            let flat_row = app.focused_to_row_index(&vis);
            let list_height = size.height.saturating_sub(9) as usize;
            if flat_row >= app.scroll_offset + list_height {
                app.scroll_offset = flat_row + 1 - list_height;
            }
            if flat_row < app.scroll_offset {
                app.scroll_offset = flat_row;
            }

            // Layout: header | task list | [git log] | input | status bar
            let git_log_height = if app.show_git_log { 6u16 } else { 0 };
            let input_height: u16 = match app.mode {
                Mode::Input | Mode::Edit | Mode::EditSubtask | Mode::Test | Mode::Search | Mode::Subtask | Mode::CommitMsg => 3,
                _ => 0,
            };

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(1)
                .constraints([
                    Constraint::Length(2),           // header
                    Constraint::Min(3),              // task list
                    Constraint::Length(git_log_height),
                    Constraint::Length(input_height),
                    Constraint::Length(1),           // status bar
                ])
                .split(size);

            // ── Header ──
            let (pending, working, done) = app.task_counts();
            let branch = git_branch();
            let proj_label = if app.current_project.is_empty() { "default".to_string() } else { app.current_project.clone() };
            let sort_label = app.sort_mode.label();
            let header_text = format!(
                " 📋 {} | branch: {} | sort: {} | pending:{} working:{} done:{}",
                proj_label, branch, sort_label, pending, working, done
            );
            let header = Paragraph::new(header_text)
                .style(Style::default().fg(Color::Cyan));
            f.render_widget(header, chunks[0]);

            // ── Help overlay ──
            if app.mode == Mode::Help {
                let help_lines = vec![
                    Spans::from(Span::styled(" Navigation", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
                    Spans::from(" j/↓  move down          k/↑  move up"),
                    Spans::from(" Enter toggle status      d    delete task"),
                    Spans::from(""),
                    Spans::from(Span::styled(" Task management", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
                    Spans::from(" a    add task            e    edit task / subtask"),
                    Spans::from(" s    add subtask         u    undo"),
                    Spans::from(" p    cycle priority      d    delete task / subtask"),
                    Spans::from(""),
                    Spans::from(Span::styled(" Subtasks", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
                    Spans::from(" j/k  navigate into subtasks (they appear below their task)"),
                    Spans::from(" Enter toggle subtask [ ] / [x]   e  edit subtask text"),
                    Spans::from(""),
                    Spans::from(Span::styled(" View / filter", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
                    Spans::from(" /    search              Esc  clear search"),
                    Spans::from(" S    cycle sort           g    toggle git log"),
                    Spans::from(" P    switch project"),
                    Spans::from(""),
                    Spans::from(Span::styled(" TCR / git", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
                    Spans::from(" T    set test command    t    run test+commit"),
                    Spans::from(" c    custom commit msg"),
                    Spans::from(""),
                    Spans::from(Span::styled(" Export / quit", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
                    Spans::from(" E    export to JSON      q    quit"),
                    Spans::from(" ?    toggle this help"),
                    Spans::from(""),
                    Spans::from(Span::styled(" Task syntax when adding/editing:", Style::default().fg(Color::Cyan))),
                    Spans::from("  #tag  due:YYYY-MM-DD  p:high / p:medium / p:low"),
                    Spans::from("  Example: Fix login bug #bug due:2025-06-01 p:high"),
                ];
                let help = Paragraph::new(help_lines)
                    .block(Block::default().title("Help (? to close)").borders(Borders::ALL))
                    .style(Style::default());
                f.render_widget(help, chunks[1]);
            } else {
                // ── Task list ──
                let title = if !app.search_query.is_empty() {
                    format!("Tasks [search: \"{}\"] — ? for help", app.search_query)
                } else {
                    "Tasks — ? for help".to_string()
                };

                let task_items: Vec<ListItem> = {
                    let mut items: Vec<ListItem> = vec![];
                    for (display_i, &real_i) in vis.iter().enumerate() {
                        let task = &app.tasks[real_i];
                        let prefix = match task.status {
                            Status::Done => "[done]   ",
                            Status::Working => "[working]",
                            Status::Pending => "[  ]     ",
                        };
                        let pri = task.priority.label();
                        let tags_str: String = task.tags.iter().map(|t| format!(" #{}", t)).collect();
                        let due_str = task.due_date.as_deref().map(|d| format!(" due:{}", d)).unwrap_or_default();

                        let overdue = task.due_date.as_deref().map(|d| is_overdue(d)).unwrap_or(false)
                            && task.status != Status::Done;

                        let task_selected = app.focused == FocusedRow::Task(display_i);
                        let base_style = if task_selected {
                            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::White)
                        };

                        let mut spans = vec![
                            Span::styled(format!("{} ", prefix), base_style),
                            Span::styled(format!("[{}] ", pri), Style::default().fg(task.priority.color())),
                            Span::styled(task.description.clone(), base_style),
                            Span::styled(tags_str, Style::default().fg(Color::Cyan)),
                        ];

                        if overdue {
                            spans.push(Span::styled(due_str, Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)));
                        } else if !task.due_date.as_deref().unwrap_or("").is_empty() {
                            spans.push(Span::styled(due_str, Style::default().fg(Color::Magenta)));
                        }

                        if !task.subtasks.is_empty() {
                            let done_count = task.subtasks.iter().filter(|s| s.status == SubStatus::Done).count();
                            spans.push(Span::styled(
                                format!(" [{}/{}]", done_count, task.subtasks.len()),
                                Style::default().fg(Color::DarkGray),
                            ));
                        }

                        items.push(ListItem::new(Spans::from(spans)));

                        // Render each subtask as its own row
                        for (si, sub) in task.subtasks.iter().enumerate() {
                            let sub_selected = app.focused == FocusedRow::Subtask(display_i, si);
                            let marker = match sub.status {
                                SubStatus::Done => "[x]",
                                SubStatus::Pending => "[ ]",
                            };
                            let sub_base = if sub_selected {
                                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                            } else if sub.status == SubStatus::Done {
                                Style::default().fg(Color::DarkGray).add_modifier(Modifier::CROSSED_OUT)
                            } else {
                                Style::default().fg(Color::Gray)
                            };
                            let connector = if si + 1 < task.subtasks.len() { "├─" } else { "└─" };
                            let sub_spans = vec![
                                Span::styled(format!("    {} {} ", connector, marker), Style::default().fg(Color::DarkGray)),
                                Span::styled(sub.description.clone(), sub_base),
                            ];
                            items.push(ListItem::new(Spans::from(sub_spans)));
                        }
                    }
                    items
                };

                let tasks_list = List::new(task_items)
                    .block(Block::default().title(title).borders(Borders::ALL));
                f.render_widget(tasks_list, chunks[1]);
            }

            // ── Git log panel ──
            if app.show_git_log && git_log_height > 0 {
                let log_entries = git_log(4);
                let log_lines: Vec<Spans> = log_entries.iter().map(|l| Spans::from(l.as_str())).collect();
                let log_widget = Paragraph::new(log_lines)
                    .block(Block::default().title("Recent commits (g to toggle)").borders(Borders::ALL))
                    .style(Style::default().fg(Color::DarkGray));
                f.render_widget(log_widget, chunks[2]);
            }

            // ── Input box ──
            if input_height > 0 {
                let (title, hint) = match app.mode {
                    Mode::Input => ("Add task", " syntax: description #tag due:YYYY-MM-DD p:high|medium|low"),
                    Mode::Edit => ("Edit task", " syntax: description #tag due:YYYY-MM-DD p:high|medium|low"),
                    Mode::EditSubtask => ("Edit subtask", " Enter to confirm, Esc to cancel"),
                    Mode::Test => ("Set test command", " e.g. cargo test"),
                    Mode::Search => ("Search", " filter tasks by description or tag (Esc to clear)"),
                    Mode::Subtask => ("Add subtask", " Enter to confirm, Esc to cancel"),
                    Mode::CommitMsg => ("Commit message", " Enter to commit, Esc to cancel"),
                    _ => ("", ""),
                };
                let display = format!("{}{}", app.input, hint);
                let input_widget = Paragraph::new(display.as_str())
                    .block(Block::default().title(title).borders(Borders::ALL))
                    .style(Style::default().fg(Color::Green));
                f.render_widget(input_widget, chunks[3]);
            }

            // ── Status bar ──
            let status_text = if !app.status_msg.is_empty() {
                app.status_msg.clone()
            } else {
                let vis_len_now = app.visible_indices().len();
                format!(" {} of {} tasks shown", vis_len_now, app.tasks.len())
            };
            let status_bar = Paragraph::new(status_text.as_str())
                .style(Style::default().fg(Color::DarkGray));
            f.render_widget(status_bar, chunks[4]);

            let _vis_len = vis.len(); // keep vis alive
        })?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                // Clear ephemeral status on any key
                app.status_msg.clear();

                match &app.mode.clone() {
                    Mode::View => {
                        let vis = app.visible_indices();
                        let vis_len = vis.len();
                        match (key.modifiers, key.code) {
                            (_, KeyCode::Char('q')) => break,
                            (_, KeyCode::Char('?')) => {
                                app.mode = Mode::Help;
                            }
                            (_, KeyCode::Char('j')) | (_, KeyCode::Down) => {
                                let vis2 = app.visible_indices();
                                app.move_down(&vis2);
                            }
                            (_, KeyCode::Char('k')) | (_, KeyCode::Up) => {
                                let vis2 = app.visible_indices();
                                app.move_up(&vis2);
                            }
                            (_, KeyCode::Char('d')) => {
                                if !vis.is_empty() {
                                    match app.focused.clone() {
                                        FocusedRow::Task(di) => {
                                            let real = vis[di];
                                            app.push_undo();
                                            app.tasks.remove(real);
                                            app.focused = FocusedRow::Task(0);
                                            app.selected = 0;
                                            app.clamp_selected(vis_len.saturating_sub(1));
                                            app.save();
                                            app.status_msg = "Task deleted. Press u to undo.".to_string();
                                        }
                                        FocusedRow::Subtask(di, si) => {
                                            let real = vis[di];
                                            app.push_undo();
                                            app.tasks[real].subtasks.remove(si);
                                            app.focused = FocusedRow::Task(di);
                                            app.save();
                                            app.status_msg = "Subtask deleted. Press u to undo.".to_string();
                                        }
                                    }
                                }
                            }
                            (_, KeyCode::Char('u')) => {
                                app.undo();
                                app.focused = FocusedRow::Task(app.selected);
                            }
                            (_, KeyCode::Char('a')) => {
                                app.input.clear();
                                app.mode = Mode::Input;
                            }
                            (_, KeyCode::Char('e')) => {
                                match app.focused.clone() {
                                    FocusedRow::Task(di) => {
                                        if let Some(&real) = vis.get(di) {
                                            let t = &app.tasks[real];
                                            let tags_str: String = t.tags.iter().map(|tag| format!(" #{}", tag)).collect();
                                            let due_str = t.due_date.as_deref().map(|d| format!(" due:{}", d)).unwrap_or_default();
                                            let pri_str = match t.priority {
                                                Priority::High => " p:high",
                                                Priority::Medium => "",
                                                Priority::Low => " p:low",
                                            };
                                            app.input = format!("{}{}{}{}", t.description, tags_str, due_str, pri_str);
                                            app.mode = Mode::Edit;
                                        }
                                    }
                                    FocusedRow::Subtask(di, si) => {
                                        if let Some(&real) = vis.get(di) {
                                            app.input = app.tasks[real].subtasks[si].description.clone();
                                            app.mode = Mode::EditSubtask;
                                        }
                                    }
                                }
                            }
                            (_, KeyCode::Char('s')) => {
                                if !vis.is_empty() {
                                    app.input.clear();
                                    app.mode = Mode::Subtask;
                                }
                            }
                            (_, KeyCode::Char('p')) => {
                                if let FocusedRow::Task(di) = app.focused.clone() {
                                    if let Some(&real) = vis.get(di) {
                                        app.push_undo();
                                        app.tasks[real].priority = match app.tasks[real].priority {
                                            Priority::High => Priority::Medium,
                                            Priority::Medium => Priority::Low,
                                            Priority::Low => Priority::High,
                                        };
                                        app.save();
                                    }
                                }
                            }
                            (_, KeyCode::Char('T')) => {
                                app.input = app.test_command.clone();
                                app.mode = Mode::Test;
                            }
                            (_, KeyCode::Char('t')) => {
                                disable_raw_mode()?;
                                execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
                                let passed = run_test_command(&app.test_command);
                                if passed {
                                    app.save();
                                    let msg = if let FocusedRow::Task(di) = &app.focused {
                                        if let Some(&real) = vis.get(*di) {
                                            format!("TCR: completed task \"{}\"", app.tasks[real].description)
                                        } else { "TCR: tests passed".to_string() }
                                    } else { "TCR: tests passed".to_string() };
                                    match commit_tasks(&msg) {
                                        Ok(_) => println!("Committed: {}", msg),
                                        Err(e) => println!("Not committed: {}", e),
                                    }
                                } else {
                                    println!("Tests failed, reverting...");
                                    let _ = Command::new("git").args(["restore", "."]).status();
                                }
                                println!("Press Enter to return...");
                                let _ = io::stdin().read_line(&mut String::new());
                                enable_raw_mode()?;
                                execute!(stdout(), EnterAlternateScreen, EnableMouseCapture)?;
                                let backend = CrosstermBackend::new(stdout());
                                *terminal = Terminal::new(backend)?;
                            }
                            (_, KeyCode::Char('c')) => {
                                app.commit_msg_buf.clear();
                                app.input.clear();
                                app.mode = Mode::CommitMsg;
                            }
                            (_, KeyCode::Enter) => {
                                match app.focused.clone() {
                                    FocusedRow::Task(di) => {
                                        if let Some(&real) = vis.get(di) {
                                            app.push_undo();
                                            app.tasks[real].status = match app.tasks[real].status {
                                                Status::Pending => Status::Done,
                                                Status::Done => Status::Working,
                                                Status::Working => Status::Pending,
                                            };
                                            app.save();
                                        }
                                    }
                                    FocusedRow::Subtask(di, si) => {
                                        if let Some(&real) = vis.get(di) {
                                            app.push_undo();
                                            app.tasks[real].subtasks[si].status = match app.tasks[real].subtasks[si].status {
                                                SubStatus::Pending => SubStatus::Done,
                                                SubStatus::Done => SubStatus::Pending,
                                            };
                                            app.save();
                                        }
                                    }
                                }
                            }
                            (_, KeyCode::Char('E')) => {
                                export_to_json(&app.tasks);
                                app.status_msg = "Exported to tasks.json".to_string();
                            }
                            (_, KeyCode::Char('S')) => {
                                app.sort_mode = app.sort_mode.next();
                                app.selected = 0;
                                app.focused = FocusedRow::Task(0);
                                app.status_msg = format!("Sort: {}", app.sort_mode.label());
                            }
                            (_, KeyCode::Char('g')) => {
                                app.show_git_log = !app.show_git_log;
                            }
                            (_, KeyCode::Char('P')) => {
                                app.projects = list_projects();
                                app.input.clear();
                                app.mode = Mode::Projects;
                            }
                            (_, KeyCode::Char('/')) => {
                                app.input = app.search_query.clone();
                                app.mode = Mode::Search;
                            }
                            (KeyModifiers::NONE, KeyCode::Esc) => {
                                if !app.search_query.is_empty() {
                                    app.search_query.clear();
                                    app.selected = 0;
                                    app.focused = FocusedRow::Task(0);
                                }
                            }
                            _ => {}
                        }
                    }

                    Mode::Help => {
                        // Any key closes help
                        app.mode = Mode::View;
                    }

                    Mode::Search => match key.code {
                        KeyCode::Enter | KeyCode::Esc => {
                            app.search_query = app.input.drain(..).collect();
                            app.selected = 0;
                            app.mode = Mode::View;
                        }
                        KeyCode::Char(c) => {
                            app.input.push(c);
                            // Live filter — update query immediately
                            app.search_query = app.input.clone();
                            app.selected = 0;
                        }
                        KeyCode::Backspace => {
                            app.input.pop();
                            app.search_query = app.input.clone();
                            app.selected = 0;
                        }
                        _ => {}
                    },

                    Mode::Input => match key.code {
                        KeyCode::Enter => {
                            app.push_undo();
                            let text: String = app.input.drain(..).collect();
                            if let Some(task) = Task::new(text) {
                                app.tasks.push(task);
                                app.save();
                                app.mode = Mode::View;
                            } else {
                                app.status_msg = "Task description cannot be empty.".to_string();
                                app.mode = Mode::View;
                            }
                        }
                        KeyCode::Esc => { app.input.clear(); app.mode = Mode::View; }
                        KeyCode::Char(c) => app.input.push(c),
                        KeyCode::Backspace => { app.input.pop(); }
                        _ => {}
                    },

                    Mode::Edit => {
                        let vis = app.visible_indices();
                        match key.code {
                            KeyCode::Enter => {
                                if let Some(&real) = vis.get(app.selected) {
                                    let text: String = app.input.drain(..).collect();
                                    if let Some(updated) = Task::new(text) {
                                        app.push_undo();
                                        let old = &app.tasks[real];
                                        app.tasks[real] = Task {
                                            status: old.status.clone(),
                                            created_at: old.created_at.clone(),
                                            subtasks: old.subtasks.clone(),
                                            ..updated
                                        };
                                        app.save();
                                        app.mode = Mode::View;
                                    } else {
                                        app.status_msg = "Description cannot be empty.".to_string();
                                        app.mode = Mode::View;
                                    }
                                }
                            }
                            KeyCode::Esc => { app.input.clear(); app.mode = Mode::View; }
                            KeyCode::Char(c) => app.input.push(c),
                            KeyCode::Backspace => { app.input.pop(); }
                            _ => {}
                        }
                    }

                    Mode::Subtask => {
                        let vis = app.visible_indices();
                        match key.code {
                            KeyCode::Enter => {
                                // Add subtask to the task that is currently focused (task-level)
                                let parent_di = match &app.focused {
                                    FocusedRow::Task(di) => *di,
                                    FocusedRow::Subtask(di, _) => *di,
                                };
                                if let Some(&real) = vis.get(parent_di) {
                                    let text: String = app.input.drain(..).collect();
                                    if let Some(sub) = Subtask::new(text) {
                                        app.push_undo();
                                        app.tasks[real].subtasks.push(sub);
                                        app.save();
                                        app.status_msg = "Subtask added.".to_string();
                                    }
                                }
                                app.mode = Mode::View;
                            }
                            KeyCode::Esc => { app.input.clear(); app.mode = Mode::View; }
                            KeyCode::Char(c) => app.input.push(c),
                            KeyCode::Backspace => { app.input.pop(); }
                            _ => {}
                        }
                    }

                    Mode::EditSubtask => {
                        let vis = app.visible_indices();
                        match key.code {
                            KeyCode::Enter => {
                                if let FocusedRow::Subtask(di, si) = app.focused.clone() {
                                    if let Some(&real) = vis.get(di) {
                                        let text: String = app.input.drain(..).collect();
                                        let trimmed = text.trim().to_string();
                                        if !trimmed.is_empty() {
                                            app.push_undo();
                                            app.tasks[real].subtasks[si].description = trimmed;
                                            app.save();
                                        } else {
                                            app.status_msg = "Subtask description cannot be empty.".to_string();
                                        }
                                    }
                                }
                                app.mode = Mode::View;
                            }
                            KeyCode::Esc => { app.input.clear(); app.mode = Mode::View; }
                            KeyCode::Char(c) => app.input.push(c),
                            KeyCode::Backspace => { app.input.pop(); }
                            _ => {}
                        }
                    }

                    Mode::Test => match key.code {
                        KeyCode::Enter => {
                            app.test_command = app.input.drain(..).collect();
                            app.mode = Mode::View;
                        }
                        KeyCode::Esc => { app.input.clear(); app.mode = Mode::View; }
                        KeyCode::Char(c) => app.input.push(c),
                        KeyCode::Backspace => { app.input.pop(); }
                        _ => {}
                    },

                    Mode::CommitMsg => match key.code {
                        KeyCode::Enter => {
                            let msg: String = app.input.drain(..).collect();
                            let msg = msg.trim().to_string();
                            if !msg.is_empty() {
                                app.save();
                                match commit_tasks(&msg) {
                                    Ok(_) => app.status_msg = "Committed!".to_string(),
                                    Err(e) => app.status_msg = format!("Commit failed: {}", e),
                                }
                            }
                            app.mode = Mode::View;
                        }
                        KeyCode::Esc => { app.input.clear(); app.mode = Mode::View; }
                        KeyCode::Char(c) => app.input.push(c),
                        KeyCode::Backspace => { app.input.pop(); }
                        _ => {}
                    },

                    Mode::Projects => match key.code {
                        KeyCode::Esc => { app.mode = Mode::View; }
                        KeyCode::Enter => {
                            let name: String = app.input.drain(..).collect();
                            let name = name.trim().to_string();
                            app.load_project(&name);
                            app.mode = Mode::View;
                            app.status_msg = if name.is_empty() {
                                "Switched to default project.".to_string()
                            } else {
                                format!("Switched to project: {}", name)
                            };
                        }
                        KeyCode::Char(c) => app.input.push(c),
                        KeyCode::Backspace => { app.input.pop(); }
                        _ => {}
                    },
                }
            }
        }
    }

    Ok(())
}
