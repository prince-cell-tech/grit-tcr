// ─── TCR loop ─────────────────────────────────────────────────────────────────
//
// The parts of test && commit || revert that don't need a UI: deciding when a
// working-tree change has settled, running one cycle, and the headless
// `grit-tcr watch` loop. The TUI uses the same pieces, so both paths behave
// identically.

use crate::git;
use crate::storage::load_config;
use std::path::PathBuf;
use std::time::Duration;

// Decides when a change to the working tree has stopped moving.
//
// An editor save is not one atomic event: formatters rewrite the file, a
// multi-file save lands over several hundred milliseconds, build tools drop
// output. Firing on the first difference seen would run the tests against a
// half-written tree — and on failure revert the saves that hadn't landed yet.
// So a change has to look *identical* for `required` consecutive observations
// before it counts as real.
#[derive(Debug)]
pub struct Settle {
    baseline: Option<String>, // last state accepted (None = nothing seen yet)
    pending: Option<String>,  // a change waiting to hold still
    repeats: u32,             // consecutive observations of `pending`
    required: u32,            // observations needed before firing
}

impl Settle {
    pub fn new(required: u32) -> Settle {
        Settle { baseline: None, pending: None, repeats: 0, required: required.max(1) }
    }

    // Forget everything, so the next observation is only a baseline. Used when
    // auto-TCR is switched on, so it never fires on dirt that was already there.
    pub fn reset(&mut self) {
        self.baseline = None;
        self.pending = None;
        self.repeats = 0;
    }

    // Accept a state without firing — after our own commit or revert changed
    // the tree, which must not read as a fresh edit.
    pub fn accept(&mut self, sig: Option<String>) {
        self.baseline = sig;
        self.pending = None;
        self.repeats = 0;
    }

    #[cfg(test)]
    pub fn baseline(&self) -> Option<&str> {
        self.baseline.as_deref()
    }

    // Record the tree state just observed. True = the change has settled, run
    // the tests now.
    pub fn observe(&mut self, sig: &str) -> bool {
        // First sighting is only a reference point.
        if self.baseline.is_none() {
            self.accept(Some(sig.to_string()));
            return false;
        }
        // Unchanged, or back to the accepted state.
        if self.baseline.as_deref() == Some(sig) {
            self.pending = None;
            self.repeats = 0;
            return false;
        }
        if self.pending.as_deref() == Some(sig) {
            self.repeats += 1;
        } else {
            self.pending = Some(sig.to_string());
            self.repeats = 1;
        }
        if self.repeats < self.required {
            return false;
        }
        self.accept(Some(sig.to_string()));
        // A tree that settled on *clean* has nothing to test.
        !sig.trim().is_empty()
    }
}

// What one TCR cycle did.
#[derive(Debug)]
pub enum Outcome {
    Committed(String),
    // Tests passed but the commit was refused (e.g. on main/master).
    NotCommitted(String),
    Reverted { patch: Option<PathBuf>, output: String },
    // Nothing was reverted because there was nothing tracked to revert.
    FailedCleanTree { output: String },
}

// Run the tests once and commit or revert. The caller owns change detection.
pub fn run_cycle(command: &str, message: &str) -> Outcome {
    let (passed, output) = git::run_test_command_captured(command);
    if passed {
        return match git::commit_tasks(message) {
            Ok(_) => Outcome::Committed(message.to_string()),
            Err(e) => Outcome::NotCommitted(e),
        };
    }
    let _ = git::save_failure_log(&output);
    // Capture BEFORE reverting, so nothing is truly lost.
    let diff = git::git_diff_all();
    if diff.trim().is_empty() {
        git::revert_worktree();
        return Outcome::FailedCleanTree { output };
    }
    let patch = git::save_patch(&diff).ok();
    git::revert_worktree();
    Outcome::Reverted { patch, output }
}

// The task a commit should be named after. In the TUI that's whichever task
// the cursor is on; with no UI the list itself has to say, so: the task marked
// Working, else the first one still Pending. Done tasks are never chosen —
// finished work doesn't describe what you're committing now.
pub fn current_task(tasks_file: &str) -> Option<crate::task::Task> {
    let tasks = crate::storage::load_tasks_from(tasks_file);
    tasks
        .iter()
        .find(|t| t.status == crate::task::Status::Working)
        .or_else(|| tasks.iter().find(|t| t.status == crate::task::Status::Pending))
        .cloned()
}

// Commit message for a run with no UI. Read fresh every cycle, so marking a
// different task as Working changes the next commit without a restart.
pub fn watch_commit_message(template: &str, tasks_file: &str) -> String {
    let tpl = if template.trim().is_empty() { "TCR: {task}" } else { template };
    let (task, status) = match current_task(tasks_file) {
        // Same fallback wording the TUI uses with nothing focused.
        None => ("tests passed".to_string(), String::new()),
        Some(t) => (t.description.clone(), t.status.label().to_string()),
    };
    tpl.replace("{task}", &task)
        .replace("{status}", &status)
        .replace("{date}", &crate::task::current_date())
        .trim()
        .to_string()
}

pub struct WatchOptions {
    pub command: String,
    pub interval: Duration,
    pub settle: u32,
    // Exit after the first completed cycle — useful in scripts and CI.
    pub once: bool,
    // Task list the commit message is named after; a project file works too.
    pub tasks_file: String,
}

impl Default for WatchOptions {
    fn default() -> Self {
        WatchOptions {
            command: String::new(),
            interval: Duration::from_millis(500),
            settle: 2,
            once: false,
            tasks_file: crate::storage::TASKS_FILE.to_string(),
        }
    }
}

// The headless watcher: no TUI, no task list, just the TCR loop on stdout.
// Runs until interrupted (Ctrl-C), or until one cycle finishes with `once`.
pub fn watch(opts: WatchOptions) -> Result<(), String> {
    if !git::in_repo() {
        return Err("not inside a git repository — run grit-tcr watch from your project root".into());
    }
    let cfg = load_config();
    let command = if opts.command.trim().is_empty() { cfg.test_command.clone() } else { opts.command.clone() };
    if command.trim().is_empty() {
        return Err(
            "no test command: pass --command \"cargo test\", or set one in .grit.json (the TUI's T key)".into(),
        );
    }
    match current_task(&opts.tasks_file) {
        Some(t) => println!("tracking task: {} [{}]", t.description, t.status.label()),
        None => println!(
            "no task in {} is working or pending — commits will say \"tests passed\"",
            opts.tasks_file
        ),
    }

    let branch = git::git_branch();
    if branch == "main" || branch == "master" {
        println!("! on '{branch}' — commits are refused there. Switch to a feature branch, or passing tests won't be committed.");
    }
    println!("grit-tcr watching. test: {command}");
    println!("  pass → commit    fail → revert (diff saved to {})", git::PATCHES_DIR);
    println!("  every {}ms, after {} stable checks. Ctrl-C to stop.", opts.interval.as_millis(), opts.settle);

    let mut settle = Settle::new(opts.settle);
    loop {
        std::thread::sleep(opts.interval);
        let Some(sig) = git::working_tree_signature() else { continue };
        if !settle.observe(&sig) {
            continue;
        }
        println!("\n— change settled, running tests…");
        // Re-read per cycle: the task you're on changes as you work.
        let message = watch_commit_message(&cfg.commit_template, &opts.tasks_file);
        match run_cycle(&command, &message) {
            Outcome::Committed(msg) => println!("  ✓ passed → committed \"{msg}\""),
            Outcome::NotCommitted(e) => println!("  ✓ passed, but not committed: {e}"),
            Outcome::Reverted { patch, output } => {
                print_tail(&output);
                match patch {
                    Some(p) => println!("  ✗ failed → reverted. recover: git apply {}", p.display()),
                    None => println!("  ✗ failed → reverted."),
                }
            }
            Outcome::FailedCleanTree { output } => {
                print_tail(&output);
                println!("  ✗ failed, but nothing tracked to revert.");
            }
        }
        // Our own commit/revert changed the tree; that isn't a new edit.
        settle.accept(git::working_tree_signature());
        if opts.once {
            return Ok(());
        }
    }
}

fn print_tail(output: &str) {
    for line in output.lines().rev().take(15).collect::<Vec<_>>().into_iter().rev() {
        println!("  | {line}");
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestEnv;
    use std::process::Command;

    fn init_repo(branch: &str) {
        for args in [
            vec!["init", "-q", "-b", branch],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
        ] {
            assert!(Command::new("git").args(&args).status().unwrap().success());
        }
    }

    fn commit_file(name: &str, content: &str) {
        std::fs::write(name, content).unwrap();
        assert!(Command::new("git").args(["add", name]).status().unwrap().success());
        assert!(Command::new("git").args(["commit", "-q", "-m", "init"]).status().unwrap().success());
    }

    #[test]
    fn first_observation_is_only_a_baseline() {
        let mut s = Settle::new(2);
        assert!(!s.observe(" M code.txt\n"), "never fire on the state we started in");
        assert_eq!(s.baseline(), Some(" M code.txt\n"));
    }

    #[test]
    fn a_change_must_hold_still_before_it_fires() {
        let mut s = Settle::new(2);
        s.observe(""); // baseline: clean

        // A save in progress: the tree looks different each time it's checked.
        assert!(!s.observe(" M a.txt\n"), "first sighting of a change only arms it");
        assert!(!s.observe(" M a.txt\n M b.txt\n"), "still moving — must not fire");
        assert!(!s.observe(" M a.txt\n M b.txt\n M c.txt\n"), "still moving");
        // It stopped moving.
        assert!(s.observe(" M a.txt\n M b.txt\n M c.txt\n"), "settled → fire once");
        assert!(!s.observe(" M a.txt\n M b.txt\n M c.txt\n"), "and never twice for the same state");
    }

    #[test]
    fn settling_back_to_clean_does_not_fire() {
        let mut s = Settle::new(2);
        s.observe(""); // baseline: clean
        s.observe(" M a.txt\n");
        // Undone in the editor before the tests ever ran.
        assert!(!s.observe(""));
        assert!(!s.observe(""), "a clean tree is never worth testing");
    }

    #[test]
    fn required_one_fires_immediately_on_a_change() {
        let mut s = Settle::new(1);
        s.observe("");
        assert!(s.observe(" M a.txt\n"));
    }

    #[test]
    fn reset_forces_a_fresh_baseline_and_accept_absorbs_our_own_writes() {
        let mut s = Settle::new(1);
        s.observe("");
        s.reset();
        assert!(!s.observe(" M dirty.txt\n"), "after reset, pre-existing dirt is the baseline");

        s.accept(Some(" M committed.txt\n".to_string()));
        assert!(!s.observe(" M committed.txt\n"), "our own commit/revert is not an edit");
    }

    const TASKS: &str = crate::storage::TASKS_FILE;

    fn write_tasks(specs: &[(&str, crate::task::Status)]) {
        let tasks: Vec<crate::task::Task> = specs
            .iter()
            .map(|(d, s)| {
                let mut t = crate::task::Task::new(d.to_string()).unwrap();
                t.status = s.clone();
                t
            })
            .collect();
        crate::storage::save_tasks_to(&tasks, TASKS).unwrap();
    }

    #[test]
    fn commit_message_falls_back_when_there_is_no_open_task() {
        let _env = TestEnv::new("grit-test-tcr-msg-none");
        assert_eq!(watch_commit_message("", TASKS), "TCR: tests passed");
        assert_eq!(watch_commit_message("TCR: {task}", TASKS), "TCR: tests passed");
        assert_eq!(watch_commit_message("green", TASKS), "green");

        // A list with nothing left to do is the same as no list.
        write_tasks(&[("shipped it", crate::task::Status::Done)]);
        assert_eq!(watch_commit_message("TCR: {task}", TASKS), "TCR: tests passed");
    }

    #[test]
    fn commit_message_names_the_working_task() {
        let _env = TestEnv::new("grit-test-tcr-msg-working");
        write_tasks(&[
            ("write the parser", crate::task::Status::Pending),
            ("fix the lexer", crate::task::Status::Working),
            ("old news", crate::task::Status::Done),
        ]);
        assert_eq!(watch_commit_message("TCR: {task}", TASKS), "TCR: fix the lexer");
        assert_eq!(
            watch_commit_message("{task} ({status})", TASKS),
            "fix the lexer (working)"
        );
    }

    #[test]
    fn commit_message_falls_back_to_the_first_pending_task() {
        let _env = TestEnv::new("grit-test-tcr-msg-pending");
        write_tasks(&[
            ("write the parser", crate::task::Status::Pending),
            ("later thing", crate::task::Status::Pending),
        ]);
        assert_eq!(watch_commit_message("TCR: {task}", TASKS), "TCR: write the parser");
    }

    #[test]
    fn commit_message_follows_the_task_you_switch_to() {
        let _env = TestEnv::new("grit-test-tcr-msg-switch");
        write_tasks(&[
            ("first thing", crate::task::Status::Working),
            ("second thing", crate::task::Status::Pending),
        ]);
        assert_eq!(watch_commit_message("TCR: {task}", TASKS), "TCR: first thing");

        // Marked done in the TUI, moved on to the next one.
        write_tasks(&[
            ("first thing", crate::task::Status::Done),
            ("second thing", crate::task::Status::Working),
        ]);
        assert_eq!(
            watch_commit_message("TCR: {task}", TASKS),
            "TCR: second thing",
            "the message must be re-read, not cached at startup"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_cycle_commits_when_tests_pass() {
        let _env = TestEnv::new("grit-test-tcr-pass");
        init_repo("feature");
        commit_file("code.txt", "one\n");
        std::fs::write("code.txt", "two\n").unwrap();

        match run_cycle("true", "TCR: green") {
            Outcome::Committed(m) => assert_eq!(m, "TCR: green"),
            other => panic!("expected a commit, got {other:?}"),
        }
        assert_eq!(git::git_log(1)[0].contains("TCR: green"), true);
        assert_eq!(git::working_tree_signature().as_deref(), Some(""), "tree is clean after committing");
    }

    #[cfg(unix)]
    #[test]
    fn run_cycle_reverts_and_saves_a_patch_when_tests_fail() {
        let _env = TestEnv::new("grit-test-tcr-fail");
        init_repo("feature");
        commit_file("code.txt", "one\n");
        std::fs::write("code.txt", "two\n").unwrap();

        match run_cycle("false", "TCR: green") {
            Outcome::Reverted { patch, .. } => {
                let p = patch.expect("a failed cycle must leave a recoverable patch");
                assert!(std::fs::read_to_string(&p).unwrap().contains("code.txt"));
            }
            other => panic!("expected a revert, got {other:?}"),
        }
        assert_eq!(std::fs::read_to_string("code.txt").unwrap(), "one\n");
    }

    #[cfg(unix)]
    #[test]
    fn run_cycle_reports_a_refused_commit_instead_of_reverting() {
        let _env = TestEnv::new("grit-test-tcr-main");
        init_repo("main");
        commit_file("code.txt", "one\n");
        std::fs::write("code.txt", "two\n").unwrap();

        match run_cycle("true", "TCR: green") {
            Outcome::NotCommitted(e) => assert!(e.contains("Refusing to commit"), "got: {e}"),
            other => panic!("expected a refused commit, got {other:?}"),
        }
        // Passing work is never thrown away just because it can't be committed.
        assert_eq!(std::fs::read_to_string("code.txt").unwrap(), "two\n");
    }

    #[cfg(unix)]
    #[test]
    fn watch_refuses_to_start_without_a_repo_or_a_command() {
        let _env = TestEnv::new("grit-test-tcr-watch-guard");
        let err = watch(WatchOptions { command: "true".into(), ..Default::default() }).unwrap_err();
        assert!(err.contains("not inside a git repository"), "got: {err}");

        init_repo("feature");
        let err = watch(WatchOptions::default()).unwrap_err();
        assert!(err.contains("no test command"), "got: {err}");
    }

    #[cfg(unix)]
    #[test]
    fn watch_once_runs_a_single_cycle_and_names_the_open_task() {
        let _env = TestEnv::new("grit-test-tcr-watch-once");
        init_repo("feature");
        commit_file("code.txt", "one\n");
        write_tasks(&[("teach the watcher to read tasks", crate::task::Status::Working)]);

        // The first observation is only a baseline, so the change has to land
        // after the watcher starts. Two writes, then silence: whatever the
        // baseline caught, the tree has changed and then held still.
        let handle = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(60));
            std::fs::write("code.txt", "two\n").unwrap();
            std::thread::sleep(Duration::from_millis(60));
            std::fs::write("more.txt", "new\n").unwrap();
        });
        let res = watch(WatchOptions {
            command: "true".into(),
            interval: Duration::from_millis(20),
            settle: 2,
            once: true,
            ..Default::default()
        });
        handle.join().unwrap();
        res.expect("watch --once should return after one cycle");
        assert!(
            git::git_log(1)[0].contains("TCR: teach the watcher to read tasks"),
            "log: {:?}",
            git::git_log(2)
        );
    }
}
