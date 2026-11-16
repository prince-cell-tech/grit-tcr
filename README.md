# Rust TUI Task Manager

A terminal user interface (TUI) task manager built with Rust and ratatui, designed to manage tasks and subtasks efficiently from your terminal — with Git TCR integration, project switching, search, undo, and more.

---

## Features

### Task management
- Add, edit, and delete tasks entirely from the keyboard.
- Toggle task status in a three-way cycle: **Pending → Done → Working**.
- Cycle task **priority** (High / Medium / Low) with a single key — colour-coded red, yellow, and blue in the list.
- Attach **tags** (`#tag`), a **due date** (`due:YYYY-MM-DD`), and a **priority** (`p:high`, `p:medium`, `p:low`) inline when adding or editing a task. Overdue dates are highlighted in red.

### Subtasks
- Add one or more subtasks to any task (`s`).
- **Nesting:** focus a subtask and press `s` again to add a *child* under it — subtasks can nest to any depth.
- Subtasks are displayed inline below their parent with tree connectors (`├─` / `└─`), indented per level.
- Navigate into subtasks with `j`/`k` — the cursor moves through tasks and subtasks as a flat list.
- Toggle a subtask between `[ ]` and `[x]` with `Enter`, just like a top-level task.
- Edit (`e`) or delete (`d`) a subtask at any depth directly while it is focused.
- A `[done/total]` badge on tasks and parents shows subtask completion across the whole subtree at a glance.
- Subtask state (including nesting) is saved to disk and restored exactly on next launch. Older files with flat subtasks are read as-is.

### Undo
- Press `u` to undo the last change (delete, edit, status toggle, priority change, subtask add/delete).
- Press `Ctrl+r` to redo an undone change.
- Up to 50 undo/redo levels are kept in memory per session; any new change discards the redo history.

### Search, filter and sort
- Press `/` to open a live search bar — the list filters as you type by description or tag.
- Press `f` for a structured **filter**: `#tag`, `s:pending|working|done`, `p:high|medium|low`, plus plain description words. All conditions AND together; `Esc` clears the filter (then the search).
- Press `S` to cycle through sort modes: **Default → Status → Priority → Due Date → Created**.
- The active filter and sort mode are shown in the header bar / list title.

### Multiple projects
- Press `P` and type a project name to switch to a separate task list saved as `projects/<name>.md`.
- Leave the name blank to return to the default `tasks.md`.
- The `projects/` directory is only created when you first switch to a named project.

### Git and TCR integration
- **`t`** — runs your configured test command. If tests pass, saves and commits automatically. If tests fail, reverts all changes with `git restore .`. Running tests with no command configured does nothing (it never reverts).
- **`T`** — set or change the test command (e.g. `cargo test`, `npm test`). The command is run through your shell, so `&&`, pipes and quotes work. It is saved to `.grit.json` and **persists between sessions**.
- **`c`** — commit with a message, **prefilled from your commit template** (editable before committing).
- **`m`** — edit the commit-message template used by `t` and auto-TCR. Placeholders: `{task}` (focused task's description), `{status}`, `{date}`. Default: `TCR: {task}`. With no focused task, `t` falls back to `TCR: tests passed`.
- **`A`** — toggle **auto-TCR**: whenever the git working tree changes (you save files in your editor), tests run in the background — pass → automatic commit, fail → the diff is saved as a patch and the tree is reverted. Skipped on a clean tree, while you're typing, and with no test command set.
- **`r`** — open the **patch browser**: patches saved by failed auto-TCR runs. `Enter` re-applies a patch (`git apply`), `d` deletes it. Failed runs also show the last test output in a panel (`Esc` closes).
- Commits are **blocked on `main` and `master`** — if you are on either of those branches the commit is refused with a clear message, so you are never accidentally committing TCR changes to your main branch.
- Task files (`tasks.md`, `tasks.json`, `.grit.json`, `projects/`, `.grit/patches/`) are kept out of commits — only code changes are committed.
- The current git branch is displayed in the header bar at all times.
- Press `g` to toggle a panel showing the last four commits.

### Safe persistence
- All file writes (task lists, config, JSON export) go through **temp-file + atomic rename**, so a crash can never leave a truncated file behind.
- If `tasks.md` is edited outside the app while it's running, grit detects it and asks: **`d`** reload from disk (your in-memory version stays recoverable with `u`) or **`k`** keep your version and overwrite.

### Persistence and export
- Tasks are saved to `tasks.md` (or `projects/<name>.md`) in human-readable Markdown, grouped by status (Working, Pending, Done).
- The file is only created when you actually add a task — launching the app in an empty directory creates no files.
- Press `E` to export the current task list to `tasks.json` (or `projects/<name>.json` when a project is active).
- Press `I` to import tasks from `tasks.json` (or `projects/<name>.json` when a project is active) — type a custom path to import a different file. You'll be asked how to merge: **`a`** appends to the current list, skipping duplicates (same description + created date), **`r`** replaces the current list. The whole import is a single undo step, so `u` reverses it.

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
| `G` | Jump to bottom (deepest row) |
| `H` | Jump to top |
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
| `s` | Add subtask to focused task — or a child under a focused subtask |
| `u` | Undo last action |
| `Ctrl+r` | Redo |

### View and filter
| Key | Action |
|---|---|
| `/` | Search / live filter |
| `f` | Structured filter (`#tag`, `s:status`, `p:priority`, words) |
| `Esc` | Clear TCR panel → filter → search |
| `S` | Cycle sort mode |
| `g` | Toggle git log panel |
| `P` | Switch project |

### Git / TCR
| Key | Action |
|---|---|
| `T` | Set test command |
| `m` | Edit commit-message template |
| `t` | Run tests → commit on pass, revert on fail |
| `c` | Custom commit message (prefilled from template) |
| `A` | Toggle auto-TCR (test+commit/revert on file changes) |
| `r` | Patch browser — re-apply or delete revert patches |

### Export
| Key | Action |
|---|---|
| `E` | Export tasks to `tasks.json` |
| `I` | Import tasks from `tasks.json` (`a` = append with dedupe, `r` = replace) |

---

## Configuration

`.grit.json` (created next to your tasks, in the repo root):

```json
{
  "test_command": "cargo test",
  "commit_template": "TCR: {task}",
  "auto_tcr": false
}
```

| Field | Meaning |
|---|---|
| `test_command` | Shell command run by `t` and auto-TCR (`T` sets it) |
| `commit_template` | Message template with `{task}`, `{status}`, `{date}` placeholders (`m` sets it) |
| `auto_tcr` | Whether auto-TCR runs on working-tree changes (`A` toggles it) |

All fields are optional — missing or corrupt files fall back to defaults.

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
