// ─── UI rendering & event handling ───────────────────────────────────────────

use crate::app::{App, FocusedRow, Mode};
use crate::git::{commit_tasks, run_test_command};
use crate::storage::{export_to_json, is_valid_project_name, list_projects};
use crate::task::{current_date, is_overdue, Priority, Status, SubStatus, Subtask, Task};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::error::Error;
use std::io::{self, stdout};
use std::process::Command;
use std::time::Duration;

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

pub fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<(), Box<dyn Error>> {
    let mut app = App::new();

    loop {
        terminal.draw(|f| {
            let size = f.area();

            // Layout: header | task list | [git log] | input | status bar
            let git_log_height = if app.show_git_log { 6u16 } else { 0 };
            let input_height: u16 = match app.mode {
                Mode::Input | Mode::Edit | Mode::EditSubtask | Mode::Test | Mode::Search | Mode::Subtask | Mode::CommitMsg | Mode::Import | Mode::ImportMerge => 3,
                _ => 0,
            };

            // Adjust scroll so selected row stays visible.
            // The list area accounts for the margin, header, status bar and any open panels.
            let vis = app.visible_indices();
            let flat_row = app.focused_to_row_index(&vis);
            let list_height = size.height.saturating_sub(5 + git_log_height + input_height) as usize;
            if flat_row >= app.scroll_offset + list_height {
                app.scroll_offset = flat_row + 1 - list_height;
            }
            if flat_row < app.scroll_offset {
                app.scroll_offset = flat_row;
            }

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
            let branch = app.cached_branch();
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
                    Line::from(Span::styled(" Navigation", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
                    Line::from(" j/↓  move down          k/↑  move up"),
                    Line::from(" G    jump to bottom      H    jump to top"),
                    Line::from(" Enter toggle status      d    delete task"),
                    Line::from(""),
                    Line::from(Span::styled(" Task management", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
                    Line::from(" a    add task            e    edit task / subtask"),
                    Line::from(" s    add subtask         u    undo"),
                    Line::from(" p    cycle priority      d    delete task / subtask"),
                    Line::from(" Ctrl+r redo"),
                    Line::from(""),
                    Line::from(Span::styled(" Subtasks", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
                    Line::from(" j/k  navigate into subtasks (they appear below their task)"),
                    Line::from(" Enter toggle subtask [ ] / [x]   e  edit subtask text"),
                    Line::from(" s    on a subtask: add a child under it (nesting)"),
                    Line::from(""),
                    Line::from(Span::styled(" View / filter", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
                    Line::from(" /    search              Esc  clear search"),
                    Line::from(" S    cycle sort           g    toggle git log"),
                    Line::from(" P    switch project"),
                    Line::from(""),
                    Line::from(Span::styled(" TCR / git", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
                    Line::from(" T    set test command    t    run test+commit"),
                    Line::from(" c    custom commit msg"),
                    Line::from(""),
                    Line::from(Span::styled(" Import / export / quit", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
                    Line::from(" I    import from JSON    E    export to JSON"),
                    Line::from(" q    quit                ?    toggle this help"),
                    Line::from(""),
                    Line::from(Span::styled(" Task syntax when adding/editing:", Style::default().fg(Color::Cyan))),
                    Line::from("  #tag  due:YYYY-MM-DD  p:high / p:medium / p:low"),
                    Line::from("  Example: Fix login bug #bug due:2025-06-01 p:high"),
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
                    let today = current_date();
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

                        let overdue = task.due_date.as_deref().map(|d| is_overdue(d, &today)).unwrap_or(false)
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
                            let total: usize = task.subtasks.iter().map(|s| s.count_total()).sum();
                            let done: usize = task.subtasks.iter().map(|s| s.count_done()).sum();
                            spans.push(Span::styled(
                                format!(" [{}/{}]", done, total),
                                Style::default().fg(Color::DarkGray),
                            ));
                        }

                        items.push(ListItem::new(Line::from(spans)));

                        // Render the subtask tree recursively, with per-depth
                        // indent and tree connectors.
                        let mut rendered: Vec<ListItem> = vec![];
                        render_subtask_tree(
                            &task.subtasks, &[], &[],
                            app.focused.clone(), display_i,
                            &mut rendered,
                        );
                        items.extend(rendered);
                    }
                    items
                };

                let tasks_list = List::new(task_items)
                    .block(Block::default().title(title).borders(Borders::ALL));
                f.render_widget(tasks_list, chunks[1]);
            }

            // ── Git log panel ──
            if app.show_git_log && git_log_height > 0 {
                let log_entries = app.cached_git_log();
                let log_lines: Vec<Line> = log_entries.iter().map(|l| Line::from(l.as_str())).collect();
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
                    Mode::Subtask => ("Add subtask / child", " Enter to confirm, Esc to cancel"),
                    Mode::CommitMsg => ("Commit message", " Enter to commit, Esc to cancel"),
                    Mode::Import => ("Import tasks", " path to tasks.json (blank = this project's JSON), Enter to continue"),
                    Mode::ImportMerge => ("Import tasks", " how to merge? a = append (dedupe), r = replace, Esc to cancel"),
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
        })?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                // Ignore key-release events (some terminals send them)
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                // Clear ephemeral status on any key
                app.status_msg.clear();

                // Ctrl+C always quits, from any mode
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                    break;
                }

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
                                        FocusedRow::Subtask(di, path) => {
                                            let real = vis[di];
                                            app.push_undo();
                                            // Remove via the parent path; focus moves
                                            // back to the parent subtask (or task).
                                            let parent = path[..path.len() - 1].to_vec();
                                            let idx = path[path.len() - 1];
                                            {
                                                let task = &mut app.tasks[real];
                                                match crate::task::subtask_mut_at(task, &parent) {
                                                    Some(p) => { let _ = p.subtasks.remove(idx); }
                                                    None => { let _ = task.subtasks.remove(idx); }
                                                }
                                            }
                                            app.focused = if parent.is_empty() {
                                                FocusedRow::Task(di)
                                            } else {
                                                let siblings = crate::task::subtask_at(&app.tasks[real], &parent)
                                                    .map(|p| p.subtasks.len())
                                                    .unwrap_or(0);
                                                if siblings == 0 {
                                                    // Parent now childless → focus the parent itself
                                                    FocusedRow::Subtask(di, parent)
                                                } else {
                                                    FocusedRow::Subtask(di, {
                                                        let mut p = parent.clone();
                                                        p.push(idx.min(siblings - 1));
                                                        p
                                                    })
                                                }
                                            };
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
                            (KeyModifiers::CONTROL, KeyCode::Char('r')) => {
                                app.redo();
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
                                    FocusedRow::Subtask(_, _) => {
                                        if let Some(sub) = app.focused_subtask(&vis) {
                                            app.input = sub.description.clone();
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
                                if app.test_command.trim().is_empty() {
                                    // Never treat a missing command as a failing test —
                                    // that used to trigger `git restore .` and wipe the tree.
                                    app.status_msg = "No test command set. Press T to set one first.".to_string();
                                } else {
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
                                    app.invalidate_git_caches();
                                }
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
                                    FocusedRow::Subtask(_, _) => {
                                        app.push_undo();
                                        if let Some(sub) = app.focused_subtask_mut(&vis) {
                                            sub.status = match sub.status {
                                                SubStatus::Pending => SubStatus::Done,
                                                SubStatus::Done => SubStatus::Pending,
                                            };
                                        }
                                        app.save();
                                    }
                                }
                            }
                            (_, KeyCode::Char('E')) => {
                                let path = app.json_path();
                                match export_to_json(&app.tasks, &path) {
                                    Ok(_) => app.status_msg = format!("Exported to {}", path),
                                    Err(e) => app.status_msg = format!("Export failed: {}", e),
                                }
                            }
                            (_, KeyCode::Char('I')) => {
                                app.input.clear();
                                app.mode = Mode::Import;
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
                            (_, KeyCode::Char('G')) => {
                                let vis2 = app.visible_indices();
                                app.jump_bottom(&vis2);
                            }
                            (_, KeyCode::Char('H')) => {
                                app.jump_top();
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
                            app.focused = FocusedRow::Task(0);
                            app.mode = Mode::View;
                        }
                        KeyCode::Char(c) => {
                            app.input.push(c);
                            // Live filter — update query immediately
                            app.search_query = app.input.clone();
                            app.selected = 0;
                            app.focused = FocusedRow::Task(0);
                        }
                        KeyCode::Backspace => {
                            app.input.pop();
                            app.search_query = app.input.clone();
                            app.selected = 0;
                            app.focused = FocusedRow::Task(0);
                        }
                        _ => {}
                    },

                    Mode::Input => match key.code {
                        KeyCode::Enter => {
                            if app.input.trim().is_empty() {
                                app.status_msg = "Task description cannot be empty.".to_string();
                                // Stay in Input mode so the user can keep typing
                            } else {
                                let text: String = app.input.drain(..).collect();
                                if let Some(task) = Task::new(text) {
                                    app.push_undo();
                                    app.tasks.push(task);
                                    app.save();
                                    app.mode = Mode::View;
                                }
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
                                    if app.input.trim().is_empty() {
                                        app.status_msg = "Description cannot be empty.".to_string();
                                        // Stay in Edit mode so the text isn't lost
                                    } else {
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
                                        }
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
                                // Add a subtask under whatever is focused:
                                // task → new subtask; subtask → child of that subtask.
                                let parent_di = match &app.focused {
                                    FocusedRow::Task(di) => *di,
                                    FocusedRow::Subtask(di, _) => *di,
                                };
                                if app.input.trim().is_empty() {
                                    app.status_msg = "Subtask description cannot be empty.".to_string();
                                    // Stay in Subtask mode so the user can keep typing
                                } else if let Some(&real) = vis.get(parent_di) {
                                    let text: String = app.input.drain(..).collect();
                                    if let Some(sub) = Subtask::new(text) {
                                        app.push_undo();
                                        let parent_path: Vec<usize> = match &app.focused {
                                            FocusedRow::Task(_) => vec![],
                                            FocusedRow::Subtask(_, path) => path.clone(),
                                        };
                                        match parent_path.as_slice() {
                                            [] => app.tasks[real].subtasks.push(sub),
                                            p => {
                                                if let Some(parent) = crate::task::subtask_mut_at(&mut app.tasks[real], p) {
                                                    parent.subtasks.push(sub);
                                                } else {
                                                    app.tasks[real].subtasks.push(sub);
                                                }
                                            }
                                        }
                                        app.save();
                                        app.status_msg = "Subtask added.".to_string();
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

                    Mode::EditSubtask => {
                        let vis = app.visible_indices();
                        match key.code {
                            KeyCode::Enter => {
                                if matches!(app.focused, FocusedRow::Subtask(_, _)) {
                                    if app.input.trim().is_empty() {
                                        app.status_msg = "Subtask description cannot be empty.".to_string();
                                        // Stay in EditSubtask mode so the text isn't lost
                                    } else {
                                        let text: String = app.input.drain(..).collect();
                                        app.push_undo();
                                        if let Some(sub) = app.focused_subtask_mut(&vis) {
                                            sub.description = text.trim().to_string();
                                        }
                                        app.save();
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

                    Mode::Test => match key.code {
                        KeyCode::Enter => {
                            let cmd: String = app.input.drain(..).collect();
                            app.set_test_command(cmd); // also persists to .grit.json
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
                                    Ok(_) => {
                                        app.invalidate_git_caches();
                                        app.status_msg = "Committed!".to_string();
                                    }
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
                        KeyCode::Char(c) => app.input.push(c),
                        KeyCode::Backspace => { app.input.pop(); }
                        _ => {}
                    },

                    Mode::Import => match key.code {
                        KeyCode::Esc => { app.input.clear(); app.mode = Mode::View; }
                        KeyCode::Enter => {
                            // Empty input defaults to the current project's JSON file
                            let path_raw: String = app.input.drain(..).collect();
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
                        KeyCode::Char(c) => app.input.push(c),
                        KeyCode::Backspace => { app.input.pop(); }
                        _ => {}
                    },

                    Mode::ImportMerge => match key.code {
                        KeyCode::Esc => { app.input.clear(); app.mode = Mode::View; }
                        KeyCode::Enter => {
                            let strategy: String = app.input.drain(..).collect();
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
