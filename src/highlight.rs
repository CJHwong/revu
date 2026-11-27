//! Syntax colors under a diff renderer's output. git's word diff and difftastic mark what
//! changed but color little else; `bat` colors the code, and the change marks become a
//! background tint so both show.

use ansi_to_tui::IntoText;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};

use crate::repo;

/// How to color: the `bat` theme and the tints for added and removed text.
#[derive(Debug, Clone, Copy)]
pub struct Syntax {
    pub bat_theme: &'static str,
    pub added: Color,
    pub removed: Color,
}

/// The renderer's ANSI output as styled text, with tabs expanded. With `syntax`, the code
/// lines get `bat`'s colors; without `bat`, or for a line `bat` changed, they stay as drawn.
pub fn to_text(bytes: &[u8], syntax: Option<Syntax>) -> Result<Text<'static>, String> {
    // The terminal draws a tab as one cell, so expand it like an editor would.
    let text = String::from_utf8_lossy(bytes)
        .replace('\t', "    ")
        .into_bytes()
        .into_text()
        .map_err(|error| format!("bad renderer output: {error}"))?;
    Ok(match syntax {
        Some(syntax) => add_syntax(text, syntax),
        None => text,
    })
}

/// True when the renderer printed no diff: no lines, or only lines with no text. A
/// renderer that fails inside a pipeline prints nothing, and an empty diff prints
/// nothing too, so the pane needs one test for both.
pub fn shows_nothing(text: &Text<'static>) -> bool {
    text.lines
        .iter()
        .all(|line| line.spans.iter().all(|span| span.content.trim().is_empty()))
}

fn add_syntax(mut text: Text<'static>, syntax: Syntax) -> Text<'static> {
    let plain: Vec<String> = text.lines.iter().map(plain_line).collect();
    for (path, indexes) in sections(&plain) {
        // difftastic draws two files side by side; each column gets its own `bat` run, so
        // a comment on the left cannot color the code on the right.
        let (left, right): (Vec<String>, Vec<String>) = indexes
            .iter()
            .map(|index| split_columns(&text.lines[*index], &plain[*index]))
            .unzip();
        let (Some(left), Some(right)) = (
            bat_column(&left, &path, syntax.bat_theme),
            bat_column(&right, &path, syntax.bat_theme),
        ) else {
            continue;
        };
        let code_background = most_common_background(left.iter().chain(&right));
        for ((index, left), right) in indexes.into_iter().zip(left).zip(right) {
            let colored = Line::from([left.spans, right.spans].concat());
            if let Some(merged) = merge(&text.lines[index], &colored, syntax) {
                text.lines[index] = merged;
            }
            let line = &mut text.lines[index];
            if let Some(background) = row_background(line, code_background, syntax) {
                line.style = line.style.bg(background);
            }
        }
    }
    text
}

/// The background `bat`'s theme gives code, when it gives one: the most common one.
fn most_common_background<'a>(lines: impl Iterator<Item = &'a Line<'static>>) -> Option<Color> {
    let mut counts: Vec<(Color, usize)> = Vec::new();
    for color in lines.flat_map(|line| line.spans.iter().filter_map(|span| span.style.bg)) {
        match counts.iter_mut().find(|(seen, _)| *seen == color) {
            Some((_, count)) => *count += 1,
            None => counts.push((color, 1)),
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(color, _)| color)
}

/// The background a code row fills to the pane's edge: the change tint when every
/// character on it changed, else the code's background. The diff pane paints it.
fn row_background(line: &Line<'static>, code: Option<Color>, syntax: Syntax) -> Option<Color> {
    let mut tints = cells(line)
        .into_iter()
        .filter(|(character, _)| !character.is_whitespace())
        .map(|(_, style)| style.bg);
    let first = tints.next().flatten();
    let changed = matches!(first, Some(tint) if tint == syntax.added || tint == syntax.removed);
    match changed && tints.all(|tint| tint == first) {
        true => first,
        false => code,
    }
}

/// A line's text before and after the right column's line numbers: the second run of dim
/// text. A one-column line is all left.
fn split_columns(line: &Line<'static>, plain: &str) -> (String, String) {
    let mut runs = 0;
    let mut was_dim = false;
    let mut split = None;
    for (position, (_, style)) in cells(line).into_iter().enumerate() {
        let dim = style.add_modifier.contains(Modifier::DIM);
        if dim && !was_dim {
            runs += 1;
        }
        if runs == 2 {
            split = Some(position);
            break;
        }
        was_dim = dim;
    }
    let split = split.unwrap_or(usize::MAX);
    (
        plain.chars().take(split).collect(),
        plain.chars().skip(split).collect(),
    )
}

/// `bat`'s colors for one column, one line per input line, or None when `bat` fails or
/// the line count differs.
fn bat_column(column: &[String], path: &str, theme: &str) -> Option<Vec<Line<'static>>> {
    if column.iter().all(String::is_empty) {
        return Some(vec![Line::default(); column.len()]);
    }
    // End every line with a newline, so `bat` keeps an empty last line.
    let mut colored = bat_lines(&(column.join("\n") + "\n"), path, theme)?;
    colored.resize(column.len(), Line::default());
    Some(colored)
}

fn plain_line(line: &Line) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

/// Groups the code lines by file, so each file gets its own language. Header lines, such
/// as `diff --git` and `@@`, belong to no section and keep the renderer's look.
fn sections(plain: &[String]) -> Vec<(String, Vec<usize>)> {
    let git = plain.iter().any(|line| line.starts_with("diff --git "));
    let mut sections: Vec<(String, Vec<usize>)> = Vec::new();
    for (index, line) in plain.iter().enumerate() {
        if let Some(path) = section_path(line, git) {
            sections.push((path, Vec::new()));
        } else if !is_header(line, git)
            && let Some((_, indexes)) = sections.last_mut()
        {
            indexes.push(index);
        }
    }
    sections
}

/// git's "diff --git a/x b/x", or difftastic's "x --- Bash" and "x --- 2/5 --- Bash".
fn section_path(line: &str, git: bool) -> Option<String> {
    if git {
        let paths = line.strip_prefix("diff --git ")?;
        return paths.rsplit_once(" b/").map(|(_, path)| path.to_string());
    }
    let (path, _) = line.split_once(" --- ")?;
    (!path.is_empty() && !path.starts_with(' ')).then(|| path.to_string())
}

fn is_header(line: &str, git: bool) -> bool {
    const GIT_HEADERS: [&str; 12] = [
        "index ",
        "--- ",
        "+++ ",
        "@@",
        "new file mode",
        "deleted file mode",
        "similarity index",
        "rename from",
        "rename to",
        "old mode",
        "new mode",
        "Binary files",
    ];
    git && GIT_HEADERS.iter().any(|header| line.starts_with(header))
}

/// The code as `bat` colors it, one line per input line, or None when `bat` is missing or
/// fails.
fn bat_lines(code: &str, path: &str, theme: &str) -> Option<Vec<Line<'static>>> {
    let args = [
        "--color=always",
        "--style=plain",
        "--paging=never",
        "--wrap=never",
        "--tabs=0",
        "--file-name",
        path,
        "--theme",
        theme,
    ];
    let output = repo::bat(code.to_string(), &args).ok()?;
    Some(output.into_text().ok()?.lines)
}

/// One line with `bat`'s colors where the renderer drew plain code, and the change tint
/// where it drew red or green. None when the two disagree on the text.
fn merge(drawn: &Line<'static>, colored: &Line<'static>, syntax: Syntax) -> Option<Line<'static>> {
    let drawn_cells = cells(drawn);
    let colored_cells = cells(colored);
    let same_text = drawn_cells.len() == colored_cells.len()
        && drawn_cells
            .iter()
            .zip(&colored_cells)
            .all(|(a, b)| a.0 == b.0);
    if !same_text {
        return None;
    }
    let mut spans: Vec<Span<'static>> = Vec::new();
    for ((character, drawn), (_, colored)) in drawn_cells.into_iter().zip(colored_cells) {
        let style = cell_style(drawn, colored, syntax);
        match spans.last_mut() {
            Some(span) if span.style == style => span.content.to_mut().push(character),
            _ => spans.push(Span::styled(character.to_string(), style)),
        }
    }
    Some(Line::from(spans))
}

fn cell_style(drawn: Style, colored: Style, syntax: Syntax) -> Style {
    // Dim text is a line number or a gutter; it keeps the renderer's look.
    if drawn.add_modifier.contains(Modifier::DIM) {
        return drawn;
    }
    let tint = match drawn.fg {
        Some(Color::Red | Color::LightRed) => Some(syntax.removed),
        Some(Color::Green | Color::LightGreen) => Some(syntax.added),
        _ => None,
    };
    match tint {
        // Bold and underline mark the changed words; they stay.
        Some(tint) => colored.bg(tint).add_modifier(drawn.add_modifier),
        None => colored,
    }
}

fn cells(line: &Line<'static>) -> Vec<(char, Style)> {
    line.spans
        .iter()
        .flat_map(|span| {
            let style = line.style.patch(span.style);
            span.content
                .chars()
                .map(move |character| (character, style))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYNTAX: Syntax = Syntax {
        bat_theme: "ansi",
        added: Color::Indexed(22),
        removed: Color::Indexed(52),
    };

    #[test]
    fn sections_follow_files_and_skip_headers() {
        let git: Vec<String> = [
            "diff --git a/x.rs b/x.rs",
            "index 1..2 100644",
            "--- a/x.rs",
            "+++ b/x.rs",
            "@@ -1 +1 @@",
            "fn main() {}",
            "diff --git a/y.sh b/y.sh",
            "echo hi",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            sections(&git),
            [("x.rs".to_string(), vec![5]), ("y.sh".to_string(), vec![7])]
        );
        let difftastic: Vec<String> = [
            "a.sh --- 1/2 --- Bash",
            "  1 echo",
            "a.sh --- 2/2 --- Bash",
            "  9 exit",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            sections(&difftastic),
            [("a.sh".to_string(), vec![1]), ("a.sh".to_string(), vec![3])]
        );
    }

    #[test]
    fn a_change_keeps_its_tint_and_emphasis_and_numbers_stay_dim() {
        let drawn = Line::from(vec![
            Span::styled(" 2 ", Style::new().add_modifier(Modifier::DIM)),
            Span::styled("let", Style::new()),
            Span::styled(
                " x",
                Style::new()
                    .fg(Color::LightGreen)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        let colored = Line::from(vec![
            Span::styled(" 2 ", Style::new().fg(Color::Blue)),
            Span::styled("let", Style::new().fg(Color::Magenta)),
            Span::styled(" x", Style::new().fg(Color::White)),
        ]);
        let merged = merge(&drawn, &colored, SYNTAX).unwrap();
        let styles: Vec<Style> = merged.spans.iter().map(|span| span.style).collect();
        assert_eq!(
            styles[0],
            Style::new().add_modifier(Modifier::DIM),
            "the line number keeps its look"
        );
        assert_eq!(
            styles[1],
            Style::new().fg(Color::Magenta),
            "plain code takes bat's color"
        );
        assert_eq!(
            styles[2],
            Style::new()
                .fg(Color::White)
                .bg(Color::Indexed(22))
                .add_modifier(Modifier::BOLD),
            "an added word gets bat's color on the added tint, still bold"
        );
    }

    #[test]
    fn a_row_takes_the_change_tint_only_when_all_of_it_changed() {
        let gray = Some(Color::Rgb(40, 40, 40));
        let on = |bg: Color| Style::new().bg(bg);
        let added = Line::from(vec![
            Span::styled("  ", on(SYNTAX.added)),
            Span::styled("let x", on(SYNTAX.added)),
        ]);
        assert_eq!(row_background(&added, gray, SYNTAX), Some(SYNTAX.added));
        let one_word = Line::from(vec![
            Span::styled("let ", Style::new()),
            Span::styled("x", on(SYNTAX.added)),
        ]);
        assert_eq!(
            row_background(&one_word, gray, SYNTAX),
            gray,
            "a changed word"
        );
        assert_eq!(
            row_background(&Line::from(""), gray, SYNTAX),
            gray,
            "a blank line"
        );
        assert_eq!(
            row_background(&Line::from("x"), None, SYNTAX),
            None,
            "a theme without one"
        );
        let lines = [
            Line::from(Span::styled("a", on(Color::Red))),
            Line::from(vec![
                Span::styled("b", on(Color::Blue)),
                Span::styled("c", on(Color::Blue)),
            ]),
        ];
        assert_eq!(most_common_background(lines.iter()), Some(Color::Blue));
    }

    #[test]
    fn a_side_by_side_line_splits_at_the_second_line_number() {
        let dim = Style::new().add_modifier(Modifier::DIM);
        let line = Line::from(vec![
            Span::styled(" 1 ", dim),
            Span::raw("# note  "),
            Span::styled(" 1 ", dim),
            Span::raw("x=1"),
        ]);
        let plain = plain_line(&line);
        assert_eq!(
            split_columns(&line, &plain),
            (" 1 # note  ".to_string(), " 1 x=1".to_string())
        );
        let one_column = Line::from(vec![Span::styled(" 1 ", dim), Span::raw("x=1")]);
        let plain = plain_line(&one_column);
        assert_eq!(
            split_columns(&one_column, &plain),
            (plain.clone(), String::new())
        );
    }

    #[test]
    fn bat_keeps_an_empty_last_line() {
        if std::process::Command::new("bat")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let column = ["a=1".to_string(), String::new()];
        let colored = bat_column(&column, "x.sh", "ansi").unwrap();
        assert_eq!(colored.len(), 2);
        assert_eq!(plain_line(&colored[0]), "a=1");
    }

    #[test]
    fn empty_output_still_holds_one_blank_line() {
        let empty = to_text(b"", None).unwrap();
        assert_eq!(empty.lines.len(), 1);
        assert!(shows_nothing(&empty));
        assert!(shows_nothing(&to_text(b"\n \n", None).unwrap()));
        assert!(!shows_nothing(&to_text(b" x\n", None).unwrap()));
    }

    #[test]
    fn different_text_is_left_as_drawn() {
        assert!(merge(&Line::from("a"), &Line::from("b"), SYNTAX).is_none());
    }
}
