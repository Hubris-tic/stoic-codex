//! Filesystem tools: read, write, edit, list, search.
//!
//! Two rules run through all of them:
//!
//! * **Every path goes through the sandbox.** No exceptions, no "just this once" for the
//!   path the model seems sure about.
//! * **Output is budgeted.** An agent that reads a 40 000-line file has spent its context
//!   on one tool call. Reads are line-ranged and capped, listings are depth-limited, and
//!   searches stop at a result count. The tool says what it truncated, so the model can
//!   ask for the rest instead of assuming it saw everything.

use std::sync::Arc;

use serde_json::Value;

use crate::sandbox::Sandbox;
use crate::tool::{
    optional_bool, optional_str, optional_u64, required_str, schema_object, Tool, ToolOutcome,
};

/// Directories that never help an agent and always waste its budget.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".venv",
    "__pycache__",
];

const MAX_READ_LINES: u64 = 2000;
const MAX_FILE_BYTES: u64 = 1024 * 1024;

pub struct ReadFile {
    sandbox: Arc<Sandbox>,
}

impl ReadFile {
    pub fn new(sandbox: Arc<Sandbox>) -> Self {
        ReadFile { sandbox }
    }
}

impl Tool for ReadFile {
    fn name(&self) -> &'static str {
        "read_file"
    }

    fn description(&self) -> &'static str {
        "Read a text file from the repository. Returns numbered lines. Use `start_line` and \
         `max_lines` for large files; the reply states how many lines exist."
    }

    fn schema(&self) -> Value {
        let mut schema = schema_object(
            &[
                ("path", "string", "Path relative to the repository root"),
                (
                    "start_line",
                    "integer",
                    "First line to return, 1-based (default 1)",
                ),
                (
                    "max_lines",
                    "integer",
                    "Maximum lines to return (default 400)",
                ),
            ],
            &["path"],
        );
        // Numbers, not strings: a model that types "10" gets a clear error instead of a
        // silently empty range.
        schema["properties"]["start_line"]["type"] = Value::String("integer".into());
        schema["properties"]["max_lines"]["type"] = Value::String("integer".into());
        schema
    }

    fn invoke(&self, args: &Value) -> ToolOutcome {
        let raw = match required_str(args, "path", "read_file") {
            Ok(p) => p,
            Err(e) => return e,
        };
        let path = match self.sandbox.resolve_existing(raw) {
            Ok(p) => p,
            Err(e) => return ToolOutcome::err(e.to_string()),
        };
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(e) => return ToolOutcome::err(format!("{raw}: {e}")),
        };
        if meta.is_dir() {
            return ToolOutcome::err(format!("`{raw}` is a directory; use list_dir"));
        }
        if meta.len() > MAX_FILE_BYTES {
            return ToolOutcome::err(format!(
                "`{raw}` is {} bytes, over the {MAX_FILE_BYTES}-byte read limit",
                meta.len()
            ));
        }
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => return ToolOutcome::err(format!("{raw}: {e}")),
        };
        let text = match String::from_utf8(bytes) {
            Ok(t) => t,
            Err(_) => return ToolOutcome::err(format!("`{raw}` is not UTF-8 text")),
        };

        let start = optional_u64(args, "start_line").unwrap_or(1).max(1);
        let want = optional_u64(args, "max_lines")
            .unwrap_or(400)
            .min(MAX_READ_LINES) as usize;
        let lines: Vec<&str> = text.lines().collect();
        let total = lines.len();
        if total == 0 {
            return ToolOutcome::ok(format!("{raw} is empty"));
        }
        if start as usize > total {
            return ToolOutcome::err(format!(
                "`{raw}` has {total} lines; start_line {start} is past the end"
            ));
        }
        let from = start as usize - 1;
        let to = (from + want).min(total);

        let mut out = String::new();
        // Line numbers are part of the contract: they are what an edit or a review refers
        // to, and models are much better at `edit_file` when they can cite a line.
        for (i, line) in lines[from..to].iter().enumerate() {
            out.push_str(&format!("{:>6}\t{}\n", from + i + 1, line));
        }
        if to < total {
            out.push_str(&format!(
                "… {} more lines (file has {total}); call read_file again with start_line={}\n",
                total - to,
                to + 1
            ));
        }
        ToolOutcome::ok(out)
    }
}

pub struct WriteFile {
    sandbox: Arc<Sandbox>,
}

impl WriteFile {
    pub fn new(sandbox: Arc<Sandbox>) -> Self {
        WriteFile { sandbox }
    }
}

impl Tool for WriteFile {
    fn name(&self) -> &'static str {
        "write_file"
    }

    fn description(&self) -> &'static str {
        "Create or replace a file with the given contents. Parent directories are created. \
         Prefer edit_file for a change to an existing file: write_file replaces the whole file."
    }

    fn schema(&self) -> Value {
        schema_object(
            &[
                ("path", "string", "Path relative to the repository root"),
                ("content", "string", "The complete new contents of the file"),
            ],
            &["path", "content"],
        )
    }

    fn invoke(&self, args: &Value) -> ToolOutcome {
        let raw = match required_str(args, "path", "write_file") {
            Ok(p) => p,
            Err(e) => return e,
        };
        let content = match required_str(args, "content", "write_file") {
            Ok(c) => c,
            Err(e) => return e,
        };
        if self.sandbox.is_git_internal(raw) {
            return ToolOutcome::err(format!(
                "`{raw}` is inside .git; the agent may not write git metadata"
            ));
        }
        let path = match self.sandbox.resolve(raw) {
            Ok(p) => p,
            Err(e) => return ToolOutcome::err(e.to_string()),
        };
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return ToolOutcome::err(format!(
                    "cannot create {}: {e}",
                    self.sandbox.display(parent)
                ));
            }
        }
        let existed = path.exists();
        match std::fs::write(&path, content) {
            Ok(()) => ToolOutcome::ok(format!(
                "{} {raw} ({} bytes)",
                if existed { "replaced" } else { "created" },
                content.len()
            )),
            Err(e) => ToolOutcome::err(format!("cannot write {raw}: {e}")),
        }
    }
}

pub struct EditFile {
    sandbox: Arc<Sandbox>,
}

impl EditFile {
    pub fn new(sandbox: Arc<Sandbox>) -> Self {
        EditFile { sandbox }
    }
}

impl Tool for EditFile {
    fn name(&self) -> &'static str {
        "edit_file"
    }

    fn description(&self) -> &'static str {
        "Replace an exact snippet in a file. `old` must match the file byte for byte, \
         including indentation. If it matches more than once the edit is refused unless \
         `replace_all` is true, so a change cannot silently hit the wrong place."
    }

    fn schema(&self) -> Value {
        schema_object(
            &[
                ("path", "string", "Path relative to the repository root"),
                (
                    "old",
                    "string",
                    "Exact text to replace, including indentation",
                ),
                ("new", "string", "Replacement text"),
                (
                    "replace_all",
                    "boolean",
                    "Replace every occurrence (default false)",
                ),
            ],
            &["path", "old", "new"],
        )
    }

    fn invoke(&self, args: &Value) -> ToolOutcome {
        let raw = match required_str(args, "path", "edit_file") {
            Ok(p) => p,
            Err(e) => return e,
        };
        let old = match required_str(args, "old", "edit_file") {
            Ok(o) => o,
            Err(e) => return e,
        };
        let new = match required_str(args, "new", "edit_file") {
            Ok(n) => n,
            Err(e) => return e,
        };
        let replace_all = optional_bool(args, "replace_all");
        if old.is_empty() {
            return ToolOutcome::err("`old` is empty; edit_file needs the text to replace");
        }
        if self.sandbox.is_git_internal(raw) {
            return ToolOutcome::err(format!("`{raw}` is inside .git; refusing to edit it"));
        }
        let path = match self.sandbox.resolve_existing(raw) {
            Ok(p) => p,
            Err(e) => return ToolOutcome::err(e.to_string()),
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => return ToolOutcome::err(format!("cannot read {raw}: {e}")),
        };
        let hits = text.matches(old).count();
        if hits == 0 {
            // The most common failure by far is whitespace drift. Say so, and show a
            // nearby line, so the next attempt can be right instead of random.
            let hint = nearest_line(&text, old);
            return ToolOutcome::err(format!(
                "`old` was not found in {raw}. Check indentation and exact text.{}",
                hint
            ));
        }
        if hits > 1 && !replace_all {
            return ToolOutcome::err(format!(
                "`old` matches {hits} places in {raw}. Include more surrounding context, or pass replace_all: true"
            ));
        }
        let updated = if replace_all {
            text.replace(old, new)
        } else {
            text.replacen(old, new, 1)
        };
        match std::fs::write(&path, &updated) {
            Ok(()) => ToolOutcome::ok(format!(
                "edited {raw} ({hits} replacement{})",
                if hits == 1 { "" } else { "s" }
            )),
            Err(e) => ToolOutcome::err(format!("cannot write {raw}: {e}")),
        }
    }
}

/// Point at the closest line so a failed edit is a hint, not a dead end.
fn nearest_line(text: &str, old: &str) -> String {
    let needle = old.lines().next().unwrap_or("").trim();
    if needle.len() < 4 {
        return String::new();
    }
    let key: String = needle.chars().take(40).collect();
    for (i, line) in text.lines().enumerate() {
        if line.contains(key.trim()) {
            return format!(" Line {} looks close: `{}`", i + 1, line.trim());
        }
    }
    String::new()
}

pub struct ListDir {
    sandbox: Arc<Sandbox>,
}

impl ListDir {
    pub fn new(sandbox: Arc<Sandbox>) -> Self {
        ListDir { sandbox }
    }
}

impl Tool for ListDir {
    fn name(&self) -> &'static str {
        "list_dir"
    }

    fn description(&self) -> &'static str {
        "List the repository (or one directory) as a tree, with sizes. Skips .git, \
         node_modules, target and other dependency directories."
    }

    fn schema(&self) -> Value {
        schema_object(
            &[
                (
                    "path",
                    "string",
                    "Directory relative to the root (default \".\")",
                ),
                ("depth", "integer", "How deep to recurse, 1-4 (default 2)"),
            ],
            &[],
        )
    }

    fn invoke(&self, args: &Value) -> ToolOutcome {
        let raw = optional_str(args, "path").unwrap_or(".");
        let depth = optional_u64(args, "depth").unwrap_or(2).clamp(1, 4) as usize;
        let path = match self.sandbox.resolve_existing(raw) {
            Ok(p) => p,
            Err(e) => return ToolOutcome::err(e.to_string()),
        };
        if !path.is_dir() {
            return ToolOutcome::err(format!("`{raw}` is a file; use read_file"));
        }
        let mut out = String::new();
        let mut count = 0usize;
        walk(&path, "", depth, &mut out, &mut count);
        if count == 0 {
            return ToolOutcome::ok(format!("{raw} is empty"));
        }
        out.push_str(&format!("\n{count} entries"));
        ToolOutcome::ok(out)
    }
}

fn walk(dir: &std::path::Path, prefix: &str, depth: usize, out: &mut String, count: &mut usize) {
    if depth == 0 || *count > 500 {
        return;
    }
    let mut entries: Vec<_> = match std::fs::read_dir(dir) {
        Ok(rd) => rd.flatten().collect(),
        Err(_) => return,
    };
    entries.sort_by_key(|e| {
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        (!is_dir, e.file_name())
    });
    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir && SKIP_DIRS.contains(&name.as_str()) {
            continue;
        }
        *count += 1;
        if is_dir {
            out.push_str(&format!("{prefix}{name}/\n"));
            walk(&entry.path(), &format!("{prefix}  "), depth - 1, out, count);
        } else {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            out.push_str(&format!("{prefix}{name} ({size}b)\n"));
        }
    }
}

pub struct Search {
    sandbox: Arc<Sandbox>,
}

impl Search {
    pub fn new(sandbox: Arc<Sandbox>) -> Self {
        Search { sandbox }
    }
}

impl Tool for Search {
    fn name(&self) -> &'static str {
        "search"
    }

    fn description(&self) -> &'static str {
        "Find a literal substring across the repository. Returns file:line matches. Use it \
         before assuming a symbol exists, and to find every place a change must touch."
    }

    fn schema(&self) -> Value {
        schema_object(
            &[
                ("pattern", "string", "Literal text to find (case-sensitive)"),
                (
                    "path",
                    "string",
                    "Directory to search (default: whole repository)",
                ),
                (
                    "max_results",
                    "integer",
                    "Stop after this many matches (default 50)",
                ),
            ],
            &["pattern"],
        )
    }

    fn invoke(&self, args: &Value) -> ToolOutcome {
        let pattern = match required_str(args, "pattern", "search") {
            Ok(p) => p,
            Err(e) => return e,
        };
        if pattern.is_empty() {
            return ToolOutcome::err("`pattern` is empty");
        }
        let root = match optional_str(args, "path") {
            Some(p) => match self.sandbox.resolve_existing(p) {
                Ok(path) => path,
                Err(e) => return ToolOutcome::err(e.to_string()),
            },
            None => self.sandbox.root().to_path_buf(),
        };
        let max = optional_u64(args, "max_results")
            .unwrap_or(50)
            .clamp(1, 500) as usize;

        let mut out = String::new();
        let mut hits = 0usize;
        let mut files = 0usize;
        search_dir(
            &root,
            self.sandbox.root(),
            pattern,
            max,
            &mut out,
            &mut hits,
            &mut files,
        );
        if hits == 0 {
            return ToolOutcome::ok(format!(
                "no matches for `{pattern}` in {files} file(s) searched"
            ));
        }
        out.push_str(&format!("\n{hits} match(es) in {files} file(s)"));
        if hits >= max {
            out.push_str(" — stopped at the result limit; narrow the search");
        }
        ToolOutcome::ok(out)
    }
}

fn search_dir(
    dir: &std::path::Path,
    root: &std::path::Path,
    pattern: &str,
    max: usize,
    out: &mut String,
    hits: &mut usize,
    files: &mut usize,
) {
    if *hits >= max {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    let mut sorted: Vec<_> = entries.flatten().collect();
    sorted.sort_by_key(|e| e.file_name());
    for entry in sorted {
        if *hits >= max {
            return;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            search_dir(&entry.path(), root, pattern, max, out, hits, files);
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if meta.len() > MAX_FILE_BYTES {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        *files += 1;
        let rel = entry
            .path()
            .strip_prefix(root)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| entry.path().display().to_string());
        for (i, line) in text.lines().enumerate() {
            if line.contains(pattern) {
                *hits += 1;
                out.push_str(&format!("{rel}:{}: {}\n", i + 1, line.trim()));
                if *hits >= max {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        dir: std::path::PathBuf,
    }

    impl Fixture {
        fn new(tag: &str) -> Fixture {
            let dir = std::env::temp_dir().join(format!(
                "zenithal-fs-{tag}-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(dir.join("src")).unwrap();
            std::fs::write(
                dir.join("src/main.rs"),
                "fn main() {\n    println!(\"hi\");\n}\n",
            )
            .unwrap();
            std::fs::write(dir.join("README.md"), "# demo\n").unwrap();
            Fixture { dir }
        }
        fn sandbox(&self) -> Arc<Sandbox> {
            Arc::new(Sandbox::new(&self.dir).unwrap())
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn reading_returns_numbered_lines() {
        let f = Fixture::new("read");
        let out = ReadFile::new(f.sandbox()).invoke(&serde_json::json!({"path": "src/main.rs"}));
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("1\tfn main() {"), "{}", out.content);
        assert!(out.content.contains("2\t    println!"));
    }

    #[test]
    fn reading_a_range_reports_what_is_left() {
        let f = Fixture::new("range");
        let out = ReadFile::new(f.sandbox())
            .invoke(&serde_json::json!({"path": "src/main.rs", "start_line": 2, "max_lines": 1}));
        assert!(out.content.contains("2\t"));
        assert!(!out.content.contains("1\tfn main"));
        assert!(out.content.contains("more lines"), "{}", out.content);
    }

    #[test]
    fn reading_past_the_end_is_a_clear_error() {
        let f = Fixture::new("past");
        let out = ReadFile::new(f.sandbox())
            .invoke(&serde_json::json!({"path": "src/main.rs", "start_line": 99}));
        assert!(out.is_error);
        assert!(out.content.contains("3 lines"), "{}", out.content);
    }

    #[test]
    fn a_directory_cannot_be_read_as_a_file() {
        let f = Fixture::new("dir");
        let out = ReadFile::new(f.sandbox()).invoke(&serde_json::json!({"path": "src"}));
        assert!(out.is_error);
        assert!(out.content.contains("list_dir"));
    }

    #[test]
    fn writing_creates_parents_and_reports_creation_then_replacement() {
        let f = Fixture::new("write");
        let tool = WriteFile::new(f.sandbox());
        let first = tool.invoke(&serde_json::json!({"path": "a/b/c.txt", "content": "one"}));
        assert!(first.content.contains("created"), "{}", first.content);
        let second = tool.invoke(&serde_json::json!({"path": "a/b/c.txt", "content": "two"}));
        assert!(second.content.contains("replaced"), "{}", second.content);
        assert_eq!(
            std::fs::read_to_string(f.dir.join("a/b/c.txt")).unwrap(),
            "two"
        );
    }

    #[test]
    fn writing_into_git_is_refused() {
        let f = Fixture::new("gitwrite");
        let out = WriteFile::new(f.sandbox())
            .invoke(&serde_json::json!({"path": ".git/hooks/pre-commit", "content": "evil"}));
        assert!(out.is_error);
        assert!(out.content.contains(".git"), "{}", out.content);
    }

    #[test]
    fn escaping_the_root_is_refused_by_every_path_tool() {
        let f = Fixture::new("escape");
        let sb = f.sandbox();
        let attempts = [
            ReadFile::new(sb.clone()).invoke(&serde_json::json!({"path": "../../etc/passwd"})),
            WriteFile::new(sb.clone()).invoke(&serde_json::json!({"path": "../x", "content": "x"})),
            ListDir::new(sb.clone()).invoke(&serde_json::json!({"path": ".."})),
        ];
        for out in attempts {
            assert!(out.is_error, "{}", out.content);
        }
    }

    #[test]
    fn editing_requires_an_exact_unique_match() {
        let f = Fixture::new("edit");
        let tool = EditFile::new(f.sandbox());

        let missing = tool.invoke(&serde_json::json!({
            "path": "src/main.rs", "old": "nonexistent", "new": "x"
        }));
        assert!(missing.is_error);
        assert!(missing.content.contains("not found"), "{}", missing.content);

        let ok = tool.invoke(&serde_json::json!({
            "path": "src/main.rs", "old": "println!(\"hi\")", "new": "println!(\"bye\")"
        }));
        assert!(!ok.is_error, "{}", ok.content);
        assert!(std::fs::read_to_string(f.dir.join("src/main.rs"))
            .unwrap()
            .contains("bye"));
    }

    #[test]
    fn an_ambiguous_edit_is_refused_with_the_count() {
        let f = Fixture::new("ambiguous");
        std::fs::write(f.dir.join("dup.txt"), "let x = 1;\nlet x = 1;\n").unwrap();
        let out = EditFile::new(f.sandbox()).invoke(
            &serde_json::json!({"path": "dup.txt", "old": "let x = 1;", "new": "let x = 2;"}),
        );
        assert!(out.is_error);
        assert!(out.content.contains("2 places"), "{}", out.content);
    }

    #[test]
    fn replace_all_is_honoured_when_asked_for() {
        let f = Fixture::new("all");
        std::fs::write(f.dir.join("dup.txt"), "aa\naa\n").unwrap();
        let out = EditFile::new(f.sandbox()).invoke(&serde_json::json!({
            "path": "dup.txt", "old": "aa", "new": "bb", "replace_all": true
        }));
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(
            std::fs::read_to_string(f.dir.join("dup.txt")).unwrap(),
            "bb\nbb\n"
        );
    }

    #[test]
    fn a_failed_edit_points_at_the_closest_line() {
        let f = Fixture::new("hint");
        let out = EditFile::new(f.sandbox()).invoke(&serde_json::json!({
            "path": "src/main.rs", "old": "\tprintln!(\"hi\");", "new": "x"
        }));
        assert!(out.is_error);
        assert!(out.content.contains("looks close"), "{}", out.content);
    }

    #[test]
    fn listing_skips_dependency_directories_and_reports_a_count() {
        let f = Fixture::new("list");
        std::fs::create_dir_all(f.dir.join("target/debug")).unwrap();
        std::fs::write(f.dir.join("target/debug/junk"), "x").unwrap();
        std::fs::create_dir_all(f.dir.join("node_modules/pkg")).unwrap();
        let out = ListDir::new(f.sandbox()).invoke(&serde_json::json!({}));
        assert!(!out.content.contains("target/"), "{}", out.content);
        assert!(!out.content.contains("node_modules"), "{}", out.content);
        assert!(out.content.contains("src/"), "{}", out.content);
    }

    #[test]
    fn search_finds_lines_and_reports_the_file_count() {
        let f = Fixture::new("search");
        std::fs::write(f.dir.join("src/lib.rs"), "pub fn hello() {}\n").unwrap();
        let out = Search::new(f.sandbox()).invoke(&serde_json::json!({"pattern": "hello"}));
        assert!(out.content.contains("src/lib.rs:1:"), "{}", out.content);
        assert!(out.content.contains("1 match"), "{}", out.content);

        let none = Search::new(f.sandbox()).invoke(&serde_json::json!({"pattern": "zebra"}));
        assert!(!none.is_error, "no matches is not an error");
        assert!(none.content.contains("no matches"), "{}", none.content);
    }

    #[test]
    fn search_stops_at_the_limit_and_says_so() {
        let f = Fixture::new("searchlimit");
        let body: String = (0..50).map(|i| format!("needle {i}\n")).collect();
        std::fs::write(f.dir.join("many.txt"), body).unwrap();
        let out = Search::new(f.sandbox())
            .invoke(&serde_json::json!({"pattern": "needle", "max_results": 5}));
        assert!(out.content.contains("5 match"), "{}", out.content);
        assert!(out.content.contains("result limit"), "{}", out.content);
    }
}
