# Rust TUI Task Manager

A terminal user interface (TUI) task manager built with Rust and tui-rs, designed to manage tasks and subtasks efficiently from your terminal — with Git TCR integration, project switching, search, undo, and more.

---

## Features

### Task management
- Add, edit, and delete tasks entirely from the keyboard.
- Toggle task status in a three-way cycle: **Pending → Done → Working**.
- Cycle task **priority** (High / Medium / Low) with a single key — colour-coded red, yellow, and blue in the list.
- Attach **tags** (`#tag`), a **due date** (`due:YYYY-MM-DD`), and a **priority** (`p:high`, `p:medium`, `p:low`) inline when adding or editing a task. Overdue dates are highlighted in red.

### Subtasks
- Add one or more subtasks to any task (`s`).
- Subtasks are displayed inline below their parent with tree connectors (`├─` / `└─`).
- Navigate into subtasks with `j`/`k` — the cursor moves through tasks and subtasks as a flat list.
- Toggle a subtask between `[ ]` and `[x]` with `Enter`, just like a top-level task.
- Edit (`e`) or delete (`d`) a subtask directly while it is focused.
- A `[done/total]` badge on the task header shows subtask completion at a glance.
- Subtask state is saved to disk and restored exactly on next launch.

### Undo
- Press `u` to undo the last change (delete, edit, status toggle, priority change, subtask add/delete).
- Up to 50 undo levels are kept in memory per session.

### Search and filter
- Press `/` to open a live search bar — the list filters as you type by description or tag.
- Press `Esc` to clear the search and return to the full list.

### Sorting
- Press `S` to cycle through sort modes: **Default → Status → Priority → Due Date → Created**.
- The active sort mode is shown in the header bar.

### Multiple projects
- Press `P` and type a project name to switch to a separate task list saved as `projects/<name>.md`.
- Leave the name blank to return to the default `tasks.md`.
- The `projects/` directory is only created when you first switch to a named project.

### Git and TCR integration
- **`t`** — runs your configured test command. If tests pass, saves and commits automatically. If tests fail, reverts all changes with `git restore .`.
- **`T`** — set or change the test command (e.g. `cargo test`, `npm test`).
- **`c`** — write a custom commit message and commit immediately.
- Commits are **blocked on `main` and `master`** — if you are on either of those branches the commit is refused with a clear message, so you are never accidentally committing TCR changes to your main branch.
- The current git branch is displayed in the header bar at all times.
- Press `g` to toggle a panel showing the last four commits.

### Persistence and export
- Tasks are saved to `tasks.md` (or `projects/<name>.md`) in human-readable Markdown, grouped by status (Working, Pending, Done).
- The file is only created when you actually add a task — launching the app in an empty directory creates no files.
- Press `E` to export the current task list to `tasks.json`.

### UI
- Header bar showing project name, git branch, sort mode, and task counts (pending / working / done).
- Status bar at the bottom with ephemeral feedback messages (e.g. "Task deleted. Press u to undo.").
- Scrolling list — the cursor always stays in view regardless of list length.
- Press `?` to open a full keybinding reference overlay.

---

## Task syntax

When adding or editing a task, metadata is parsed directly from the description line:

```
Fix login bug #bug #auth due:2025-06-01 p:high
```

| Token | Meaning |
|---|---|
| `#tag` | Attaches a tag (multiple allowed) |
| `due:YYYY-MM-DD` | Sets a due date |
| `p:high` / `p:medium` / `p:low` | Sets priority |

---

## Keybindings

### Navigation
| Key | Action |
|---|---|
| `j` / `↓` | Move down (through tasks and subtasks) |
| `k` / `↑` | Move up |
| `?` | Toggle help overlay |
| `q` | Quit |

### Task management
| Key | Action |
|---|---|
| `a` | Add new task |
| `e` | Edit focused task or subtask |
| `d` | Delete focused task or subtask |
| `Enter` | Toggle status (task) or toggle done (subtask) |
| `p` | Cycle priority (High → Medium → Low → High) |
| `s` | Add subtask to focused task |
| `u` | Undo last action |

### View and filter
| Key | Action |
|---|---|
| `/` | Search / live filter |
| `Esc` | Clear search |
| `S` | Cycle sort mode |
| `g` | Toggle git log panel |
| `P` | Switch project |

### Git / TCR
| Key | Action |
|---|---|
| `T` | Set test command |
| `t` | Run tests → commit on pass, revert on fail |
| `c` | Custom commit message and commit |

### Export
| Key | Action |
|---|---|
| `E` | Export tasks to `tasks.json` |

---

## Getting Started

1. Clone this repository.
2. Install Rust and Cargo if you haven't already.
3. Run `cargo run` to start the app.
4. Press `a` to add your first task, `?` to see all keybindings.

---

## Advantages

- **Reliability:** Rust's compile-time checks minimise bugs and crashes.
- **Performance:** Native execution ensures a fast and responsive UI.
- **Portability:** Runs on any machine with Rust and a terminal.
- **Human-readable storage:** Tasks live in plain Markdown files you can read and edit outside the app.
- **Safe Git workflow:** TCR commits are blocked on `main`/`master` by design.
- **Keyboard-first:** Every action is reachable without a mouse.

---

Feel free to contribute or report issues!
