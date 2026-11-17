//! Renders GitHub-flavored Markdown, plus the HTML GitHub allows in descriptions and
//! comments, into terminal lines. A `<details>` block becomes a fold the user opens.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::collections::HashSet;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme::Theme;

type Spans = Vec<Span<'static>>;

/// Width-independent content. `layout` wraps it to a width.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// One logical line. When it wraps, the next lines start `indent` columns in.
    Line {
        spans: Spans,
        indent: usize,
    },
    Blank,
    Rule,
    Details {
        summary: Spans,
        body: Vec<Block>,
        /// `<details open>`, or an unresolved review thread: shown open until toggled.
        open: bool,
    },
    Table {
        header: Vec<Spans>,
        rows: Vec<Vec<Spans>>,
    },
}

/// One screen line. `fold` marks the summary bar of a `<details>` block; `anchor` marks
/// a line that `]` and `[` jump to.
#[derive(Debug, Clone, PartialEq)]
pub struct TextRow {
    pub line: Line<'static>,
    pub fold: Option<usize>,
    pub anchor: bool,
}

impl TextRow {
    pub fn plain(line: Line<'static>) -> Self {
        TextRow {
            line,
            fold: None,
            anchor: false,
        }
    }

    pub fn anchor(line: Line<'static>) -> Self {
        TextRow {
            line,
            fold: None,
            anchor: true,
        }
    }

    pub fn blank() -> Self {
        TextRow::plain(Line::default())
    }

    fn prefixed(mut self, prefix: Span<'static>) -> Self {
        self.line.spans.insert(0, prefix);
        self
    }
}

/// Numbers folds in document order. A fold's number stays the same when an earlier fold
/// opens or closes, so the toggled set stays valid across layouts.
pub struct Folds<'a> {
    pub next: usize,
    /// Folds the user flipped from their default state.
    pub toggled: &'a HashSet<usize>,
}

pub fn parse(markdown: &str, theme: &Theme) -> Vec<Block> {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut builder = Builder::new(theme);
    for event in Parser::new_ext(markdown, options) {
        builder.event(event);
    }
    builder.finish()
}

pub fn layout(blocks: &[Block], width: usize, folds: &mut Folds, theme: &Theme) -> Vec<TextRow> {
    let mut rows = Vec::new();
    for block in blocks {
        layout_block(block, width, folds, theme, &mut rows);
    }
    rows
}

fn layout_block(
    block: &Block,
    width: usize,
    folds: &mut Folds,
    theme: &Theme,
    rows: &mut Vec<TextRow>,
) {
    match block {
        Block::Line { spans, indent } => {
            rows.extend(wrap(spans, width, *indent).into_iter().map(TextRow::plain))
        }
        Block::Blank => rows.push(TextRow::blank()),
        Block::Rule => rows.push(TextRow::plain(Line::styled(
            "─".repeat(width),
            Style::new().fg(theme.muted),
        ))),
        Block::Table { header, rows: body } => {
            rows.extend(
                table(header, body, width, theme)
                    .into_iter()
                    .map(TextRow::plain),
            );
        }
        Block::Details {
            summary,
            body,
            open,
        } => layout_details(summary, body, *open, width, folds, theme, rows),
    }
}

fn layout_details(
    summary: &Spans,
    body: &[Block],
    open_by_default: bool,
    width: usize,
    folds: &mut Folds,
    theme: &Theme,
    rows: &mut Vec<TextRow>,
) {
    let id = folds.next;
    folds.next += 1;
    let open = open_by_default != folds.toggled.contains(&id);
    // The body is laid out even when closed: the bar shows its line count, and the folds
    // inside it use up their numbers either way.
    let body_rows = layout(body, width.saturating_sub(2).max(1), folds, theme);
    let bar = fold_bar(summary, open, body_rows.len(), width, theme);
    rows.push(TextRow {
        line: bar,
        fold: Some(id),
        anchor: true,
    });
    if open {
        let rail = Span::styled("│ ", Style::new().fg(theme.fold));
        rows.extend(body_rows.into_iter().map(|row| row.prefixed(rail.clone())));
    }
}

/// A full-width bar in the fold color, so a closed block is hard to miss.
fn fold_bar(
    summary: &Spans,
    open: bool,
    hidden: usize,
    width: usize,
    theme: &Theme,
) -> Line<'static> {
    let style = Style::new()
        .bg(theme.fold)
        .fg(theme.background)
        .add_modifier(Modifier::BOLD);
    let marker = if open { " ▼ " } else { " ▶ " };
    let hint = if open {
        " enter folds ".to_string()
    } else {
        format!(" {hidden} lines, enter opens ")
    };
    let text: String = summary.iter().map(|span| span.content.as_ref()).collect();
    let text = if text.trim().is_empty() {
        "Details".to_string()
    } else {
        text.trim().to_string()
    };
    let room = width.saturating_sub(marker.width() + hint.width());
    let title = truncate(&text, room);
    let padding = room.saturating_sub(title.width());
    Line::from(vec![
        Span::styled(format!("{marker}{title}{}", " ".repeat(padding)), style),
        Span::styled(hint, style.remove_modifier(Modifier::BOLD)),
    ])
}

// ---- Wrapping ----

/// Wraps styled text at spaces. A word wider than the line is split by character.
pub fn wrap(spans: &[Span<'static>], width: usize, indent: usize) -> Vec<Line<'static>> {
    let width = width.max(indent + 10);
    let mut lines = Vec::new();
    let mut current: Spans = Vec::new();
    let mut used = 0;
    for span in spans {
        for piece in span.content.split_inclusive(' ') {
            let fits = used + piece.trim_end().width() <= width;
            if !fits && used > indent {
                lines.push(Line::from(std::mem::take(&mut current)));
                current.push(Span::raw(" ".repeat(indent)));
                used = indent;
            }
            let piece = if used == indent && !lines.is_empty() {
                piece.trim_start()
            } else {
                piece
            };
            used = push_piece(
                piece,
                span.style,
                width,
                indent,
                &mut current,
                &mut lines,
                used,
            );
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(Line::from(current));
    }
    lines
}

/// Adds one word to the line, splitting it when it alone is wider than the line.
/// Returns the line's new width.
fn push_piece(
    piece: &str,
    style: Style,
    width: usize,
    indent: usize,
    current: &mut Spans,
    lines: &mut Vec<Line<'static>>,
    mut used: usize,
) -> usize {
    let mut chunk = String::new();
    for character in piece.chars() {
        let char_width = character.width().unwrap_or(0);
        let overflows = used + char_width > width;
        // A space that ends a full line is dropped, not carried to the next one.
        if overflows && character == ' ' {
            continue;
        }
        if overflows && used > indent {
            current.push(Span::styled(std::mem::take(&mut chunk), style));
            lines.push(Line::from(std::mem::take(current)));
            current.push(Span::raw(" ".repeat(indent)));
            used = indent;
        }
        chunk.push(character);
        used += char_width;
    }
    if !chunk.is_empty() {
        current.push(Span::styled(chunk, style));
    }
    used
}

pub fn truncate(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    let mut result = String::new();
    let mut used = 0;
    for character in text.chars() {
        let char_width = character.width().unwrap_or(0);
        if used + char_width + 1 > width {
            break;
        }
        result.push(character);
        used += char_width;
    }
    result.push('…');
    result
}

pub fn line_width(line: &Line) -> usize {
    line.spans.iter().map(|span| span.content.width()).sum()
}

// ---- Tables ----

const COLUMN_GAP: &str = " │ ";

/// Columns as wide as their widest cell. When the table is wider than the space, the
/// widest column shrinks first and its cells end in "…".
fn table(header: &[Spans], body: &[Vec<Spans>], width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let columns = body
        .iter()
        .map(Vec::len)
        .chain([header.len()])
        .max()
        .unwrap_or(0);
    if columns == 0 {
        return Vec::new();
    }
    let widths = column_widths(header, body, columns, width);
    let gap = Span::styled(COLUMN_GAP, Style::new().fg(theme.muted));
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let mut lines = vec![table_row(header, &widths, &gap, Some(bold))];
    let rule: Vec<String> = widths.iter().map(|width| "─".repeat(*width)).collect();
    lines.push(Line::styled(rule.join("─┼─"), Style::new().fg(theme.muted)));
    lines.extend(body.iter().map(|row| table_row(row, &widths, &gap, None)));
    lines
}

fn column_widths(
    header: &[Spans],
    body: &[Vec<Spans>],
    columns: usize,
    width: usize,
) -> Vec<usize> {
    let cell_width = |cell: &Spans| cell.iter().map(|span| span.content.width()).sum::<usize>();
    let mut widths = vec![3; columns];
    for row in body.iter().chain([&header.to_vec()]) {
        for (index, cell) in row.iter().enumerate() {
            widths[index] = widths[index].max(cell_width(cell));
        }
    }
    let room = width.saturating_sub(COLUMN_GAP.width() * (columns - 1));
    while widths.iter().sum::<usize>() > room {
        let Some(widest) = widths.iter_mut().max().filter(|widest| **widest > 3) else {
            break;
        };
        *widest -= 1;
    }
    widths
}

fn table_row(
    cells: &[Spans],
    widths: &[usize],
    gap: &Span<'static>,
    style: Option<Style>,
) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, width) in widths.iter().enumerate() {
        if index > 0 {
            spans.push(gap.clone());
        }
        let text: String = cells
            .get(index)
            .map(|cell| cell.iter().map(|span| span.content.as_ref()).collect())
            .unwrap_or_default();
        let text = truncate(&text, *width);
        let padding = width.saturating_sub(text.width());
        let cell_style = style
            .or_else(|| cells.get(index)?.first().map(|span| span.style))
            .unwrap_or_default();
        spans.push(Span::styled(
            format!("{text}{}", " ".repeat(padding)),
            cell_style,
        ));
    }
    Line::from(spans)
}

// ---- Parsing ----

#[derive(Default)]
struct Container {
    blocks: Vec<Block>,
    summary: Spans,
    open: bool,
}

#[derive(Default)]
struct TableState {
    header: Vec<Spans>,
    rows: Vec<Vec<Spans>>,
    row: Vec<Spans>,
}

struct Builder<'t> {
    theme: &'t Theme,
    /// The document, then one entry per open `<details>`.
    containers: Vec<Container>,
    line: Spans,
    styles: Vec<Style>,
    /// Inline HTML tags that pushed a style, so a closing tag pops only its own.
    html_tags: Vec<String>,
    /// One entry per open list: the next number, or `None` for a bullet list.
    lists: Vec<Option<u64>>,
    /// The marker owed to the first line of the current list item.
    bullet: Option<Span<'static>>,
    bullet_width: usize,
    quote_depth: usize,
    code_block: bool,
    table: Option<TableState>,
    cell: Option<Spans>,
    summary: Option<Spans>,
}

impl<'t> Builder<'t> {
    fn new(theme: &'t Theme) -> Self {
        Builder {
            theme,
            containers: vec![Container::default()],
            line: Vec::new(),
            styles: Vec::new(),
            html_tags: Vec::new(),
            lists: Vec::new(),
            bullet: None,
            bullet_width: 0,
            quote_depth: 0,
            code_block: false,
            table: None,
            cell: None,
            summary: None,
        }
    }

    fn finish(mut self) -> Vec<Block> {
        self.flush();
        while self.containers.len() > 1 {
            self.close_details();
        }
        let mut blocks = self
            .containers
            .pop()
            .map(|root| root.blocks)
            .unwrap_or_default();
        trim_blanks(&mut blocks);
        blocks
    }

    fn style(&self) -> Style {
        self.styles
            .last()
            .copied()
            .unwrap_or_else(|| Style::new().fg(self.theme.foreground))
    }

    fn code_style(&self) -> Style {
        Style::new().fg(self.theme.accent).bg(self.theme.selection)
    }

    fn push_modifier(&mut self, modifier: Modifier) {
        self.styles.push(self.style().add_modifier(modifier));
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) if self.code_block => self.code_lines(&text),
            Event::Text(text) => self.push(text.to_string(), self.style()),
            Event::Code(code) => self.push(code.to_string(), self.code_style()),
            Event::Html(html) | Event::InlineHtml(html) => self.html(&html),
            Event::SoftBreak => self.push(" ".to_string(), self.style()),
            Event::HardBreak => self.flush(),
            Event::Rule => {
                self.flush();
                self.add(Block::Rule);
            }
            Event::TaskListMarker(done) => self.task_marker(done),
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Heading { .. } => {
                self.flush();
                let style = Style::new()
                    .fg(self.theme.heading)
                    .add_modifier(Modifier::BOLD);
                self.styles.push(style);
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.quote_depth += 1;
            }
            Tag::CodeBlock(_) => {
                self.flush();
                self.code_block = true;
            }
            Tag::List(start) => {
                self.flush();
                self.lists.push(start);
            }
            Tag::Item => self.start_item(),
            Tag::Emphasis => self.push_modifier(Modifier::ITALIC),
            Tag::Strong => self.push_modifier(Modifier::BOLD),
            Tag::Strikethrough => self.push_modifier(Modifier::CROSSED_OUT),
            Tag::Link { .. } => self.styles.push(
                self.style()
                    .fg(self.theme.accent)
                    .add_modifier(Modifier::UNDERLINED),
            ),
            Tag::Image { .. } => {
                self.styles.push(Style::new().fg(self.theme.muted));
                self.push("[image: ".to_string(), self.style());
            }
            Tag::Table(_) => {
                self.flush();
                self.table = Some(TableState::default());
            }
            Tag::TableCell => self.cell = Some(Vec::new()),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.end_block(),
            TagEnd::Heading(level) => {
                self.styles.pop();
                self.flush();
                if level <= HeadingLevel::H2 {
                    self.add(Block::Rule);
                }
                self.add(Block::Blank);
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quote_depth = self.quote_depth.saturating_sub(1);
                self.end_block();
            }
            TagEnd::CodeBlock => {
                self.code_block = false;
                self.end_block();
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
                self.end_block();
            }
            TagEnd::Item => {
                self.flush();
                self.bullet = None;
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link => {
                self.styles.pop();
            }
            TagEnd::Image => {
                self.push("]".to_string(), self.style());
                self.styles.pop();
            }
            TagEnd::TableCell => self.end_cell(),
            TagEnd::TableHead => self.end_table_row(true),
            TagEnd::TableRow => self.end_table_row(false),
            TagEnd::Table => self.end_table(),
            _ => {}
        }
    }

    /// Ends a paragraph-level block. Blocks inside a list stay tight, as on GitHub.
    fn end_block(&mut self) {
        self.flush();
        if self.lists.is_empty() {
            self.add(Block::Blank);
        }
    }

    fn start_item(&mut self) {
        self.flush();
        let marker = match self.lists.last_mut() {
            Some(Some(number)) => {
                *number += 1;
                format!("{}. ", *number - 1)
            }
            _ => "• ".to_string(),
        };
        self.bullet_width = marker.width();
        self.bullet = Some(Span::styled(marker, Style::new().fg(self.theme.muted)));
    }

    /// A task item shows a checkbox instead of its bullet.
    fn task_marker(&mut self, done: bool) {
        let (marker, color) = if done {
            ("☑ ", self.theme.success)
        } else {
            ("☐ ", self.theme.muted)
        };
        self.bullet_width = marker.width();
        self.bullet = Some(Span::styled(marker, Style::new().fg(color)));
    }

    fn end_cell(&mut self) {
        let cell = self.cell.take().unwrap_or_default();
        if let Some(table) = &mut self.table {
            table.row.push(cell);
        }
    }

    fn end_table_row(&mut self, header: bool) {
        let Some(table) = &mut self.table else {
            return;
        };
        let row = std::mem::take(&mut table.row);
        if header {
            table.header = row;
        } else {
            table.rows.push(row);
        }
    }

    fn end_table(&mut self) {
        if let Some(table) = self.table.take() {
            self.add(Block::Table {
                header: table.header,
                rows: table.rows,
            });
            self.end_block();
        }
    }

    fn code_lines(&mut self, text: &str) {
        for line in text.trim_end_matches('\n').split('\n') {
            self.line.push(Span::raw("  "));
            self.line
                .push(Span::styled(line.to_string(), self.code_style()));
            self.emit();
        }
    }

    /// Sends text to the summary being read, the table cell, or the current line.
    fn push(&mut self, text: String, style: Style) {
        let span = Span::styled(text, style);
        if let Some(summary) = &mut self.summary {
            summary.push(span);
        } else if let Some(cell) = &mut self.cell {
            cell.push(span);
        } else {
            self.line.push(span);
        }
    }

    fn flush(&mut self) {
        if !self.line.is_empty() {
            self.emit();
        }
    }

    /// Ends the current line, with the quote bars and list indent in front of it.
    fn emit(&mut self) {
        let mut spans: Spans = Vec::new();
        let rail = Style::new().fg(self.theme.muted);
        spans.extend((0..self.quote_depth).map(|_| Span::styled("▎ ", rail)));
        let nesting = self.lists.len().saturating_sub(1) * 2;
        if !self.lists.is_empty() {
            spans.push(Span::raw(" ".repeat(nesting)));
            let marker = self
                .bullet
                .take()
                .unwrap_or_else(|| Span::raw(" ".repeat(self.bullet_width)));
            spans.push(marker);
        }
        let indent = spans.iter().map(|span| span.content.width()).sum();
        spans.append(&mut self.line);
        self.add(Block::Line { spans, indent });
    }

    fn add(&mut self, block: Block) {
        let Some(container) = self.containers.last_mut() else {
            return;
        };
        let after_blank = matches!(container.blocks.last(), None | Some(Block::Blank));
        if block == Block::Blank && after_blank {
            return;
        }
        container.blocks.push(block);
    }

    // ---- HTML ----

    /// Reads the tags GitHub allows. Unknown tags are dropped and their text kept.
    fn html(&mut self, html: &str) {
        let mut rest = html;
        while let Some(open) = rest.find('<') {
            self.html_text(&rest[..open]);
            rest = &rest[open..];
            let end = if rest.starts_with("<!--") {
                rest.find("-->").map(|index| index + 3)
            } else {
                rest.find('>').map(|index| index + 1)
            };
            let Some(end) = end else {
                break;
            };
            self.html_tag(&rest[..end]);
            rest = &rest[end..];
        }
        self.html_text(rest);
    }

    /// Text between tags. A line break in an HTML block ends the line.
    fn html_text(&mut self, text: &str) {
        let decoded = decode_entities(text);
        let mut parts = decoded.split('\n').peekable();
        while let Some(part) = parts.next() {
            if !part.trim().is_empty() {
                self.push(part.to_string(), self.style());
            }
            if parts.peek().is_some() && self.summary.is_none() && self.cell.is_none() {
                self.flush();
            }
        }
    }

    fn html_tag(&mut self, raw: &str) {
        if raw.starts_with("<!--") {
            return;
        }
        let inner = raw
            .trim_start_matches('<')
            .trim_end_matches('>')
            .trim_end_matches('/')
            .trim();
        let (name, attributes) = inner.split_once(char::is_whitespace).unwrap_or((inner, ""));
        let name = name.to_ascii_lowercase();
        match name.as_str() {
            "details" => {
                self.flush();
                let open = attributes
                    .split_whitespace()
                    .any(|attribute| attribute == "open");
                self.containers.push(Container {
                    open,
                    ..Container::default()
                });
            }
            "/details" => self.close_details(),
            "summary" => {
                self.flush();
                self.summary = Some(Vec::new());
            }
            "/summary" => self.close_summary(),
            "br" | "p" | "/p" | "div" | "/div" | "li" | "/li" => self.flush(),
            "hr" => {
                self.flush();
                self.add(Block::Rule);
            }
            "img" => {
                // An image without alt text is an icon; it carries no words to show.
                let alt = attribute(attributes, "alt").unwrap_or_default();
                if !alt.trim().is_empty() {
                    self.push(format!("[image: {alt}]"), Style::new().fg(self.theme.muted));
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.flush();
                self.open_inline(
                    &name,
                    Style::new()
                        .fg(self.theme.heading)
                        .add_modifier(Modifier::BOLD),
                );
            }
            "/h1" | "/h2" | "/h3" | "/h4" | "/h5" | "/h6" => {
                self.close_inline(&name[1..]);
                self.end_block();
            }
            _ => self.inline_tag(&name),
        }
    }

    fn inline_tag(&mut self, name: &str) {
        let style = self.style();
        let opened = match name {
            "a" => style
                .fg(self.theme.accent)
                .add_modifier(Modifier::UNDERLINED),
            "b" | "strong" => style.add_modifier(Modifier::BOLD),
            "i" | "em" => style.add_modifier(Modifier::ITALIC),
            "code" | "kbd" => self.code_style(),
            "sub" | "sup" | "small" => style.fg(self.theme.muted),
            "s" | "del" => style.add_modifier(Modifier::CROSSED_OUT),
            _ => {
                if let Some(closed) = name.strip_prefix('/') {
                    self.close_inline(closed);
                }
                return;
            }
        };
        self.open_inline(name, opened);
    }

    fn open_inline(&mut self, name: &str, style: Style) {
        self.html_tags.push(name.to_string());
        self.styles.push(style);
    }

    fn close_inline(&mut self, name: &str) {
        if self.html_tags.last().is_some_and(|open| open == name) {
            self.html_tags.pop();
            self.styles.pop();
        }
    }

    fn close_summary(&mut self) {
        let Some(summary) = self.summary.take() else {
            return;
        };
        if let Some(container) = self.containers.last_mut() {
            container.summary = summary;
        }
    }

    fn close_details(&mut self) {
        self.flush();
        self.close_summary();
        if self.containers.len() < 2 {
            return;
        }
        let Some(mut details) = self.containers.pop() else {
            return;
        };
        trim_blanks(&mut details.blocks);
        self.add(Block::Details {
            summary: details.summary,
            body: details.blocks,
            open: details.open,
        });
        self.add(Block::Blank);
    }
}

fn trim_blanks(blocks: &mut Vec<Block>) {
    while blocks.last() == Some(&Block::Blank) {
        blocks.pop();
    }
}

fn attribute(attributes: &str, name: &str) -> Option<String> {
    let start = attributes.find(&format!("{name}="))? + name.len() + 1;
    let rest = &attributes[start..];
    let quote = rest
        .chars()
        .next()
        .filter(|character| *character == '"' || *character == '\'')?;
    let value = &rest[1..];
    Some(decode_entities(&value[..value.find(quote)?]))
}

fn decode_entities(text: &str) -> String {
    text.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&amp;", "&")
}

/// Black or white text, whichever reads better on a label's own color.
pub fn label_colors(hex: &str) -> (Color, Color) {
    let channel = |range: std::ops::Range<usize>| u8::from_str_radix(hex.get(range)?, 16).ok();
    let (Some(red), Some(green), Some(blue)) = (channel(0..2), channel(2..4), channel(4..6)) else {
        return (Color::DarkGray, Color::White);
    };
    let luminance = 0.299 * f64::from(red) + 0.587 * f64::from(green) + 0.114 * f64::from(blue);
    let text = if luminance > 150.0 {
        Color::Black
    } else {
        Color::White
    };
    (Color::Rgb(red, green, blue), text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;

    fn render(markdown: &str, width: usize, open: &[usize]) -> Vec<TextRow> {
        let theme = theme::by_name("dracula").unwrap();
        let open: HashSet<usize> = open.iter().copied().collect();
        let mut folds = Folds {
            next: 0,
            toggled: &open,
        };
        layout(&parse(markdown, &theme), width, &mut folds, &theme)
    }

    fn text(rows: &[TextRow]) -> Vec<String> {
        rows.iter()
            .map(|row| {
                row.line
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn headings_lose_their_marks_and_html_comments_vanish() {
        let rows = text(&render(
            "<!-- bot marker -->\n\n## Reason\n\nBecause **it** is `slow`.",
            40,
            &[],
        ));
        assert_eq!(rows[0], "Reason");
        assert!(rows[1].starts_with("───"), "an h2 gets a rule under it");
        assert_eq!(rows[3], "Because it is slow.");
        assert!(
            rows.iter()
                .all(|row| !row.contains("<!--") && !row.contains("##"))
        );
    }

    #[test]
    fn details_is_a_closed_fold_until_opened() {
        let markdown = "Before\n\n<details>\n<summary>🤖 Rework status</summary>\n\nRound 2 done.\n\n- one\n- two\n\n</details>\n\nAfter";
        let closed = render(markdown, 50, &[]);
        let bar = closed
            .iter()
            .find(|row| row.fold.is_some())
            .expect("a fold bar");
        let bar_text: String = bar
            .line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(bar_text.starts_with(" ▶ 🤖 Rework status"));
        assert!(bar_text.contains("4 lines, enter opens"), "{bar_text}");
        assert!(!text(&closed).iter().any(|row| row.contains("Round 2")));
        assert_eq!(text(&closed).last().unwrap(), "After");

        let opened = text(&render(markdown, 50, &[0]));
        assert!(opened.contains(&"│ Round 2 done.".to_string()));
        assert!(opened.contains(&"│ • two".to_string()));
    }

    #[test]
    fn details_open_starts_open_and_a_toggle_closes_it() {
        let markdown = "<details open>\n<summary>Shown</summary>\n\nbody\n\n</details>";
        assert!(text(&render(markdown, 40, &[])).contains(&"│ body".to_string()));
        assert!(!text(&render(markdown, 40, &[0])).contains(&"│ body".to_string()));
    }

    #[test]
    fn a_nested_fold_keeps_its_number_while_the_outer_one_is_closed() {
        let markdown = "<details><summary>outer</summary>\n\n<details><summary>inner</summary>\n\nx\n\n</details>\n\n</details>\n\n<details><summary>last</summary>\n\ny\n\n</details>";
        let closed = render(markdown, 40, &[]);
        let folds: Vec<_> = closed.iter().filter_map(|row| row.fold).collect();
        assert_eq!(folds, [0, 2], "the hidden inner fold still uses number 1");
        let open = render(markdown, 40, &[0]);
        assert_eq!(
            open.iter().filter_map(|row| row.fold).collect::<Vec<_>>(),
            [0, 1, 2]
        );
    }

    #[test]
    fn html_links_images_and_bold_render_as_text() {
        let markdown = "<a href=\"https://x\"><picture><img alt=\"Explain Change\" src=\"a.svg\"></picture></a> and <b>Merge Risk:</b> Low &amp; fine";
        let rows = text(&render(markdown, 80, &[]));
        assert_eq!(
            rows[0],
            "[image: Explain Change] and Merge Risk: Low & fine"
        );
        let icon = text(&render(
            "<b>Merge Risk:</b><img src=\"dot.svg\"> Low",
            80,
            &[],
        ));
        assert_eq!(icon[0], "Merge Risk: Low");
    }

    #[test]
    fn task_lists_show_checkboxes_and_ordered_lists_count() {
        let rows = text(&render(
            "- [x] Keep it small\n- [ ] Add docs\n\n1. one\n2. two",
            40,
            &[],
        ));
        assert_eq!(rows[..2], ["☑ Keep it small", "☐ Add docs"]);
        assert_eq!(rows[3..5], ["1. one", "2. two"]);
    }

    #[test]
    fn wrapped_list_lines_hang_under_the_text() {
        let rows = text(&render("- alpha beta gamma delta epsilon", 20, &[]));
        assert_eq!(rows, ["• alpha beta gamma", "  delta epsilon"]);
    }

    #[test]
    fn a_wide_table_shrinks_its_widest_column() {
        let markdown = "| Name | Notes |\n|---|---|\n| a | a very long note that cannot fit |";
        let rows = text(&render(markdown, 24, &[]));
        assert_eq!(rows[0], "Name │ Notes");
        assert!(rows[2].ends_with('…'));
        assert!(rows.iter().all(|row| row.width() <= 24), "{rows:?}");
    }

    #[test]
    fn a_word_that_fills_the_line_does_not_push_its_space_down() {
        // "hij" ends exactly at column 13, so its trailing space has no room.
        let lines = wrap(&[Span::raw("- "), Span::raw("abcdefg hij klm")], 13, 2);
        let text: Vec<String> = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect();
        assert_eq!(text, ["- abcdefg hij", "  klm"]);
    }

    #[test]
    fn a_long_word_splits_at_the_width() {
        let rows = wrap(&[Span::raw("x".repeat(25))], 10, 0);
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn label_text_contrasts_with_the_label() {
        assert_eq!(label_colors("ffffff").1, Color::Black);
        assert_eq!(label_colors("0e1a40").1, Color::White);
    }
}
