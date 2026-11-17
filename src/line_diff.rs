//! One file's diff as rows that know their line numbers, so a row on screen maps to the
//! side and line a GitHub line comment takes. Renderers such as delta and difftastic
//! draw lines their own way, so comment mode reads the plain diff instead.

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
    fn a_new_file_counts_from_one_and_a_single_line_range_has_no_comma() {
        let rows = parse("--- /dev/null\n+++ b/n.md\n@@ -0,0 +1 @@\n+only\n");
        assert_eq!(rows[1].target(), Some((Side::Right, 1)));
    }
}
