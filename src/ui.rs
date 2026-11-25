// ─── UI rendering & event handling ───────────────────────────────────────────

use crate::app::{App, FocusedRow, Mode};
use crate::git::{commit_tasks, run_test_command};
use crate::storage::{export_to_json, is_valid_project_name, list_projects};
use crate::task::{current_date, is_overdue, Priority, Status, SubStatus, Subtask, Task};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::{Frame, Terminal, backend::CrosstermBackend};
use std::error::Error;
use std::io::{self, stdout};
use std::time::{Duration, Instant};

// Recursively render a subtask tree as list rows with tree connectors.
// `ancestor_continues` is one bool per ancestor level: true when that ancestor
// still has later siblings, so a vertical line continues on this row's prefix.
fn render_subtask_tree(
    subs: &[Subtask],
    path: &[usize],
    ancestor_continues: &[bool],
    focused: FocusedRow,
    display_i: usize,
    out: &mut Vec<ListItem>,
) {
    for (i, sub) in subs.iter().enumerate() {
        let mut this_path = path.to_vec();
        this_path.push(i);
        let is_last = i + 1 == subs.len();

        let mut prefix = String::from("    ");
        for &cont in ancestor_continues {
            prefix.push_str(if cont { "│ " } else { "  " });
        }
        prefix.push_str(if is_last { "└─ " } else { "├─ " });

        let marker = match sub.status {
            SubStatus::Done => "[x]",
            SubStatus::Pending => "[ ]",
        };
        let selected = focused == FocusedRow::Subtask(display_i, this_path.clone());
        let style = if selected {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else if sub.status == SubStatus::Done {
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::CROSSED_OUT)
        } else {
            Style::default().fg(Color::Gray)
        };

        let mut spans = vec![
            Span::styled(prefix, Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{} ", marker), style),
            Span::styled(sub.description.clone(), style),
        ];
        if !sub.subtasks.is_empty() {
            let total: usize = sub.subtasks.iter().map(|s| s.count_total()).sum();
            let done: usize = sub.subtasks.iter().map(|s| s.count_done()).sum();
            spans.push(Span::styled(
                format!(" [{}/{}]", done, total),
                Style::default().fg(Color::DarkGray),
            ));
        }
        out.push(ListItem::new(Line::from(spans)));

        let mut next_cont = ancestor_continues.to_vec();
        next_cont.push(!is_last);
        render_subtask_tree(&sub.subtasks, &this_path, &next_cont, focused.clone(), display_i, out);
    }
}

// ─── Layout ──────────────────────────────────────────────────────────────────

// Heights of the optional panels below the task list: (git log, TCR, input).
fn panel_heights(app: &App) -> (u16, u16, u16) {
    let git_log = if app.show_git_log { 6 } else { 0 };
    let tcr = if app.show_tcr_output {
        (app.tcr_output_tail.len() as u16).min(15) + 2
    } else {
        0
    };
    let input = if app.mode.prompt().is_some() { 3 } else { 0 };
    (git_log, tcr, input)
}

// Adjust scroll so the focused row stays visible. Called before drawing, so
// rendering itself never mutates app state.
fn keep_selection_visible(app: &mut App, height: u16) {
    let (git_log_height, tcr_height, input_height) = panel_heights(app);
    let vis = app.visible_indices();
    let flat_row = app.focused_to_row_index(&vis);
    // Inner height: terminal minus margins (2), header (2), status (1) and
    // the list block's own borders (2).
    let list_height =
        height.saturating_sub(7 + git_log_height + tcr_height + input_height) as usize;
    if flat_row >= app.scroll_offset + list_height {
        app.scroll_offset = flat_row + 1 - list_height;
    }
    if flat_row < app.scroll_offset {
        app.scroll_offset = flat_row;
    }
}

// ─── Rendering ───────────────────────────────────────────────────────────────

fn render(f: &mut Frame, app: &mut App) {
    let size = f.area();
    let (git_log_height, tcr_height, input_height) = panel_heights(app);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(2),           // header
            Constraint::Min(3),              // task list
            Constraint::Length(git_log_height),
            Constraint::Length(tcr_height),
            Constraint::Length(input_height),
            Constraint::Length(1),           // status bar
        ])
        .split(size);

    render_header(f, app, chunks[0]);
    if app.mode == Mode::PatchBrowser {
        render_patch_browser(f, app, chunks[1]);
    } else {
        render_task_list(f, app, chunks[1]);
    }
    if app.show_git_log && git_log_height > 0 {
        render_git_log(f, app, chunks[2]);
    }
    if app.show_tcr_output && tcr_height > 0 {
        render_tcr_panel(f, app, chunks[3]);
    }
    if input_height > 0 {
        render_input_box(f, app, chunks[4]);
    }
    render_status(f, app, chunks[5]);
    // Full-screen modal, rendered last so it covers everything beneath it.
    if app.mode == Mode::Help {
        render_help(f, size);
    }
}

fn render_header(f: &mut Frame, app: &mut App, area: Rect) {
    let (pending, working, done) = app.task_counts();
    let branch = app.cached_branch();
    let proj_label = if app.current_project.is_empty() {
        "default".to_string()
    } else {
        app.current_project.clone()
    };
    let sort_label = app.sort_mode.label();
    let auto_label = if app.auto_tcr { " | auto-TCR" } else { "" };
    let header_text = format!(
        " 📋 {} | branch: {} | sort: {} | pending:{} working:{} done:{}{}",
        proj_label, branch, sort_label, pending, working, done, auto_label
    );
    let header = Paragraph::new(header_text).style(Style::default().fg(Color::Cyan));
    f.render_widget(header, area);
}

fn task_list_title(app: &App) -> String {
    if !app.search_query.is_empty() {
        if !app.filter_query.is_empty() {
            format!(
                "Tasks [filter: \"{}\"] [search: \"{}\"] — ? for help",
                app.filter_query, app.search_query
            )
        } else {
            format!("Tasks [search: \"{}\"] — ? for help", app.search_query)
        }
    } else if !app.filter_query.is_empty() {
        format!("Tasks [filter: \"{}\"] — ? for help", app.filter_query)
    } else {
        "Tasks — ? for help".to_string()
    }
}

// One task row: status marker, priority, description, tags, due date, counts.
fn task_row(app: &App, task: &Task, display_i: usize, today: &str) -> ListItem<'static> {
    let prefix = match task.status {
        Status::Done => "[done]   ",
        Status::Working => "[working]",
        Status::Pending => "[  ]     ",
    };
    let pri = task.priority.label();
    let tags_str: String = task.tags.iter().map(|t| format!(" #{}", t)).collect();
    let due_str = task
        .due_date
        .as_deref()
        .map(|d| format!(" due:{}", d))
        .unwrap_or_default();

    let overdue = task
        .due_date
        .as_deref()
        .map(|d| is_overdue(d, today))
        .unwrap_or(false)
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
        spans.push(Span::styled(
            due_str,
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
    } else if !task.due_date.as_deref().unwrap_or("").is_empty() {
        spans.push(Span::styled(due_str, Style::default().fg(Color::Magenta)));
    }

    if !task.subtasks.is_empty() {
        let total: usize = task.subtasks.iter().map(|s| s.count_total()).sum();
        let done: usize = task.subtasks.iter().map(|s| s.count_done()).sum();
        spans.push(Span::styled(
            format!(" [{}/{}]", done, total),
            Style::default().fg(Color::DarkGray),
        ));
    }

    ListItem::new(Line::from(spans))
}

fn render_task_list(f: &mut Frame, app: &App, area: Rect) {
    let vis = app.visible_indices();
    let today = current_date();
    let mut items: Vec<ListItem> = vec![];
    for (display_i, &real_i) in vis.iter().enumerate() {
        let task = &app.tasks[real_i];
        items.push(task_row(app, task, display_i, &today));

        // Render the subtask tree recursively, with per-depth indent and
        // tree connectors.
        let mut rendered: Vec<ListItem> = vec![];
        render_subtask_tree(
            &task.subtasks,
            &[],
            &[],
            app.focused.clone(),
            display_i,
            &mut rendered,
        );
        items.extend(rendered);
    }

    let tasks_list = List::new(items)
        .block(Block::default().title(task_list_title(app)).borders(Borders::ALL));
    // Apply the scroll offset computed by keep_selection_visible; without a
    // stateful render the list always starts at row 0.
    let mut state = ListState::default();
    *state.offset_mut() = app.scroll_offset;
    f.render_stateful_widget(tasks_list, area, &mut state);
}

fn render_patch_browser(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = vec![];
    for (i, name) in app.patches.iter().enumerate() {
        let style = if i == app.patch_selected {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        lines.push(Line::from(Span::styled(format!(" {}", name), style)));
    }
    let browser = Paragraph::new(lines)
        .block(
            Block::default()
                .title("Revert patches — Enter: apply, d: delete, Esc: close")
                .borders(Borders::ALL),
        )
        .style(Style::default());
    f.render_widget(Clear, area);
    f.render_widget(browser, area);
}

fn render_git_log(f: &mut Frame, app: &mut App, area: Rect) {
    let log_entries = app.cached_git_log();
    let log_lines: Vec<Line> = log_entries.iter().map(|l| Line::from(l.as_str())).collect();
    let log_widget = Paragraph::new(log_lines)
        .block(Block::default().title("Recent commits (g to toggle)").borders(Borders::ALL))
        .style(Style::default().fg(Color::DarkGray));
    f.render_widget(log_widget, area);
}

fn render_tcr_panel(f: &mut Frame, app: &App, area: Rect) {
    let lines: Vec<Line> = app.tcr_output_tail.iter().map(|l| Line::from(l.as_str())).collect();
    let panel = Paragraph::new(lines)
        .block(
            Block::default()
                .title("TCR failed — last output (Esc to close)")
                .borders(Borders::ALL),
        )
        .style(Style::default().fg(Color::Red));
    f.render_widget(panel, area);
}

fn render_input_box(f: &mut Frame, app: &App, area: Rect) {
    let Some((title, hint)) = app.mode.prompt() else { return };
    let display = format!("{}{}", app.input, hint);
    let input_widget = Paragraph::new(display.as_str())
        .block(Block::default().title(title).borders(Borders::ALL))
        .style(Style::default().fg(if app.mode == Mode::ReloadPrompt {
            Color::Red
        } else {
            Color::Green
        }));
    f.render_widget(input_widget, area);
}

fn render_status(f: &mut Frame, app: &App, area: Rect) {
    let status_text = if !app.status_msg.is_empty() {
        app.status_msg.clone()
    } else {
        let vis_len_now = app.visible_indices().len();
        format!(" {} of {} tasks shown", vis_len_now, app.tasks.len())
    };
    let status_bar = Paragraph::new(status_text.as_str()).style(Style::default().fg(Color::DarkGray));
    f.render_widget(status_bar, area);
}

fn render_help(f: &mut Frame, area: Rect) {
    let help_lines = vec![
        Line::from(Span::styled(" Navigation", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
        Line::from(" j/↓  move down          k/↑  move up"),
        Line::from(" G    jump to bottom      H    jump to top"),
        Line::from(" Enter toggle status      d    delete task"),
        Line::from(Span::styled(" Task management", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
        Line::from(" a    add task            e    edit task / subtask"),
        Line::from(" s    add subtask         u    undo   Ctrl+r redo"),
        Line::from(" p    cycle priority      d    delete task / subtask"),
        Line::from(Span::styled(" Subtasks", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
        Line::from(" j/k  navigate into subtasks (they appear below their task)"),
        Line::from(" Enter toggle subtask [ ] / [x]   e  edit subtask text"),
        Line::from(" s    on a subtask: add a child under it (nesting)"),
        Line::from(Span::styled(" View / filter", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
        Line::from(" /    search              Esc  clear search/filter"),
        Line::from(" f    filter (#tag, s:status, p:priority + words)"),
        Line::from(" S    cycle sort           g    toggle git log"),
        Line::from(" P    switch project"),
        Line::from(Span::styled(" TCR / git", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
        Line::from(" T    set test command    t    run test+commit"),
        Line::from(" c    custom commit msg   m    commit msg template"),
        Line::from(" A    toggle auto-TCR (runs on file changes)"),
        Line::from(" r    restore a reverted change from a patch"),
        Line::from(Span::styled(" Import / export / quit", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
        Line::from(" I    import from JSON    E    export to JSON"),
        Line::from(" q    quit                ?    toggle this help"),
        Line::from(Span::styled(" Task syntax:  #tag  due:YYYY-MM-DD  p:high|medium|low", Style::default().fg(Color::Cyan))),
        Line::from(Span::styled("   e.g. Fix login bug #bug due:2025-06-01 p:high", Style::default().fg(Color::DarkGray))),
    ];
    let help = Paragraph::new(help_lines)
        .block(Block::default().title("Help (? to close)").borders(Borders::ALL))
        .style(Style::default());
    // Clear first: Paragraph only paints its text lines, so without this,
    // earlier widgets bleed through below the text.
    f.render_widget(Clear, area);
    f.render_widget(help, area);
}

// ─── Event loop ──────────────────────────────────────────────────────────────

pub fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<(), Box<dyn Error>> {
    let mut app = App::new();

    loop {
        keep_selection_visible(&mut app, terminal.size()?.height);
        terminal.draw(|f| render(f, &mut app))?;

        // Non-blocking: collect a finished background auto-TCR run.
        app.poll_tcr_result();

        // Throttled polling (every 2s): external task-file edits → prompt;
        // working-tree changes → auto-TCR run (only while idle in View).
        if app.mode == Mode::View && app.last_poll.elapsed() >= Duration::from_secs(2) {
            app.last_poll = Instant::now();
            if app.detect_external_edit() {
                app.mode = Mode::ReloadPrompt;
            } else if app.auto_tcr {
                app.maybe_start_auto_tcr();
            }
        }

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if handle_key(&mut app, key, terminal)? {
                    break;
                }
            }
        }
    }

    Ok(())
}

// Returns true when the app should quit.
fn handle_key(
    app: &mut App,
    key: KeyEvent,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<bool, Box<dyn Error>> {
    // Ignore key-release events (some terminals send them)
    if key.kind != KeyEventKind::Press {
        return Ok(false);
    }

    // Clear ephemeral status on any key
    app.status_msg.clear();

    // Ctrl+C always quits, from any mode
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Ok(true);
    }

    match app.mode.clone() {
        Mode::View => return handle_view_key(app, key, terminal),
        Mode::Help => app.mode = Mode::View,
        Mode::Search => handle_search_key(app, key),
        Mode::ReloadPrompt => handle_reload_prompt_key(app, key),
        Mode::Filter => handle_filter_key(app, key),
        Mode::CommitTemplate => handle_commit_template_key(app, key),
        Mode::PatchBrowser => handle_patch_browser_key(app, key),
        Mode::Input => handle_add_task_key(app, key),
        Mode::Edit => handle_edit_task_key(app, key),
        Mode::Subtask => handle_add_subtask_key(app, key),
        Mode::EditSubtask => handle_edit_subtask_key(app, key),
        Mode::Test => handle_test_command_key(app, key),
        Mode::CommitMsg => handle_commit_msg_key(app, key),
        Mode::Projects => handle_projects_key(app, key),
        Mode::Import => handle_import_key(app, key),
        Mode::ImportMerge => handle_import_merge_key(app, key),
    }
    Ok(false)
}

// Apply plain text-editing keys. Returns true when the key was consumed.
fn edit_line(buf: &mut String, key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::Char(c) => {
            buf.push(c);
            true
        }
        KeyCode::Backspace => {
            buf.pop();
            true
        }
        _ => false,
    }
}

fn reset_selection(app: &mut App) {
    app.selected = 0;
    app.focused = FocusedRow::Task(0);
}

// ─── View-mode keys ──────────────────────────────────────────────────────────

fn handle_view_key(
    app: &mut App,
    key: KeyEvent,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<bool, Box<dyn Error>> {
    match (key.modifiers, key.code) {
        (_, KeyCode::Char('q')) => return Ok(true),
        (_, KeyCode::Char('?')) => app.mode = Mode::Help,
        (_, KeyCode::Char('j')) | (_, KeyCode::Down) => {
            let vis = app.visible_indices();
            app.move_down(&vis);
        }
        (_, KeyCode::Char('k')) | (_, KeyCode::Up) => {
            let vis = app.visible_indices();
            app.move_up(&vis);
        }
        (_, KeyCode::Char('G')) => {
            let vis = app.visible_indices();
            app.jump_bottom(&vis);
        }
        (_, KeyCode::Char('H')) => app.jump_top(),
        (_, KeyCode::Char('d')) => {
            let vis = app.visible_indices();
            app.delete_focused(&vis);
        }
        (_, KeyCode::Enter) => {
            let vis = app.visible_indices();
            app.toggle_focused_status(&vis);
        }
        (_, KeyCode::Char('p')) => {
            let vis = app.visible_indices();
            app.cycle_focused_priority(&vis);
        }
        (_, KeyCode::Char('e')) => edit_focused(app),
        (_, KeyCode::Char('s')) => {
            let vis = app.visible_indices();
            if !vis.is_empty() {
                app.input.clear();
                app.mode = Mode::Subtask;
            }
        }
        (_, KeyCode::Char('u')) => {
            app.undo();
            app.focused = FocusedRow::Task(app.selected);
        }
        (KeyModifiers::CONTROL, KeyCode::Char('r')) => {
            app.redo();
            app.focused = FocusedRow::Task(app.selected);
        }
        (KeyModifiers::NONE, KeyCode::Esc) => clear_transient_view_state(app),
        _ => handle_view_command(app, key, terminal)?,
    }
    Ok(false)
}

// Prefill the edit prompt with the focused task's full metadata, or the
// focused subtask's description.
fn edit_focused(app: &mut App) {
    let vis = app.visible_indices();
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
        FocusedRow::Subtask(_, _) => {
            if let Some(sub) = app.focused_subtask(&vis) {
                app.input = sub.description.clone();
                app.mode = Mode::EditSubtask;
            }
        }
    }
}

// Esc in View mode peels off one layer at a time: TCR panel, filter, search.
fn clear_transient_view_state(app: &mut App) {
    if app.show_tcr_output {
        app.show_tcr_output = false;
    } else if !app.filter_query.is_empty() {
        app.filter_query.clear();
        reset_selection(app);
    } else if !app.search_query.is_empty() {
        app.search_query.clear();
        reset_selection(app);
    }
}

// Command keys that open prompts, panels, or run actions.
fn handle_view_command(
    app: &mut App,
    key: KeyEvent,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<(), Box<dyn Error>> {
    match key.code {
        KeyCode::Char('a') => {
            app.input.clear();
            app.mode = Mode::Input;
        }
        KeyCode::Char('T') => {
            app.input = app.test_command.clone();
            app.mode = Mode::Test;
        }
        KeyCode::Char('m') => {
            app.input = app.commit_template.clone();
            app.mode = Mode::CommitTemplate;
        }
        KeyCode::Char('A') => app.toggle_auto_tcr(),
        KeyCode::Char('r') => open_patch_browser(app),
        KeyCode::Char('f') => {
            app.input = app.filter_query.clone();
            app.mode = Mode::Filter;
        }
        KeyCode::Char('t') => run_tests_and_commit(app, terminal)?,
        KeyCode::Char('c') => {
            // Prefill with the template-generated message; editable before commit.
            let vis = app.visible_indices();
            app.input = app.build_commit_message(&vis);
            app.commit_msg_buf.clear();
            app.mode = Mode::CommitMsg;
        }
        KeyCode::Char('E') => export_tasks(app),
        KeyCode::Char('I') => {
            app.input.clear();
            app.mode = Mode::Import;
        }
        KeyCode::Char('S') => {
            app.sort_mode = app.sort_mode.next();
            reset_selection(app);
            app.status_msg = format!("Sort: {}", app.sort_mode.label());
        }
        KeyCode::Char('g') => app.show_git_log = !app.show_git_log,
        KeyCode::Char('P') => {
            app.projects = list_projects();
            app.input.clear();
            app.mode = Mode::Projects;
        }
        KeyCode::Char('/') => {
            app.input = app.search_query.clone();
            app.mode = Mode::Search;
        }
        _ => {}
    }
    Ok(())
}

fn open_patch_browser(app: &mut App) {
    app.patches = crate::git::list_patches();
    app.patch_selected = 0;
    if app.patches.is_empty() {
        app.status_msg = "No saved patches.".to_string();
    } else {
        app.mode = Mode::PatchBrowser;
    }
}

fn export_tasks(app: &mut App) {
    let path = app.json_path();
    match export_to_json(&app.tasks, &path) {
        Ok(_) => app.status_msg = format!("Exported to {}", path),
        Err(e) => app.status_msg = format!("Export failed: {}", e),
    }
}

// Leave the TUI, run the configured test command, and commit or revert.
fn run_tests_and_commit(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<(), Box<dyn Error>> {
    if app.test_command.trim().is_empty() {
        // Never treat a missing command as a failing test — that used to
        // trigger `git restore .` and wipe the tree.
        app.status_msg = "No test command set. Press T to set one first.".to_string();
        return Ok(());
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    let passed = run_test_command(&app.test_command);
    if passed {
        app.save();
        let vis = app.visible_indices();
        let msg = app.build_commit_message(&vis);
        match commit_tasks(&msg) {
            Ok(_) => println!("Committed: {}", msg),
            Err(e) => println!("Not committed: {}", e),
        }
    } else {
        println!("Tests failed, reverting...");
        crate::git::revert_worktree();
    }
    println!("Press Enter to return...");
    let _ = io::stdin().read_line(&mut String::new());
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout());
    *terminal = Terminal::new(backend)?;
    app.invalidate_git_caches();
    Ok(())
}

// ─── Prompt-mode keys ────────────────────────────────────────────────────────

fn handle_search_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Enter | KeyCode::Esc => {
            app.search_query = std::mem::take(&mut app.input);
            reset_selection(app);
            app.mode = Mode::View;
        }
        _ if edit_line(&mut app.input, &key) => {
            // Live filter — update query immediately
            app.search_query = app.input.clone();
            reset_selection(app);
        }
        _ => {}
    }
}

fn handle_reload_prompt_key(app: &mut App, key: KeyEvent) {
    match key.code {
        // d → take the disk version; k / Esc / Enter → overwrite with ours
        KeyCode::Char('d') | KeyCode::Char('D') => {
            app.reload_from_disk();
            app.mode = Mode::View;
            app.status_msg = "Reloaded from disk. Press u to restore your version.".to_string();
        }
        KeyCode::Char('k') | KeyCode::Char('K') | KeyCode::Esc | KeyCode::Enter => {
            app.keep_my_version();
            app.mode = Mode::View;
            app.status_msg = "Kept your version.".to_string();
        }
        _ => {}
    }
}

fn handle_filter_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Enter | KeyCode::Esc => app.mode = Mode::View,
        _ if edit_line(&mut app.input, &key) => {
            // Live filter — update query immediately
            app.filter_query = app.input.clone();
            reset_selection(app);
        }
        _ => {}
    }
}

fn handle_commit_template_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Enter => {
            let tpl = std::mem::take(&mut app.input);
            app.set_commit_template(tpl.trim().to_string()); // persists to .grit.json
            app.mode = Mode::View;
            app.status_msg = "Commit template saved.".to_string();
        }
        KeyCode::Esc => {
            app.input.clear();
            app.mode = Mode::View;
        }
        _ => {
            edit_line(&mut app.input, &key);
        }
    }
}

fn handle_patch_browser_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.mode = Mode::View,
        KeyCode::Char('j') | KeyCode::Down => {
            if app.patch_selected + 1 < app.patches.len() {
                app.patch_selected += 1;
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.patch_selected = app.patch_selected.saturating_sub(1);
        }
        KeyCode::Enter => {
            if let Some(name) = app.patches.get(app.patch_selected).cloned() {
                match crate::git::apply_patch(&name) {
                    Ok(_) => {
                        app.status_msg = format!("Patch {} applied.", name);
                        app.invalidate_git_caches();
                    }
                    Err(e) => app.status_msg = e,
                }
                app.mode = Mode::View;
            }
        }
        KeyCode::Char('d') => {
            if let Some(name) = app.patches.get(app.patch_selected).cloned() {
                let _ = crate::git::delete_patch(&name);
                app.patches.remove(app.patch_selected);
                app.patch_selected = app.patch_selected.min(app.patches.len().saturating_sub(1));
                app.status_msg = format!("Patch {} deleted.", name);
                if app.patches.is_empty() {
                    app.mode = Mode::View;
                }
            }
        }
        _ => {}
    }
}

fn handle_add_task_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Enter => {
            if app.input.trim().is_empty() {
                app.status_msg = "Task description cannot be empty.".to_string();
                // Stay in Input mode so the user can keep typing
            } else {
                let text = std::mem::take(&mut app.input);
                if let Some(task) = Task::new(text) {
                    app.push_undo();
                    app.tasks.push(task);
                    app.save();
                    app.mode = Mode::View;
                }
            }
        }
        KeyCode::Esc => {
            app.input.clear();
            app.mode = Mode::View;
        }
        _ => {
            edit_line(&mut app.input, &key);
        }
    }
}

fn handle_edit_task_key(app: &mut App, key: KeyEvent) {
    let vis = app.visible_indices();
    match key.code {
        KeyCode::Enter => {
            if let Some(&real) = vis.get(app.selected) {
                if app.input.trim().is_empty() {
                    app.status_msg = "Description cannot be empty.".to_string();
                    // Stay in Edit mode so the text isn't lost
                } else {
                    let text = std::mem::take(&mut app.input);
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
                    }
                }
            }
        }
        KeyCode::Esc => {
            app.input.clear();
            app.mode = Mode::View;
        }
        _ => {
            edit_line(&mut app.input, &key);
        }
    }
}

fn handle_add_subtask_key(app: &mut App, key: KeyEvent) {
    let vis = app.visible_indices();
    match key.code {
        KeyCode::Enter => {
            // Add under whatever is focused: task → new subtask;
            // subtask → child of that subtask.
            let text = std::mem::take(&mut app.input);
            if app.add_subtask(&vis, &text) {
                app.mode = Mode::View;
            } else {
                // Blank text: keep it so the user can edit.
                app.input = text;
            }
        }
        KeyCode::Esc => {
            app.input.clear();
            app.mode = Mode::View;
        }
        _ => {
            edit_line(&mut app.input, &key);
        }
    }
}

fn handle_edit_subtask_key(app: &mut App, key: KeyEvent) {
    let vis = app.visible_indices();
    match key.code {
        KeyCode::Enter => {
            if matches!(app.focused, FocusedRow::Subtask(_, _)) {
                if app.input.trim().is_empty() {
                    app.status_msg = "Subtask description cannot be empty.".to_string();
                    // Stay in EditSubtask mode so the text isn't lost
                } else {
                    let text = std::mem::take(&mut app.input);
                    app.push_undo();
                    if let Some(sub) = app.focused_subtask_mut(&vis) {
                        sub.description = text.trim().to_string();
                    }
                    app.save();
                    app.mode = Mode::View;
                }
            }
        }
        KeyCode::Esc => {
            app.input.clear();
            app.mode = Mode::View;
        }
        _ => {
            edit_line(&mut app.input, &key);
        }
    }
}

fn handle_test_command_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Enter => {
            let cmd = std::mem::take(&mut app.input);
            app.set_test_command(cmd); // also persists to .grit.json
            app.mode = Mode::View;
        }
        KeyCode::Esc => {
            app.input.clear();
            app.mode = Mode::View;
        }
        _ => {
            edit_line(&mut app.input, &key);
        }
    }
}

fn handle_commit_msg_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Enter => {
            let msg = std::mem::take(&mut app.input);
            let msg = msg.trim().to_string();
            if !msg.is_empty() {
                app.save();
                match commit_tasks(&msg) {
                    Ok(_) => {
                        app.invalidate_git_caches();
                        app.status_msg = "Committed!".to_string();
                    }
                    Err(e) => app.status_msg = format!("Commit failed: {}", e),
                }
            }
            app.mode = Mode::View;
        }
        KeyCode::Esc => {
            app.input.clear();
            app.mode = Mode::View;
        }
        _ => {
            edit_line(&mut app.input, &key);
        }
    }
}

fn handle_projects_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.mode = Mode::View,
        KeyCode::Enter => {
            let name = std::mem::take(&mut app.input);
            let name = name.trim().to_string();
            if !name.is_empty() && !is_valid_project_name(&name) {
                app.status_msg = format!("Invalid project name: \"{}\" (no '/' or '\\')", name);
                app.mode = Mode::View;
            } else {
                app.load_project(&name);
                app.mode = Mode::View;
                app.status_msg = if name.is_empty() {
                    "Switched to default project.".to_string()
                } else {
                    format!("Switched to project: {}", name)
                };
            }
        }
        _ => {
            edit_line(&mut app.input, &key);
        }
    }
}

fn handle_import_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            app.input.clear();
            app.mode = Mode::View;
        }
        KeyCode::Enter => {
            // Empty input defaults to the current project's JSON file
            let path_raw = std::mem::take(&mut app.input);
            let path_raw = path_raw.trim().to_string();
            let path = if path_raw.is_empty() {
                app.json_path()
            } else {
                path_raw
            };
            app.input = path.clone();
            app.import_path = path;
            app.input.clear();
            app.mode = Mode::ImportMerge;
        }
        _ => {
            edit_line(&mut app.input, &key);
        }
    }
}

fn handle_import_merge_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            app.input.clear();
            app.mode = Mode::View;
        }
        KeyCode::Enter => {
            let strategy = std::mem::take(&mut app.input);
            let strategy = strategy.trim().to_lowercase();
            if strategy != "a" && strategy != "r" {
                app.status_msg = "Type 'a' to append or 'r' to replace, Esc to cancel.".to_string();
                app.input = strategy;
            } else {
                // Path was stashed by the Import prompt
                let path = app.import_path.clone();
                match app.import_tasks(&path, strategy == "r") {
                    Ok((added, skipped)) => {
                        app.status_msg = if strategy == "r" {
                            format!("Imported {} tasks (replaced).", added)
                        } else {
                            format!("Imported {} tasks (skipped {} duplicates).", added, skipped)
                        };
                        app.mode = Mode::View;
                    }
                    Err(e) => {
                        app.status_msg = e;
                        app.mode = Mode::Import;
                        app.input = path;
                    }
                }
            }
        }
        _ => {
            edit_line(&mut app.input, &key);
        }
    }
}
