//! One file's diff as rows that know their line numbers, so a row on screen maps to the
//! side and line a GitHub line comment takes. Renderers such as delta and difftastic
//! draw lines their own way, so select lines reads the plain diff instead.

use crate::model::Side;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// An `@@ -a,b +c,d @@` line.
    Hunk,
    Context,
    Added,
    Removed,
    /// `\ No newline at end of file` and the like.
    Note,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRow {
    pub kind: RowKind,
    /// The line without its +, - or space mark.
    pub text: String,
    pub old: Option<u64>,
    pub new: Option<u64>,
}

impl DiffRow {
    /// Where a comment on this row goes: removed lines on the old side, the rest on the
    /// new side, as GitHub places them.
    pub fn target(&self) -> Option<(Side, u64)> {
        match self.kind {
            RowKind::Removed => self.old.map(|line| (Side::Left, line)),
            RowKind::Added | RowKind::Context => self.new.map(|line| (Side::Right, line)),
            RowKind::Hunk | RowKind::Note => None,
        }
    }
}

/// The rows of every hunk in a plain `git diff`; the file headers above the first hunk
/// are left out.
pub fn parse(diff: &str) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    let (mut old, mut new) = (0, 0);
    let mut in_hunk = false;
    for line in diff.lines() {
        if let Some((old_start, new_start)) = hunk_starts(line) {
            (old, new, in_hunk) = (old_start, new_start, true);
            rows.push(row(RowKind::Hunk, line, None, None));
            continue;
        }
        if !in_hunk {
            continue;
        }
        let (mark, text) = line.split_at(line.chars().next().map_or(0, char::len_utf8));
        let next = match mark {
            "+" => row(RowKind::Added, text, None, Some(new)),
            "-" => row(RowKind::Removed, text, Some(old), None),
            " " | "" => row(RowKind::Context, text, Some(old), Some(new)),
            "\\" => row(RowKind::Note, line, None, None),
            // A new file's header, when several files are in one diff.
            _ => {
                in_hunk = false;
                continue;
            }
        };
        old += u64::from(next.old.is_some());
        new += u64::from(next.new.is_some());
        rows.push(next);
    }
    rows
}

fn row(kind: RowKind, text: &str, old: Option<u64>, new: Option<u64>) -> DiffRow {
    DiffRow {
        kind,
        text: text.to_string(),
        old,
        new,
    }
}

/// The first old and new line numbers from `@@ -12,3 +14,5 @@ fn name`.
fn hunk_starts(line: &str) -> Option<(u64, u64)> {
    let ranges = line.strip_prefix("@@ ")?.split(" @@").next()?;
    let (old, new) = ranges.split_once(' ')?;
    let start = |range: &str, mark: char| -> Option<u64> {
        let range = range.strip_prefix(mark)?;
        range.split(',').next()?.parse().ok()
    };
    Some((start(old, '-')?, start(new, '+')?))
}

/// Where the rows sit: `path:40-46` by their new line numbers, or by the old ones with
/// "(removed)" when every row was removed.
pub fn place(path: &str, rows: &[DiffRow]) -> String {
    let new: Vec<u64> = rows.iter().filter_map(|row| row.new).collect();
    let (numbers, suffix) = match new.is_empty() {
        true => (
            rows.iter().filter_map(|row| row.old).collect(),
            " (removed)",
        ),
        false => (new, ""),
    };
    match (numbers.first(), numbers.last()) {
        (Some(first), Some(last)) if first != last => format!("{path}:{first}-{last}{suffix}"),
        (Some(line), _) => format!("{path}:{line}{suffix}"),
        _ => path.to_string(),
    }
}

/// The rows as Markdown: their place, then a fenced diff block with the +/- marks.
pub fn snippet(path: &str, rows: &[DiffRow]) -> String {
    let lines: Vec<String> = rows
        .iter()
        .map(|row| match row.kind {
            RowKind::Added => format!("+{}", row.text),
            RowKind::Removed => format!("-{}", row.text),
            RowKind::Context => format!(" {}", row.text),
            RowKind::Hunk | RowKind::Note => row.text.clone(),
        })
        .collect();
    // Longer than any run of backticks in the code, so the block cannot end early.
    let longest = lines
        .iter()
        .map(|line| backtick_run(line))
        .max()
        .unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    let body = lines.join("\n");
    format!("{}\n{fence}diff\n{body}\n{fence}\n", place(path, rows))
}

fn backtick_run(line: &str) -> usize {
    line.split(|char| char != '`')
        .map(str::len)
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "diff --git a/a.rs b/a.rs\n\
index 1..2 100644\n\
--- a/a.rs\n\
+++ b/a.rs\n\
@@ -10,3 +10,3 @@ fn main() {\n \
keep\n\
-old\n\
+new\n \
end\n\
\\ No newline at end of file\n";

    #[test]
    fn rows_carry_the_line_numbers_github_comments_take() {
        let rows = parse(DIFF);
        let kinds: Vec<RowKind> = rows.iter().map(|row| row.kind).collect();
        use RowKind::*;
        assert_eq!(kinds, [Hunk, Context, Removed, Added, Context, Note]);
        let targets: Vec<Option<(Side, u64)>> = rows.iter().map(DiffRow::target).collect();
        assert_eq!(
            targets,
            [
                None,
                Some((Side::Right, 10)),
                Some((Side::Left, 11)),
                Some((Side::Right, 11)),
                Some((Side::Right, 12)),
                None
            ]
        );
        assert_eq!(rows[2].text, "old");
    }

    #[test]
    fn a_snippet_names_its_lines_and_keeps_the_diff_marks() {
        let rows = parse(DIFF);
        assert_eq!(
            snippet("a.rs", &rows[1..5]),
            "a.rs:10-12\n```diff\n keep\n-old\n+new\n end\n```\n"
        );
        assert_eq!(
            snippet("a.rs", &rows[2..3]),
            "a.rs:11 (removed)\n```diff\n-old\n```\n",
            "removed lines alone are named by their old numbers"
        );
        assert_eq!(place("a.rs", &rows[3..4]), "a.rs:11");
        let fenced = parse("@@ -1 +1 @@\n+```rust\n");
        assert!(
            snippet("r.md", &fenced[1..]).starts_with("r.md:1\n````diff\n"),
            "a fence in the code gets a longer fence"
        );
    }

    #[test]
    fn a_new_file_counts_from_one_and_a_single_line_range_has_no_comma() {
        let rows = parse("--- /dev/null\n+++ b/n.md\n@@ -0,0 +1 @@\n+only\n");
        assert_eq!(rows[1].target(), Some((Side::Right, 1)));
    }
}
