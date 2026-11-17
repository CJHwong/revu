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

fn add_syntax(mut text: Text<'static>, syntax: Syntax) -> Text<'static> {
    let plain: Vec<String> = text.lines.iter().map(plain_line).collect();
    for (path, indexes) in sections(&plain) {
        let code: Vec<&str> = indexes.iter().map(|index| plain[*index].as_str()).collect();
        // End every line with a newline, so `bat` keeps an empty last line.
        let Some(mut colored) = bat_lines(&(code.join("\n") + "\n"), &path, syntax.bat_theme)
        else {
            continue;
        };
        colored.truncate(indexes.len());
        if colored.len() != indexes.len() {
            continue;
        }
        for (index, colored) in indexes.into_iter().zip(colored) {
            if let Some(merged) = merge(&text.lines[index], &colored, syntax) {
                text.lines[index] = merged;
            }
        }
    }
    text
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
    fn different_text_is_left_as_drawn() {
        assert!(merge(&Line::from("a"), &Line::from("b"), SYNTAX).is_none());
    }
}
