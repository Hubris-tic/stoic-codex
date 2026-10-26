//! The Phalanx: the short-hand system codes that name Stoic Codex's executing agents.
//!
//! Under **Strategos** there is exactly one kind of subordinate: a *short-hand system code*.
//! Not a person, not a persona — an operational slot with a name short enough to survive a
//! log line at 3am:
//!
//! ```text
//! [strategos] deployed x4-core   the base context harvester
//! [strategos] deployed x1-write  the primary logic generator
//! [strategos] deployed x2-check  the testing and validation engine
//! [strategos] deployed x3-ship   the deployment and optimization pipeline
//! ```
//!
//! Taken together they are **The Phalanx**: one line of operators, each in its own git
//! worktree, none of them able to see the others. The name is the job. That is the whole
//! point of a short-hand code — `x1-write` tells you what a log line is about before you
//! have finished reading it, and it fits in a terminal column.
//!
//! ## Why the table lives here
//!
//! This crate is the product's vocabulary (its name, its binary, its config dir), and these
//! codes are the same kind of object: strings that must not drift between the orchestrator,
//! the CLI and the TUI. One table, no copies.
//!
//! ## Naming is Strategos' job
//!
//! [`assign`] is the deterministic fallback the orchestrator uses when the commander does
//! not name its own team: it scores each code's keywords against the assignment text and
//! prefers unused codes, so a three-operator mission gets three different jobs. When
//! Strategos *does* name the team, its answer is used verbatim as long as it is a code from
//! this table and not already taken.

/// One operational slot: a code, the job it names, and the words that suggest it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Code {
    /// The code itself, e.g. `x1-write`. Short, stable, lowercase.
    pub id: &'static str,
    /// What the code means, in one phrase — the line that follows the code in a log.
    pub role: &'static str,
    /// Keywords that suggest this code. Matched case-insensitively as substrings.
    pub keywords: &'static [&'static str],
}

/// The catalogue, in code order. `x0` to `x7`: eight jobs, which is more than a mission ever
/// needs and few enough to memorise.
pub const CODES: [Code; 8] = [
    Code {
        id: "x0-core",
        role: "the base context harvester",
        keywords: &[
            "context",
            "layout",
            "structure",
            "map",
            "inventory",
            "understand",
            "explore",
            "audit repo",
            "overview",
        ],
    },
    Code {
        id: "x1-write",
        role: "the primary logic generator",
        // Deliberately no "add"/"create": those words introduce almost every assignment and
        // would drown the specific job the assignment is about.
        keywords: &[
            "implement",
            "write",
            "logic",
            "feature",
            "function",
            "endpoint",
            "parser",
            "generator",
        ],
    },
    Code {
        id: "x2-check",
        role: "the testing and validation engine",
        keywords: &[
            "test", "verify", "validate", "assert", "coverage", "check", "lint", "spec",
        ],
    },
    Code {
        id: "x3-ship",
        role: "the deployment and optimization pipeline",
        keywords: &[
            "deploy",
            "ship",
            "release",
            "package",
            "optimize",
            "performance",
            "benchmark",
            "ci",
            "docker",
            "pipeline",
            "publish",
        ],
    },
    Code {
        id: "x4-scan",
        role: "the search and retrieval array",
        keywords: &[
            "search",
            "grep",
            "find",
            "locate",
            "usages",
            "scan",
            "enumerate",
            "which files",
        ],
    },
    Code {
        id: "x5-patch",
        role: "the surgical edit unit",
        keywords: &[
            "rename", "migrate", "replace", "refactor", "bump", "upgrade", "move", "extract",
        ],
    },
    Code {
        id: "x6-audit",
        role: "the review and hardening layer",
        keywords: &[
            "review", "security", "harden", "document", "docs", "comment", "readme", "safety",
        ],
    },
    Code {
        id: "x7-trace",
        role: "the diagnostics and debugging rig",
        keywords: &[
            "fix", "bug", "debug", "error", "crash", "panic", "leak", "race", "profile", "log",
            "hang", "flaky",
        ],
    },
];

/// The collective noun. Every code together, on one line, taking ground.
pub const PHALANX: &str = "The Phalanx";

/// The commander. Kept here so the orchestrator, the CLI and the TUI cannot spell it
/// differently.
pub const STRATEGOS: &str = "Strategos";

/// The code at `index`, cycling if a mission asks for more operators than there are jobs.
pub fn at(index: usize) -> &'static Code {
    &CODES[index % CODES.len()]
}

/// Look a code up by id, case-insensitively. Used to trust — or refuse — a name Strategos
/// proposed.
pub fn lookup(id: &str) -> Option<&'static Code> {
    let id = id.trim().to_ascii_lowercase();
    CODES.iter().find(|code| code.id == id)
}

/// Score a code against an assignment. Higher is a better fit; 0 means "no evidence".
///
/// Substring matching, deliberately: an assignment says "write a parser for X" or "add tests
/// for Y", and the giveaway is a word, not a grammar. The whole thing is a heuristic and is
/// documented as one — the authority is Strategos, and this is what happens when it stays
/// quiet.
pub fn score(code: &Code, task: &str) -> usize {
    let task = task.to_ascii_lowercase();
    code.keywords
        .iter()
        .map(|word| {
            // A keyword that *starts* a word is strong evidence, which also covers the
            // plural and the participle (`tests`, `tested`, `testing`); anything else is
            // buried inside another word (`latest` must not look like testing). Both are
            // case-insensitive because assignments are written by a model, not a grammar.
            if task
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .any(|w| w.starts_with(word))
            {
                3
            } else if task.contains(word) {
                1
            } else {
                0
            }
        })
        .sum()
}

/// The best code for one assignment, ignoring what is already taken.
pub fn classify(task: &str) -> &'static Code {
    let mut best = &CODES[1]; // `x1-write`: the honest default is "this is implementation work".
    let mut best_score = 0usize;
    for code in CODES.iter() {
        let score = score(code, task);
        if score > best_score {
            best = code;
            best_score = score;
        }
    }
    best
}

/// Name a team: one code per assignment, preferring jobs nobody has yet.
///
/// Deterministic, in plan order. When a mission has more operators than there are distinct
/// jobs — which takes nine — the repeats get a numeric suffix (`x1-write-2`) rather than a
/// made-up ninth job: inventing a code would break the promise that a code means one thing.
pub fn assign(tasks: &[&str]) -> Vec<String> {
    let mut taken: Vec<&'static str> = Vec::new();
    let mut out: Vec<String> = Vec::with_capacity(tasks.len());
    for task in tasks {
        // A free code with the most evidence; if nothing has evidence, the implementer;
        // if the whole table is taken, whatever the task looks most like.
        let id = best_free(task, &taken).unwrap_or_else(|| classify(task).id);
        if !taken.contains(&id) {
            taken.push(id);
        }
        let repeats = out.iter().filter(|name| name.as_str() == id).count() + 1;
        out.push(if repeats > 1 {
            format!("{id}-{repeats}")
        } else {
            id.to_string()
        });
    }
    out
}

/// The best code that nobody is using yet.
fn best_free(task: &str, taken: &[&'static str]) -> Option<&'static str> {
    let free: Vec<(&'static Code, usize)> = CODES
        .iter()
        .filter(|code| !taken.contains(&code.id))
        .map(|code| (code, score(code, task)))
        .collect();
    let top = free.iter().map(|(_, score)| *score).max().unwrap_or(0);
    if top > 0 {
        return free
            .iter()
            .find(|(_, score)| *score == top)
            .map(|(code, _)| code.id);
    }
    // No evidence at all: implementation is the honest default, and it is one of the free
    // ones unless the mission is enormous.
    free.iter()
        .find(|(code, _)| code.id == "x1-write")
        .or_else(|| free.first())
        .map(|(code, _)| code.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_codes_are_short_lowercase_and_unique() {
        let mut seen = Vec::new();
        for code in CODES.iter() {
            assert!(code.id.starts_with('x'), "{} must be an x-code", code.id);
            assert!(code.id.len() <= 9, "{} is too long for a log line", code.id);
            assert_eq!(
                code.id,
                code.id.to_ascii_lowercase(),
                "{} must be lowercase",
                code.id
            );
            assert!(!code.role.is_empty());
            assert!(!seen.contains(&code.id), "{} is listed twice", code.id);
            seen.push(code.id);
        }
    }

    #[test]
    fn the_doctrinal_four_are_where_the_brief_put_them() {
        assert_eq!(CODES[0].id, "x0-core");
        assert_eq!(CODES[0].role, "the base context harvester");
        assert_eq!(CODES[1].id, "x1-write");
        assert_eq!(CODES[1].role, "the primary logic generator");
        assert_eq!(CODES[2].id, "x2-check");
        assert_eq!(CODES[2].role, "the testing and validation engine");
        assert_eq!(CODES[3].id, "x3-ship");
        assert_eq!(CODES[3].role, "the deployment and optimization pipeline");
    }

    #[test]
    fn the_collective_is_the_phalanx() {
        assert_eq!(PHALANX, "The Phalanx");
        assert_eq!(STRATEGOS, "Strategos");
    }

    #[test]
    fn classification_reads_the_verb_of_the_assignment() {
        assert_eq!(classify("add tests for the token cache").id, "x2-check");
        assert_eq!(classify("implement the rate limiter").id, "x1-write");
        assert_eq!(
            classify("wire up the docker release pipeline").id,
            "x3-ship"
        );
        assert_eq!(
            classify("map the repository layout and report what it finds").id,
            "x0-core"
        );
        assert_eq!(classify("fix the panic in the diff parser").id, "x7-trace");
        assert_eq!(
            classify("rename file_exists to path_exists everywhere").id,
            "x5-patch"
        );
        assert_eq!(classify("grep for callers of run_agent").id, "x4-scan");
        assert_eq!(classify("harden the sandbox deny list").id, "x6-audit");
    }

    #[test]
    fn a_whole_word_beats_a_substring() {
        // "latest" must not look like testing.
        assert_ne!(classify("bump to the latest edition").id, "x2-check");
        assert!(score(&CODES[2], "add tests") > score(&CODES[2], "the latest release"));
    }

    #[test]
    fn silence_names_an_implementer_rather_than_nothing() {
        assert_eq!(classify("something vague").id, "x1-write");
    }

    #[test]
    fn a_team_gets_distinct_jobs() {
        let names = assign(&[
            "add tests for X",
            "fix the panic in Y",
            "map the repository",
        ]);
        assert_eq!(names, vec!["x2-check", "x7-trace", "x0-core"]);
        let mut unique = names.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            names.len(),
            "three operators, three jobs: {names:?}"
        );
    }

    #[test]
    fn identical_assignments_still_get_distinct_codes() {
        let names = assign(&["do the thing", "do the thing", "do the thing"]);
        assert_eq!(names.len(), 3);
        let mut unique = names.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            3,
            "repeat work still lands on different codes: {names:?}"
        );
    }

    #[test]
    fn more_operators_than_jobs_repeats_with_a_counter() {
        let tasks: Vec<&str> = std::iter::repeat("implement it").take(10).collect();
        let names = assign(&tasks);
        assert_eq!(names.len(), 10);
        assert_eq!(names[0], "x1-write");
        assert!(
            names.iter().any(|n| n.starts_with("x1-write-2")),
            "{names:?}"
        );
    }

    #[test]
    fn names_from_the_commander_are_checked_not_trusted() {
        assert_eq!(lookup("x1-write").map(|c| c.id), Some("x1-write"));
        assert_eq!(lookup(" X3-SHIP ").map(|c| c.id), Some("x3-ship"));
        assert!(lookup("y9-bogus").is_none());
        assert!(lookup("").is_none());
    }

    #[test]
    fn indexing_wraps_instead_of_panicking() {
        assert_eq!(at(0).id, "x0-core");
        assert_eq!(at(CODES.len()).id, "x0-core");
        assert_eq!(at(CODES.len() + 3).id, "x3-ship");
    }
}
