mod app;
mod git;
mod storage;
mod task;
mod tcr;
#[cfg(test)]
mod testutil;
mod ui;

use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::error::Error;
use std::io::stdout;
use std::time::Duration;

const USAGE: &str = "\
grit-tcr — test && commit || revert

USAGE:
    grit-tcr                 task manager TUI, with TCR built in
    grit-tcr watch           TCR in the terminal: no UI, just the loop
    grit-tcr --help

WATCH OPTIONS:
    -c, --command <CMD>      test command (default: test_command in .grit.json)
    -i, --interval <MS>      how often to check the tree (default: 500)
    -n, --settle <N>         stable checks before testing (default: 2)
    -t, --tasks <FILE>       task list to name commits after (default: tasks.md)
        --once               run one cycle, then exit

On every settled change: tests pass → commit, tests fail → the diff is saved
to .grit/patches/ and the tree is reverted. Commits are refused on main and
master, so work on a feature branch.

Commit messages follow your task list: {task} in the commit template becomes
the task marked Working, or the first one still Pending. Re-read every cycle,
so moving to the next task renames the commits without a restart.
";

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => run_tui(),
        Some("watch") => {
            let opts = parse_watch_args(&args[1..])?;
            if let Err(e) = tcr::watch(opts) {
                eprintln!("grit-tcr: {e}");
                std::process::exit(1);
            }
            Ok(())
        }
        Some("help") | Some("-h") | Some("--help") => {
            print!("{USAGE}");
            Ok(())
        }
        Some("-V") | Some("--version") => {
            println!("grit-tcr {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(other) => {
            eprintln!("grit-tcr: unknown argument '{other}'\n\n{USAGE}");
            std::process::exit(2);
        }
    }
}

// The value that follows a flag, e.g. the "cargo test" in `--command "cargo test"`.
fn value(args: &[String], i: usize, name: &str) -> Result<String, String> {
    args.get(i + 1)
        .cloned()
        .ok_or_else(|| format!("{name} needs a value"))
}

fn parse_watch_args(args: &[String]) -> Result<tcr::WatchOptions, String> {
    let mut opts = tcr::WatchOptions::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-c" | "--command" => {
                opts.command = value(args, i, "--command")?;
                i += 2;
            }
            "-i" | "--interval" => {
                let ms: u64 = value(args, i, "--interval")?
                    .parse()
                    .map_err(|_| "--interval takes a whole number of milliseconds".to_string())?;
                opts.interval = Duration::from_millis(ms.max(50));
                i += 2;
            }
            "-n" | "--settle" => {
                opts.settle = value(args, i, "--settle")?
                    .parse()
                    .map_err(|_| "--settle takes a whole number".to_string())?;
                i += 2;
            }
            "-t" | "--tasks" => {
                opts.tasks_file = value(args, i, "--tasks")?;
                i += 2;
            }
            "--once" => {
                opts.once = true;
                i += 1;
            }
            other => return Err(format!("unknown watch option '{other}'\n\n{USAGE}")),
        }
    }
    Ok(opts)
}

fn run_tui() -> Result<(), Box<dyn Error>> {
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = ui::run_app(&mut terminal);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("Error: {:?}", err);
    }
    Ok(())
}
