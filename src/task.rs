// ─── Data types ──────────────────────────────────────────────────────────────

use ratatui::style::Color;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Status {
    Pending,
    Working,
    Done,
}

impl Status {
    // Lowercase label used in commit-message templates ({status}).
    pub fn label(&self) -> &str {
        match self {
            Status::Pending => "pending",
            Status::Working => "working",
            Status::Done => "done",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Priority {
    High,
    Medium,
    Low,
}

impl Priority {
    pub fn label(&self) -> &str {
        match self {
            Priority::High => "!!",
            Priority::Medium => "! ",
            Priority::Low => "  ",
        }
    }
    pub fn color(&self) -> Color {
        match self {
            Priority::High => Color::Red,
            Priority::Medium => Color::Yellow,
            Priority::Low => Color::Blue,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SubStatus {
    Pending,
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subtask {
    pub description: String,
    pub status: SubStatus,
    // Nested subtasks — a subtask can have children of its own.
    // serde(default) keeps old tasks.json files importable (field was absent).
    #[serde(default)]
    pub subtasks: Vec<Subtask>,
}

impl Subtask {
    pub fn new(description: String) -> Option<Self> {
        let d = description.trim().to_string();
        if d.is_empty() {
            None
        } else {
            Some(Subtask { description: d, status: SubStatus::Pending, subtasks: vec![] })
        }
    }

    // Recursive done count over this subtask and all descendants.
    pub fn count_done(&self) -> usize {
        let own = usize::from(self.status == SubStatus::Done);
        own + self.subtasks.iter().map(|s| s.count_done()).sum::<usize>()
    }

    // Recursive total count over this subtask and all descendants.
    pub fn count_total(&self) -> usize {
        1 + self.subtasks.iter().map(|s| s.count_total()).sum::<usize>()
    }
}

// Walk a Vec<usize> path (indices into nested `subtasks` vecs) and return a
// mutable reference to the subtask it points to. An empty path returns None —
// callers handle the task-level case themselves.
pub fn subtask_mut_at<'a>(task: &'a mut Task, path: &[usize]) -> Option<&'a mut Subtask> {
    let mut node = task.subtasks.get_mut(*path.first()?)?;
    for &i in &path[1..] {
        node = node.subtasks.get_mut(i)?;
    }
    Some(node)
}

// Immutable version of subtask_mut_at.
pub fn subtask_at<'a>(task: &'a Task, path: &[usize]) -> Option<&'a Subtask> {
    let mut node = task.subtasks.get(*path.first()?)?;
    for &i in &path[1..] {
        node = node.subtasks.get(i)?;
    }
    Some(node)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub description: String,
    pub status: Status,
    pub priority: Priority,
    pub tags: Vec<String>,
    pub due_date: Option<String>, // stored as YYYY-MM-DD string
    pub created_at: String,
    pub subtasks: Vec<Subtask>,
}

impl Task {
    pub fn new(description: String) -> Option<Self> {
        if description.trim().is_empty() {
            return None;
        }
        let (desc, tags) = parse_tags(&description);
        let (desc, due) = parse_due_date(&desc);
        let (desc, priority) = parse_priority(&desc);
        // The pre-parse check passes for e.g. "#tag p:high" — but if every word
        // was consumed as metadata the description itself is empty.
        if desc.trim().is_empty() {
            return None;
        }
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

pub fn parse_tags(input: &str) -> (String, Vec<String>) {
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

pub fn parse_due_date(input: &str) -> (String, Option<String>) {
    let mut due = None;
    let mut rest = String::new();
    for word in input.split_whitespace() {
        if word.starts_with("due:") && is_valid_date(&word[4..]) {
            due = Some(word[4..].to_string());
        } else {
            if !rest.is_empty() { rest.push(' '); }
            rest.push_str(word);
        }
    }
    (rest, due)
}

// Strict YYYY-MM-DD check so overdue comparisons and date sorting stay meaningful.
// Invalid dates stay part of the description instead of becoming metadata.
pub fn is_valid_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let all_digits = |r: &[u8]| r.iter().all(|c| c.is_ascii_digit());
    if !(all_digits(&b[0..4]) && all_digits(&b[5..7]) && all_digits(&b[8..10])) {
        return false;
    }
    let month: u32 = s[5..7].parse().unwrap_or(0);
    let day: u32 = s[8..10].parse().unwrap_or(0);
    (1..=12).contains(&month) && (1..=31).contains(&day)
}

pub fn parse_priority(input: &str) -> (String, Priority) {
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

pub fn current_date() -> String {
    // UTC date from the system clock — no subprocess needed.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

// UTC date+time compacted for filenames: YYYYMMDDHHMMSS.
pub fn current_datetime_compact() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (days, sod) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    let (h, mi, s) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    format!("{y:04}{m:02}{d:02}{h:02}{mi:02}{s:02}")
}

// Days since 1970-01-01 -> (year, month, day) in UTC.
// Howard Hinnant's civil_from_days algorithm.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

pub fn is_overdue(due: &str, today: &str) -> bool {
    due < today
}

// ─── Filter ──────────────────────────────────────────────────────────────────

// A structured filter parsed from the prompt opened with `f`.
// Tokens: #tag, s:pending|working|done, p:high|medium|low, plain words.
// Semantics: tags AND words must ALL match; multiple statuses/priorities
// mean "any of them" (a task has exactly one of each).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TaskFilter {
    tags: Vec<String>,
    statuses: Vec<Status>,
    priorities: Vec<Priority>,
    words: Vec<String>,
}

impl TaskFilter {
    pub fn parse(query: &str) -> Self {
        let mut f = TaskFilter::default();
        for word in query.split_whitespace() {
            let lower = word.to_lowercase();
            if let Some(tag) = lower.strip_prefix('#') {
                if !tag.is_empty() {
                    f.tags.push(tag.to_string());
                }
            } else if let Some(s) = lower.strip_prefix("s:") {
                match s {
                    "pending" | "p" => f.statuses.push(Status::Pending),
                    "working" | "w" => f.statuses.push(Status::Working),
                    "done" | "d" => f.statuses.push(Status::Done),
                    _ => f.words.push(lower),
                }
            } else if let Some(p) = lower.strip_prefix("p:") {
                match p {
                    "high" | "h" => f.priorities.push(Priority::High),
                    "medium" | "m" => f.priorities.push(Priority::Medium),
                    "low" | "l" => f.priorities.push(Priority::Low),
                    _ => f.words.push(lower),
                }
            } else {
                f.words.push(lower);
            }
        }
        f
    }

    pub fn is_empty(&self) -> bool {
        self.tags.is_empty() && self.statuses.is_empty() && self.priorities.is_empty() && self.words.is_empty()
    }

    pub fn matches(&self, task: &Task) -> bool {
        let desc = task.description.to_lowercase();
        self.tags.iter().all(|tag| task.tags.iter().any(|t| t.to_lowercase() == *tag))
            && (self.statuses.is_empty() || self.statuses.contains(&task.status))
            && (self.priorities.is_empty() || self.priorities.contains(&task.priority))
            && self.words.iter().all(|w| desc.contains(w))
    }
}

// ─── Sort ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum SortMode {
    None,
    Status,
    Priority,
    DueDate,
    Created,
}

impl SortMode {
    pub fn label(&self) -> &str {
        match self {
            SortMode::None => "Default",
            SortMode::Status => "Status",
            SortMode::Priority => "Priority",
            SortMode::DueDate => "Due Date",
            SortMode::Created => "Created",
        }
    }
    pub fn next(&self) -> SortMode {
        match self {
            SortMode::None => SortMode::Status,
            SortMode::Status => SortMode::Priority,
            SortMode::Priority => SortMode::DueDate,
            SortMode::DueDate => SortMode::Created,
            SortMode::Created => SortMode::None,
        }
    }
}

pub fn sorted_indices(tasks: &[Task], mode: &SortMode) -> Vec<usize> {
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

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_days_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(19_753), (2024, 1, 31));
        assert_eq!(civil_from_days(19_754), (2024, 2, 1));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29)); // leap day
        assert_eq!(civil_from_days(20_680), (2026, 8, 15));
    }

    #[test]
    fn current_date_is_iso_format() {
        let d = current_date();
        assert!(is_valid_date(&d), "got: {d}");
    }

    #[test]
    fn date_validation() {
        assert!(is_valid_date("2025-06-01"));
        assert!(is_valid_date("1999-12-31"));
        assert!(!is_valid_date("2025-13-01"));   // bad month
        assert!(!is_valid_date("2025-00-10"));   // bad month
        assert!(!is_valid_date("2025-06-32"));   // bad day
        assert!(!is_valid_date("2025-06-00"));   // bad day
        assert!(!is_valid_date("2025-6-01"));    // wrong width
        assert!(!is_valid_date("abcd-ef-gh"));
        assert!(!is_valid_date("2025/06/01"));
        assert!(!is_valid_date(""));
    }

    #[test]
    fn invalid_due_token_stays_in_description() {
        let (rest, due) = parse_due_date("fix bug due:2025-99-99");
        assert_eq!(rest, "fix bug due:2025-99-99");
        assert_eq!(due, None);
        let (rest, due) = parse_due_date("fix bug due:2025-06-01");
        assert_eq!(rest, "fix bug");
        assert_eq!(due, Some("2025-06-01".to_string()));
    }

    #[test]
    fn task_metadata_parsing() {
        let t = Task::new("Fix login bug #bug #auth due:2025-06-01 p:high".to_string()).unwrap();
        assert_eq!(t.description, "Fix login bug");
        assert_eq!(t.tags, vec!["bug", "auth"]);
        assert_eq!(t.due_date, Some("2025-06-01".to_string()));
        assert_eq!(t.priority, Priority::High);
        assert!(is_overdue("2025-06-01", "2026-01-01"));
        assert!(!is_overdue("2025-06-01", "2025-06-01"));
    }

    #[test]
    fn empty_description_is_rejected() {
        assert!(Task::new(String::new()).is_none());
        assert!(Task::new("   ".to_string()).is_none());
        // A description made only of tokens is still empty after parsing
        assert!(Task::new("#tag due:2025-06-01 p:high".to_string()).is_none());
    }

    #[test]
    fn subtask_empty_description_is_rejected() {
        assert!(Subtask::new(String::new()).is_none());
        assert!(Subtask::new("  ".to_string()).is_none());
        assert!(Subtask::new("real work".to_string()).is_some());
    }

    #[test]
    fn subtask_new_starts_childless() {
        let s = Subtask::new("parent".to_string()).unwrap();
        assert!(s.subtasks.is_empty());
        assert_eq!(s.count_total(), 1);
        assert_eq!(s.count_done(), 0);
    }

    #[test]
    fn subtask_recursive_counts() {
        let mut root = Subtask::new("root".to_string()).unwrap();
        let mut child = Subtask::new("child".to_string()).unwrap();
        let grand = Subtask::new("grand".to_string()).unwrap();
        let mut grand2 = Subtask::new("grand2".to_string()).unwrap();
        grand2.status = SubStatus::Done;
        child.subtasks = vec![grand, grand2];
        child.status = SubStatus::Done;
        root.subtasks = vec![child];

        assert_eq!(root.count_total(), 4); // root + child + grand + grand2
        assert_eq!(root.count_done(), 2);  // child + grand2
        // child was moved into root, count through the tree instead
        assert_eq!(root.subtasks[0].count_total(), 3);
        assert_eq!(root.subtasks[0].count_done(), 2);
    }

    #[test]
    fn subtask_path_accessor_walks_nesting() {
        let mut t = Task::new("task".to_string()).unwrap();
        let mut child = Subtask::new("child".to_string()).unwrap();
        child.subtasks.push(Subtask::new("grand".to_string()).unwrap());
        t.subtasks.push(child);

        // [0] → child, [0, 0] → grand
        let child_mut = subtask_mut_at(&mut t, &[0]).unwrap();
        assert_eq!(child_mut.description, "child");
        let grand_mut = subtask_mut_at(&mut t, &[0, 0]).unwrap();
        grand_mut.status = SubStatus::Done;

        // Out-of-range paths are None
        assert!(subtask_mut_at(&mut t, &[1]).is_none());
        assert!(subtask_mut_at(&mut t, &[0, 1]).is_none());
        assert!(subtask_mut_at(&mut t, &[]).is_none());
        // The mutation through the path stuck
        assert_eq!(t.subtasks[0].subtasks[0].status, SubStatus::Done);
    }

    #[test]
    fn tag_parsing_edge_cases() {
        // Lone '#' becomes an empty tag and is removed from the text
        let (rest, tags) = parse_tags("buy milk #");
        assert_eq!(rest, "buy milk");
        assert_eq!(tags, vec![""]);

        // A word merely starting with '#' is a tag, even digits ("#12" → tag "12")
        let (rest, tags) = parse_tags("issue #12 is fixed");
        assert_eq!(rest, "issue is fixed");
        assert_eq!(tags, vec!["12"]);

        // Duplicate tags are kept as-is
        let (rest, tags) = parse_tags("x #a #a");
        assert_eq!(rest, "x");
        assert_eq!(tags, vec!["a", "a"]);

        // Multiple spaces collapse to one
        let (rest, _) = parse_tags("a    b");
        assert_eq!(rest, "a b");
    }

    #[test]
    fn priority_short_forms() {
        assert_eq!(parse_priority("x p:h").1, Priority::High);
        assert_eq!(parse_priority("x p:m").1, Priority::Medium);
        assert_eq!(parse_priority("x p:l").1, Priority::Low);
        assert_eq!(parse_priority("x p:high").0, "x");
        assert_eq!(parse_priority("x priority:high").0, "x priority:high");
    }

    #[test]
    fn datetime_compact_is_14_digits() {
        let d = current_datetime_compact();
        assert_eq!(d.len(), 14, "got: {d}");
        assert!(d.chars().all(|c| c.is_ascii_digit()), "got: {d}");
    }

    #[test]
    fn status_labels() {
        assert_eq!(Status::Pending.label(), "pending");
        assert_eq!(Status::Working.label(), "working");
        assert_eq!(Status::Done.label(), "done");
    }

    #[test]
    fn filter_parse_extracts_tags_statuses_priorities_and_words() {
        let f = TaskFilter::parse("#auth s:working p:high urgent");
        assert_eq!(f.tags, vec!["auth"]);
        assert_eq!(f.statuses, vec![Status::Working]);
        assert_eq!(f.priorities, vec![Priority::High]);
        assert_eq!(f.words, vec!["urgent"]);
        assert!(!f.is_empty());

        // Unknown s:/p: values fall back to plain word matching
        let f = TaskFilter::parse("s:later");
        assert!(f.statuses.is_empty());
        assert_eq!(f.words, vec!["s:later"]);

        // Everything at once is empty → matches everything
        assert!(TaskFilter::parse("").is_empty());
        assert!(TaskFilter::parse("   ").is_empty());
        // A lone '#' contributes nothing
        assert!(TaskFilter::parse("#").is_empty());
    }

    #[test]
    fn filter_matches_requires_all_conditions() {
        // tags AND words; statuses OR priorities are each single-valued so
        // multiple selections mean "any of them"
        let mut t = Task::new("fix login bug #auth #web".to_string()).unwrap();
        t.status = Status::Working;
        t.priority = Priority::High;

        assert!(TaskFilter::parse("#auth").matches(&t));
        assert!(TaskFilter::parse("#auth #web").matches(&t));
        assert!(!TaskFilter::parse("#auth #missing").matches(&t));

        assert!(TaskFilter::parse("login").matches(&t));
        assert!(!TaskFilter::parse("logout").matches(&t));
        // multiple words AND together
        assert!(TaskFilter::parse("fix bug").matches(&t));
        assert!(!TaskFilter::parse("fix logout").matches(&t));

        assert!(TaskFilter::parse("s:working").matches(&t));
        assert!(!TaskFilter::parse("s:pending").matches(&t));
        assert!(TaskFilter::parse("s:pending s:working").matches(&t), "two statuses = any of them");
        assert!(TaskFilter::parse("p:high").matches(&t));
        assert!(!TaskFilter::parse("p:low").matches(&t));

        // full conjunction
        assert!(TaskFilter::parse("#auth s:working p:high login").matches(&t));
        assert!(!TaskFilter::parse("#auth s:done login").matches(&t));

        // case-insensitive tags/words
        assert!(TaskFilter::parse("#AUTH Login").matches(&t));
    }

    #[test]
    fn sort_mode_cycles() {
        let mut m = SortMode::None;
        let order = [
            (SortMode::Status, "Status"),
            (SortMode::Priority, "Priority"),
            (SortMode::DueDate, "Due Date"),
            (SortMode::Created, "Created"),
            (SortMode::None, "Default"),
        ];
        for (expected, label) in order {
            m = m.next();
            assert_eq!(m, expected);
            assert_eq!(m.label(), label);
        }
    }

    fn sample_tasks() -> Vec<Task> {
        let mut a = Task::new("first p:high".to_string()).unwrap();
        a.status = Status::Pending;
        let mut b = Task::new("second p:low".to_string()).unwrap();
        b.status = Status::Working;
        let mut c = Task::new("third p:medium".to_string()).unwrap();
        c.status = Status::Done;
        vec![a, b, c]
    }

    #[test]
    fn sorted_indices_status_order() {
        let tasks = sample_tasks();
        let got = sorted_indices(&tasks, &SortMode::Status);
        assert_eq!(got, vec![1, 0, 2]); // Working, Pending, Done
    }

    #[test]
    fn sorted_indices_priority_order() {
        let tasks = sample_tasks();
        let got = sorted_indices(&tasks, &SortMode::Priority);
        assert_eq!(got, vec![0, 2, 1]); // High, Medium, Low
    }

    #[test]
    fn sorted_indices_due_date_puts_none_last() {
        let mut a = Task::new("a due:2026-03-01".to_string()).unwrap();
        a.due_date = Some("2026-03-01".to_string());
        let mut b = Task::new("b due:2026-01-01".to_string()).unwrap();
        b.due_date = Some("2026-01-01".to_string());
        let c = Task::new("c".to_string()).unwrap(); // no due date
        let tasks = vec![a, b, c];
        let got = sorted_indices(&tasks, &SortMode::DueDate);
        assert_eq!(got, vec![1, 0, 2]);
    }

    #[test]
    fn sorted_indices_created_stable_on_ties() {
        let mut a = Task::new("a".to_string()).unwrap();
        a.created_at = "2026-01-01".to_string();
        let mut b = Task::new("b".to_string()).unwrap();
        b.created_at = "2026-01-01".to_string();
        let mut c = Task::new("c".to_string()).unwrap();
        c.created_at = "2025-12-31".to_string();
        let tasks = vec![a, b, c];
        let got = sorted_indices(&tasks, &SortMode::Created);
        assert_eq!(got, vec![2, 0, 1]);
    }

    #[test]
    fn sorted_indices_empty_list() {
        assert!(sorted_indices(&[], &SortMode::Status).is_empty());
    }

    #[test]
    fn sorted_indices_none_is_identity() {
        let tasks = sample_tasks();
        let got = sorted_indices(&tasks, &SortMode::None);
        assert_eq!(got, vec![0, 1, 2]);
    }
}
