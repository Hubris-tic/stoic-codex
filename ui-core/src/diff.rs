//! Unified-diff parsing and side-by-side view model.
//!
//! Kept out of the render layer on purpose: every hard part of a diff viewer is
//! data, not drawing — hunk boundaries, pairing removed with added lines, folding
//! long runs of context, and character-level highlighting inside a changed line.
//! All of that is testable without a terminal, so it is tested here.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Deleted,
    Modified,
    Renamed,
    Binary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_lineno: Option<u32>,
    pub new_lineno: Option<u32>,
    /// Line content without the leading +/-/space marker.
    pub text: String,
    /// No trailing newline in the source file.
    pub no_newline: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: u32,
    pub old_len: u32,
    pub new_start: u32,
    pub new_len: u32,
    /// Text after the closing `@@`, e.g. the enclosing function.
    pub header: Option<String>,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub status: FileStatus,
    pub hunks: Vec<Hunk>,
}

impl FileDiff {
    /// Display path: the new path, falling back to the old one for deletions.
    pub fn display_path(&self) -> &str {
        self.new_path
            .as_deref()
            .or(self.old_path.as_deref())
            .unwrap_or("<unknown>")
    }

    pub fn counts(&self) -> (u32, u32) {
        let mut add = 0;
        let mut del = 0;
        for h in &self.hunks {
            for l in &h.lines {
                match l.kind {
                    LineKind::Added => add += 1,
                    LineKind::Removed => del += 1,
                    LineKind::Context => {}
                }
            }
        }
        (add, del)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffError {
    NotADiff,
    MalformedHunk(String),
}

impl fmt::Display for DiffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DiffError::NotADiff => write!(f, "input does not look like a unified diff"),
            DiffError::MalformedHunk(s) => write!(f, "malformed hunk header: {s}"),
        }
    }
}

impl std::error::Error for DiffError {}

fn strip_prefix<'a>(s: &'a str, p: &str) -> Option<&'a str> {
    s.strip_prefix(p)
}

/// Parse `git diff` / `diff -u` output.
pub fn parse(input: &str) -> Result<Vec<FileDiff>, DiffError> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut cur: Option<FileDiff> = None;
    let mut hunk: Option<Hunk> = None;
    let mut old_no = 0u32;
    let mut new_no = 0u32;
    let mut saw_anything = false;

    let flush_hunk = |file: &mut Option<FileDiff>, hunk: &mut Option<Hunk>| {
        if let (Some(f), Some(h)) = (file.as_mut(), hunk.take()) {
            f.hunks.push(h);
        }
    };
    let flush_file = |files: &mut Vec<FileDiff>, cur: &mut Option<FileDiff>, hunk: &mut Option<Hunk>| {
        if let (Some(f), Some(h)) = (cur.as_mut(), hunk.take()) {
            f.hunks.push(h);
        }
        if let Some(f) = cur.take() {
            files.push(f);
        }
    };

    for raw in input.lines() {
        if let Some(rest) = strip_prefix(raw, "diff --git ") {
            saw_anything = true;
            flush_file(&mut files, &mut cur, &mut hunk);
            let (a, b) = split_git_paths(rest);
            cur = Some(FileDiff {
                old_path: a,
                new_path: b,
                status: FileStatus::Modified,
                hunks: Vec::new(),
            });
            continue;
        }
        if raw.starts_with("--- ") && !raw.starts_with("--- /dev/null") || raw == "--- /dev/null" {
            if cur.is_none() {
                cur = Some(FileDiff {
                    old_path: None,
                    new_path: None,
                    status: FileStatus::Modified,
                    hunks: Vec::new(),
                });
            }
            if let Some(f) = cur.as_mut() {
                f.old_path = clean_ab_path(&raw[4..]);
            }
            saw_anything = true;
            continue;
        }
        if let Some(rest) = strip_prefix(raw, "+++ ") {
            if cur.is_none() {
                cur = Some(FileDiff {
                    old_path: None,
                    new_path: None,
                    status: FileStatus::Modified,
                    hunks: Vec::new(),
                });
            }
            if let Some(f) = cur.as_mut() {
                if rest.trim() == "/dev/null" {
                    f.new_path = None;
                    f.status = FileStatus::Deleted;
                } else {
                    f.new_path = clean_ab_path(rest);
                }
            }
            saw_anything = true;
            continue;
        }
        if let Some(rest) = strip_prefix(raw, "new file mode") {
            let _ = rest;
            if let Some(f) = cur.as_mut() {
                f.status = FileStatus::Added;
            }
            continue;
        }
        if let Some(rest) = strip_prefix(raw, "deleted file mode") {
            let _ = rest;
            if let Some(f) = cur.as_mut() {
                f.status = FileStatus::Deleted;
            }
            continue;
        }
        if raw.starts_with("rename from ") || raw.starts_with("rename to ") {
            if let Some(f) = cur.as_mut() {
                f.status = FileStatus::Renamed;
            }
            continue;
        }
        if let Some(rest) = strip_prefix(raw, "Binary files ") {
            let _ = rest;
            if let Some(f) = cur.as_mut() {
                f.status = FileStatus::Binary;
            }
            continue;
        }
        if let Some(rest) = strip_prefix(raw, "@@ ") {
            flush_hunk(&mut cur, &mut hunk);
            let (o, n, hdr) = parse_hunk_header(rest)?;
            old_no = o.0;
            new_no = n.0;
            hunk = Some(Hunk {
                old_start: o.0,
                old_len: o.1,
                new_start: n.0,
                new_len: n.1,
                header: hdr,
                lines: Vec::new(),
            });
            saw_anything = true;
            continue;
        }
        // Body lines only count while inside a hunk.
        if hunk.is_some() {
            let Some(h) = hunk.as_mut() else { continue };
            if raw == "\\ No newline at end of file" {
                if let Some(last) = h.lines.last_mut() {
                    last.no_newline = true;
                }
                continue;
            }
            let (kind, text) = match raw.as_bytes().first() {
                Some(b'+') => (LineKind::Added, &raw[1..]),
                Some(b'-') => (LineKind::Removed, &raw[1..]),
                Some(b' ') => (LineKind::Context, &raw[1..]),
                // Some tools emit a bare empty line for empty context.
                None => (LineKind::Context, ""),
                _ => continue, // e.g. "\ No newline" variants or trailing noise
            };
            let (o, n) = match kind {
                LineKind::Added => {
                    let n_now = Some(new_no);
                    new_no += 1;
                    (None, n_now)
                }
                LineKind::Removed => {
                    let o_now = Some(old_no);
                    old_no += 1;
                    (o_now, None)
                }
                LineKind::Context => {
                    let pair = (Some(old_no), Some(new_no));
                    old_no += 1;
                    new_no += 1;
                    pair
                }
            };
            h.lines.push(DiffLine {
                kind,
                old_lineno: o,
                new_lineno: n,
                text: text.to_string(),
                no_newline: false,
            });
        }
    }
    flush_file(&mut files, &mut cur, &mut hunk);

    if !saw_anything {
        return Err(DiffError::NotADiff);
    }
    Ok(files)
}

fn clean_ab_path(s: &str) -> Option<String> {
    let s = s.trim();
    if s == "/dev/null" {
        return None;
    }
    // Strip a trailing tab + timestamp (plain `diff -u` output).
    let s = s.split('\t').next().unwrap_or(s);
    Some(s.trim_start_matches("a/").trim_start_matches("b/").to_string())
}

fn split_git_paths(rest: &str) -> (Option<String>, Option<String>) {
    // "a/path b/path"; paths with spaces are ambiguous without quoting, so fall back
    // to a simple split on " b/" which is what git emits for unquoted paths.
    let rest = rest.trim();
    if let Some(idx) = rest.find(" b/") {
        let a = rest[..idx].trim_start_matches("a/").to_string();
        let b = rest[idx + 3..].to_string();
        (Some(a), Some(b))
    } else {
        (Some(rest.to_string()), Some(rest.to_string()))
    }
}

/// Parse `-1,5 +1,6 @@ context` (the text after `@@ `).
#[allow(clippy::type_complexity)]
fn parse_hunk_header(
    rest: &str,
) -> Result<((u32, u32), (u32, u32), Option<String>), DiffError> {
    // rest looks like: "-1,5 +1,6 @@ fn main()"  or  "-1 +1 @@"
    let (ranges, tail) = match rest.find("@@") {
        Some(i) => (&rest[..i], Some(rest[i + 2..].trim().to_string())),
        None => (rest, None),
    };
    let mut parts = ranges.split_whitespace();
    let old = parts
        .next()
        .ok_or_else(|| DiffError::MalformedHunk(rest.to_string()))?;
    let new = parts
        .next()
        .ok_or_else(|| DiffError::MalformedHunk(rest.to_string()))?;
    let parse = |s: &str| -> Result<(u32, u32), DiffError> {
        let s = s
            .trim_start_matches('-')
            .trim_start_matches('+');
        let mut it = s.split(',');
        let start: u32 = it
            .next()
            .unwrap_or("0")
            .parse()
            .map_err(|_| DiffError::MalformedHunk(rest.to_string()))?;
        let len: u32 = match it.next() {
            Some(v) => v.parse().map_err(|_| DiffError::MalformedHunk(rest.to_string()))?,
            None => 1,
        };
        Ok((start, len))
    };
    let header = tail.filter(|t| !t.is_empty());
    Ok((parse(old)?, parse(new)?, header))
}

// ---------------------------------------------------------------- side-by-side

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SideCell {
    Line { lineno: Option<u32>, text: String, kind: LineKind },
    /// Folded run of unchanged lines.
    Fold { count: u32 },
    Blank,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SideRow {
    pub left: SideCell,
    pub right: SideCell,
}

/// Build a side-by-side view for one file, pairing each run of removals with the
/// following run of additions. This is what makes a rewrite readable: 3 old lines and
/// 3 new lines sit opposite each other instead of as six stacked lines.
pub fn side_by_side(file: &FileDiff, context: u32) -> Vec<SideRow> {
    let mut rows = Vec::new();
    for (hi, h) in file.hunks.iter().enumerate() {
        if hi > 0 {
            rows.push(SideRow { left: SideCell::Fold { count: 0 }, right: SideCell::Fold { count: 0 } });
        }
        let lines = &h.lines;
        let mut i = 0usize;
        let mut ctx_run: u32 = 0;
        while i < lines.len() {
            match lines[i].kind {
                LineKind::Context => {
                    ctx_run += 1;
                    if ctx_run > context {
                        // Collapse the middle of a long unchanged run.
                        let start = i;
                        while i < lines.len() && lines[i].kind == LineKind::Context {
                            i += 1;
                        }
                        let run = (i - start) as u32;
                        let keep = context;
                        for l in &lines[start..start + keep as usize] {
                            rows.push(row_for(l, l));
                        }
                        rows.push(SideRow {
                            left: SideCell::Fold { count: run - keep * 2 },
                            right: SideCell::Fold { count: run - keep * 2 },
                        });
                        for l in &lines[i - keep as usize..i] {
                            rows.push(row_for(l, l));
                        }
                        ctx_run = 0;
                    } else {
                        let l = &lines[i];
                        rows.push(row_for(l, l));
                        i += 1;
                    }
                }
                _ => {
                    ctx_run = 0;
                    let start = i;
                    while i < lines.len() && lines[i].kind == LineKind::Removed {
                        i += 1;
                    }
                    let removed = &lines[start..i];
                    let astart = i;
                    while i < lines.len() && lines[i].kind == LineKind::Added {
                        i += 1;
                    }
                    let added = &lines[astart..i];
                    let n = removed.len().max(added.len());
                    for k in 0..n {
                        let left = removed
                            .get(k)
                            .map(|l| SideCell::Line {
                                lineno: l.old_lineno,
                                text: l.text.clone(),
                                kind: LineKind::Removed,
                            })
                            .unwrap_or(SideCell::Blank);
                        let right = added
                            .get(k)
                            .map(|l| SideCell::Line {
                                lineno: l.new_lineno,
                                text: l.text.clone(),
                                kind: LineKind::Added,
                            })
                            .unwrap_or(SideCell::Blank);
                        rows.push(SideRow { left, right });
                    }
                }
            }
        }
    }
    rows
}

fn row_for(l: &DiffLine, _other: &DiffLine) -> SideRow {
    let cell = SideCell::Line {
        lineno: l.old_lineno,
        text: l.text.clone(),
        kind: l.kind,
    };
    SideRow { left: cell.clone(), right: cell }
}

// ------------------------------------------------------------- inline diffing

/// Character ranges that differ between two lines, as `(start, end)` byte offsets.
/// Used to colour the changed middle of a line instead of the whole line.
pub fn inline_diff(old: &str, new: &str) -> (Vec<(usize, usize)>, Vec<(usize, usize)>) {
    let a: Vec<char> = old.chars().collect();
    let b: Vec<char> = new.chars().collect();
    // LCS table over characters; lines are short so O(n*m) is fine and predictable.
    let (n, m) = (a.len(), b.len());
    if n == 0 || m == 0 {
        return if n > 0 { (vec![(0, n)], vec![]) } else if m > 0 { (vec![], vec![(0, m)]) } else { (vec![], vec![]) };
    }
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut old_keep = vec![false; n];
    let mut new_keep = vec![false; m];
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if a[i] == b[j] {
            old_keep[i] = true;
            new_keep[j] = true;
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    (ranges_of_false(&old_keep), ranges_of_false(&new_keep))
}

fn ranges_of_false(keep: &[bool]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, k) in keep.iter().enumerate() {
        match (k, start) {
            (false, None) => start = Some(i),
            (true, Some(s)) => {
                out.push((s, i));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push((s, keep.len()));
    }
    out
}

/// Character indices (not bytes) to the byte offset used by `inline_diff`.
pub fn char_range_to_bytes(s: &str, ranges: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let offsets: Vec<usize> = s.char_indices().map(|(b, _)| b).chain(std::iter::once(s.len())).collect();
    ranges
        .iter()
        .map(|(a, b)| {
            let s0 = *offsets.get(*a).unwrap_or(&s.len());
            let s1 = *offsets.get(*b).unwrap_or(&s.len());
            (s0, s1)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
diff --git a/src/main.rs b/src/main.rs
index 1111111..2222222 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,5 +1,6 @@ fn main()
 fn main() {
-    println!(\"old\");
+    println!(\"new\");
+    // added
     let x = 1;
 }
@@ -20,3 +21,3 @@
 a
-b
+B
 c
";

    #[test]
    fn parses_files_hunks_and_line_numbers() {
        let files = parse(SAMPLE).unwrap();
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f.display_path(), "src/main.rs");
        assert_eq!(f.status, FileStatus::Modified);
        assert_eq!(f.hunks.len(), 2);
        let h = &f.hunks[0];
        assert_eq!((h.old_start, h.old_len, h.new_start, h.new_len), (1, 5, 1, 6));
        assert_eq!(h.header.as_deref(), Some("fn main()"));
        assert_eq!(f.counts(), (3, 2));
        // removed line keeps old number, added line keeps new number
        let removed = h.lines.iter().find(|l| l.kind == LineKind::Removed).unwrap();
        assert_eq!(removed.old_lineno, Some(2));
        assert_eq!(removed.new_lineno, None);
        let added = h.lines.iter().find(|l| l.kind == LineKind::Added).unwrap();
        assert_eq!(added.new_lineno, Some(2));
        assert_eq!(added.old_lineno, None);
    }

    #[test]
    fn detects_added_and_deleted_files() {
        let add = "diff --git a/n.rs b/n.rs\nnew file mode 100644\n--- /dev/null\n+++ b/n.rs\n@@ -0,0 +1,1 @@\n+hi\n";
        let f = parse(add).unwrap();
        assert_eq!(f[0].status, FileStatus::Added);
        assert_eq!(f[0].old_path, None);

        let del = "diff --git a/d.rs b/d.rs\ndeleted file mode 100644\n--- a/d.rs\n+++ /dev/null\n@@ -1,1 +0,0 @@\n-bye\n";
        let f = parse(del).unwrap();
        assert_eq!(f[0].status, FileStatus::Deleted);
        assert_eq!(f[0].display_path(), "d.rs");
    }

    #[test]
    fn rejects_non_diff_input() {
        assert_eq!(parse("hello world\n").unwrap_err(), DiffError::NotADiff);
        assert_eq!(parse("").unwrap_err(), DiffError::NotADiff);
    }

    #[test]
    fn side_by_side_pairs_removals_with_additions() {
        let files = parse(SAMPLE).unwrap();
        let rows = side_by_side(&files[0], 3);
        // First hunk: context, removed+added paired on one row, then the extra addition.
        let paired = rows
            .iter()
            .find(|r| {
                matches!(&r.left, SideCell::Line { kind: LineKind::Removed, .. })
                    && matches!(&r.right, SideCell::Line { kind: LineKind::Added, .. })
            })
            .expect("a removal must be paired with an addition on the same row");
        if let (SideCell::Line { text: l, .. }, SideCell::Line { text: r, .. }) = (&paired.left, &paired.right) {
            assert!(l.contains("old"));
            assert!(r.contains("new"));
        }
        // The second addition has no counterpart: right only.
        let right_only = rows.iter().find(|r| {
            matches!(r.left, SideCell::Blank) && matches!(&r.right, SideCell::Line { kind: LineKind::Added, .. })
        });
        assert!(right_only.is_some(), "unpaired addition should leave a blank left cell");
    }

    #[test]
    fn long_context_runs_fold() {
        let mut diff = String::from("--- a/f\n+++ b/f\n@@ -1,40 +1,41 @@\n");
        for i in 0..20 {
            diff.push_str(&format!(" line{i}\n"));
        }
        diff.push_str("+added\n");
        for i in 20..40 {
            diff.push_str(&format!(" line{i}\n"));
        }
        let files = parse(&diff).unwrap();
        let rows = side_by_side(&files[0], 2);
        let folds: Vec<u32> = rows
            .iter()
            .filter_map(|r| match r.left {
                SideCell::Fold { count } => Some(count),
                _ => None,
            })
            .collect();
        assert!(!folds.is_empty(), "long unchanged region must fold");
        assert!(folds.iter().any(|c| *c > 0), "fold should report how many lines are hidden");
    }

    #[test]
    fn inline_diff_marks_only_changed_characters() {
        let (o, n) = inline_diff("let x = 1;", "let x = 2;");
        let ob = char_range_to_bytes("let x = 1;", &o);
        let nb = char_range_to_bytes("let x = 2;", &n);
        assert_eq!(&"let x = 1;"[ob[0].0..ob[0].1], "1");
        assert_eq!(&"let x = 2;"[nb[0].0..nb[0].1], "2");
    }

    #[test]
    fn inline_diff_handles_unicode_without_panicking() {
        let a = "let s = \"héllo\";";
        let b = "let s = \"héllo wörld\";";
        let (o, n) = inline_diff(a, b);
        let nb = char_range_to_bytes(b, &n);
        for (s, e) in nb {
            assert!(b.is_char_boundary(s) && b.is_char_boundary(e));
        }
        let _ = char_range_to_bytes(a, &o);
    }

    #[test]
    fn inline_diff_whole_line_replacements() {
        let (_, n) = inline_diff("aaaa", "bbbb");
        assert_eq!(n, vec![(0, 4)]);
        let (o2, n2) = inline_diff("", "new");
        assert_eq!(o2, vec![]);
        assert_eq!(n2, vec![(0, 3)]);
    }

    #[test]
    fn side_by_side_never_loses_lines() {
        let files = parse(SAMPLE).unwrap();
        let total_lines: usize = files[0].hunks.iter().map(|h| h.lines.len()).sum();
        let rows = side_by_side(&files[0], 100); // no folding
        let shown = rows
            .iter()
            .map(|r| {
                let mut n = 0;
                if matches!(r.left, SideCell::Line { .. }) {
                    n += 1;
                }
                if matches!(r.right, SideCell::Line { .. }) {
                    n += 1;
                }
                n
            })
            .sum::<usize>();
        // Context lines appear on both sides, so shown >= total.
        assert!(shown >= total_lines, "shown {shown} < source {total_lines}");
    }
}
