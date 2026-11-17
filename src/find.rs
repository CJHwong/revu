//! What a find looks for: plain text, ignoring case, with `^` and `$` to tie it to where
//! the code starts or ends. Diff lines carry line numbers and +/- marks before the code,
//! so the anchors skip them rather than match the raw line.

use ratatui::style::Modifier;
use ratatui::text::Line;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    /// The text to find, lowercased, without its anchors.
    pub text: String,
    /// `^`: the code starts with the text, after its indentation.
    pub at_start: bool,
    /// `$`: the code ends with the text, before trailing spaces.
    pub at_end: bool,
}

impl Pattern {
    /// `^` at the start and `$` at the end are anchors; `\^` and `\$` find the characters.
    pub fn parse(query: &str) -> Pattern {
        let mut text = query.to_lowercase();
        let at_start = text.starts_with('^');
        // An anchor goes, and so does the backslash of an escaped one.
        if at_start || text.starts_with("\\^") {
            text.remove(0);
        }
        let at_end = text.ends_with('$') && !text.ends_with("\\$");
        if at_end {
            text.pop();
        } else if text.ends_with("\\$") {
            text.remove(text.len() - 2);
        }
        Pattern {
            text,
            at_start,
            at_end,
        }
    }

    fn anchored(&self) -> bool {
        self.at_start || self.at_end
    }

    /// Whether a line as a renderer drew it matches. Dim text is a gutter, such as line
    /// numbers, so each run of other text is one stretch of code; difftastic's two
    /// columns are two.
    pub fn matches_drawn(&self, line: &Line) -> bool {
        if !self.anchored() {
            return self.matches_code(&plain(line));
        }
        code_runs(line).iter().any(|code| self.matches_code(code))
    }

    /// Whether a line of a plain `git diff` matches; its first +, - or space is a mark,
    /// not code.
    pub fn matches_raw(&self, line: &str) -> bool {
        if !self.anchored() {
            return self.matches_code(line);
        }
        let code = line.strip_prefix(['+', '-', ' ']).unwrap_or(line);
        self.matches_code(code)
    }

    fn matches_code(&self, code: &str) -> bool {
        let code = code.to_lowercase();
        let code = match (self.at_start, self.at_end) {
            (false, false) => return code.contains(&self.text),
            (true, false) => return code.trim_start().starts_with(&self.text),
            (false, true) => return code.trim_end().ends_with(&self.text),
            (true, true) => code.trim(),
        };
        code == self.text
    }
}

fn plain(line: &Line) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

/// The runs of text between dim stretches, such as the code beside line numbers.
fn code_runs(line: &Line) -> Vec<String> {
    let mut runs = vec![String::new()];
    for span in &line.spans {
        let style = line.style.patch(span.style);
        if style.add_modifier.contains(Modifier::DIM) {
            runs.push(String::new());
        } else if let Some(run) = runs.last_mut() {
            run.push_str(&span.content);
        }
    }
    runs.retain(|run| !run.trim().is_empty());
    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;
    use ratatui::text::Span;

    #[test]
    fn anchors_parse_and_escapes_stay_literal() {
        let pattern = Pattern::parse("^Def$");
        assert_eq!(
            (pattern.text.as_str(), pattern.at_start, pattern.at_end),
            ("def", true, true)
        );
        let literal = Pattern::parse("\\^a\\$");
        assert_eq!(
            (literal.text.as_str(), literal.at_start, literal.at_end),
            ("^a$", false, false)
        );
    }

    #[test]
    fn a_start_anchor_skips_indentation_and_the_diff_mark() {
        let pattern = Pattern::parse("^def ");
        assert!(pattern.matches_raw("+    def names_ga(types):"));
        assert!(!pattern.matches_raw("+    x = def_value"));
        assert!(Pattern::parse(":$").matches_raw("-    if ready:   "));
    }

    #[test]
    fn each_column_beside_dim_line_numbers_is_its_own_code() {
        let dim = Style::new().add_modifier(Modifier::DIM);
        let line = Line::from(vec![
            Span::styled(" 12 ", dim),
            Span::raw("    return x"),
            Span::styled(" 13 ", dim),
            Span::raw("def y():"),
        ]);
        assert!(
            Pattern::parse("^def").matches_drawn(&line),
            "the right column"
        );
        assert!(
            Pattern::parse("^return").matches_drawn(&line),
            "the left column"
        );
        assert!(
            !Pattern::parse("^12").matches_drawn(&line),
            "line numbers are not code"
        );
    }
}
