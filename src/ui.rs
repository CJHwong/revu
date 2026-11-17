use chrono::{DateTime, Utc};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::UnicodeWidthChar;

use crate::app::{App, DetailView, Row, Screen};
use crate::model::{CheckState, CheckSummary, Group, PrDetail, PrSummary, ReviewState};

const INBOX_HELP: &str = "j/k move | enter open | space fold | o browser | d diff | r refresh | q quit";
const DETAIL_HELP: &str = "j/k scroll | space/b page | o browser | d diff | r reload | esc back";

pub fn render(frame: &mut Frame, app: &mut App, now: DateTime<Utc>) {
    match &app.screen {
        Screen::Inbox => render_inbox(frame, app, now),
        Screen::Detail(view) => render_detail(frame, view, now),
    }
}

// ---- Inbox ----

fn render_inbox(frame: &mut Frame, app: &mut App, now: DateTime<Utc>) {
    let [top, body, bottom] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
    frame.render_widget(Paragraph::new(inbox_status(app, now)), top);

    let row_width = body.width.saturating_sub(2) as usize;
    let items: Vec<ListItem> = app.rows().into_iter().map(|row| row_item(app, row, row_width, now)).collect();
    let list = List::new(items).highlight_style(Style::new().bg(Color::DarkGray));
    frame.render_stateful_widget(list, body, &mut app.list_state);

    frame.render_widget(Paragraph::new(INBOX_HELP).dark_gray(), bottom);
}

fn inbox_status(app: &App, now: DateTime<Utc>) -> Line<'static> {
    let mut spans = vec![Span::from(" GitHub Inbox ").bold()];
    if app.loading {
        spans.push(Span::from(" loading...").yellow());
    } else if let Some(fetched_at) = app.fetched_at {
        spans.push(Span::from(format!(" fetched {}", relative_time(fetched_at, now))).dark_gray());
    }
    if let Some(error) = &app.error {
        spans.push(Span::from(format!(" {error}")).red());
    }
    Line::from(spans)
}

fn row_item(app: &App, row: Row, width: usize, now: DateTime<Utc>) -> ListItem<'static> {
    match row {
        Row::Header(group_index) => ListItem::new(header_line(&app.groups[group_index])),
        Row::Pr(group_index, pr_index) => pr_item(&app.groups[group_index].prs[pr_index], width, now),
    }
}

fn header_line(group: &Group) -> Line<'static> {
    let marker = if group.collapsed { "▸" } else { "▾" };
    let shown = group.prs.len() as u64;
    // A search past the page limit shows how many of the matches are listed.
    let count = if group.total > shown { format!("{shown} of {}", group.total) } else { group.total.to_string() };
    Line::from(vec![
        Span::from(format!("{marker} {}  ", group.title)).bold(),
        Span::from(count).cyan(),
    ])
}

fn pr_item(pr: &PrSummary, width: usize, now: DateTime<Utc>) -> ListItem<'static> {
    let badges = Line::from(badge_spans(pr));
    let title_width = width.saturating_sub(badges.width() + 4);
    let title = pad_to_width(&truncate_to_width(&pr.title, title_width), title_width);

    let mut first = vec![Span::from("    "), Span::from(title).bold(), Span::from(" ")];
    first.extend(badges.spans);
    let second = Line::from(format!(
        "    {}#{} by {}, updated {}",
        pr.repo,
        pr.number,
        pr.author,
        relative_time(pr.updated_at, now)
    ))
    .dark_gray();
    ListItem::new(Text::from(vec![Line::from(first), second]))
}

fn badge_spans(pr: &PrSummary) -> Vec<Span<'static>> {
    let review = if pr.is_draft { Span::from("Draft").dark_gray() } else { review_span(pr.review) };
    vec![
        pad_span(review, 18),
        pad_span(checks_span(pr.checks.as_ref()), 9),
        Span::from(format!("{:>5} comments", pr.comments)).dark_gray(),
    ]
}

fn review_span(review: ReviewState) -> Span<'static> {
    match review {
        ReviewState::Approved => Span::from("Approved").green(),
        ReviewState::ChangesRequested => Span::from("Changes requested").red(),
        ReviewState::Awaiting => Span::from("Awaiting approval").yellow(),
    }
}

fn checks_span(checks: Option<&CheckSummary>) -> Span<'static> {
    let Some(checks) = checks else {
        return Span::from("-").dark_gray();
    };
    let (symbol, color) = check_symbol(checks.state);
    Span::styled(format!("{symbol} {}/{}", checks.passed, checks.total), Style::new().fg(color))
}

fn check_symbol(state: CheckState) -> (&'static str, Color) {
    match state {
        CheckState::Passing => ("✓", Color::Green),
        CheckState::Pending => ("●", Color::Yellow),
        CheckState::Failing => ("✗", Color::Red),
    }
}

fn pad_span(span: Span<'static>, width: usize) -> Span<'static> {
    let style = span.style;
    Span::styled(pad_to_width(&span.content, width), style)
}

// ---- Detail ----

fn render_detail(frame: &mut Frame, view: &DetailView, now: DateTime<Utc>) {
    let [top, body, bottom] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
    frame.render_widget(Paragraph::new(DETAIL_HELP).dark_gray(), bottom);

    let detail = match &view.detail {
        None => {
            frame.render_widget(Paragraph::new(detail_title(&view.pr)), top);
            frame.render_widget(Paragraph::new("loading...").yellow(), body);
            return;
        }
        Some(Err(error)) => {
            frame.render_widget(Paragraph::new(detail_title(&view.pr)), top);
            frame.render_widget(Paragraph::new(error.clone()).red().wrap(Wrap { trim: false }), body);
            return;
        }
        Some(Ok(detail)) => detail,
    };

    frame.render_widget(Paragraph::new(detail_header(detail, now)), top);
    let [description, sidebar] =
        Layout::horizontal([Constraint::Percentage(65), Constraint::Percentage(35)]).areas(body);
    render_description(frame, detail, view.scroll, description);
    let sidebar_text = Text::from(sidebar_lines(detail, now));
    frame.render_widget(
        Paragraph::new(sidebar_text).wrap(Wrap { trim: true }).block(Block::new().borders(Borders::LEFT)),
        sidebar,
    );
}

fn detail_title(pr: &PrSummary) -> Text<'static> {
    Text::from(vec![
        Line::from(pr.title.clone()).bold(),
        Line::from(format!("{}#{}", pr.repo, pr.number)).dark_gray(),
    ])
}

fn detail_header(detail: &PrDetail, now: DateTime<Utc>) -> Text<'static> {
    let pr = &detail.summary;
    let state = if pr.is_draft { Span::from("Draft").dark_gray() } else { Span::from("Open").green() };
    let opened = format!(
        " by {}, opened {}. {} ← {}. ",
        pr.author,
        relative_time(detail.created_at, now),
        detail.base,
        detail.head
    );
    Text::from(vec![
        Line::from(vec![Span::from(format!("{}  ", pr.title)).bold(), Span::from(format!("#{}", pr.number)).dark_gray()]),
        Line::from(vec![
            state,
            Span::from(opened),
            Span::from(format!("+{}", detail.additions)).green(),
            Span::from(" "),
            Span::from(format!("-{}", detail.deletions)).red(),
            Span::from(format!(" in {} files", detail.changed_files)),
        ]),
        Line::from(pr.repo.clone()).dark_gray(),
    ])
}

fn render_description(frame: &mut Frame, detail: &PrDetail, scroll: u16, area: Rect) {
    let body = if detail.body.trim().is_empty() { "_No description provided._" } else { detail.body.as_str() };
    let paragraph = Paragraph::new(tui_markdown::from_str(body))
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0))
        .block(Block::new().borders(Borders::TOP).title(" Description "));
    frame.render_widget(paragraph, area);
}

fn sidebar_lines(detail: &PrDetail, now: DateTime<Utc>) -> Vec<Line<'static>> {
    let mut lines = section("Status");
    let review = if detail.summary.is_draft { Span::from("Draft").dark_gray() } else { review_span(detail.summary.review) };
    lines.push(Line::from(vec![Span::from("Review: "), review]));
    lines.push(Line::from(vec![Span::from("Checks: "), checks_span(detail.summary.checks.as_ref())]));
    lines.push(Line::from(format!("Comments: {}", detail.summary.comments)));

    lines.extend(section("Reviewers"));
    lines.extend(detail.reviewers.iter().map(|reviewer| {
        Line::from(vec![Span::from(format!("{} ", reviewer.name)), reviewer_state_span(&reviewer.state)])
    }));
    lines.extend(section("Assignees"));
    lines.extend(detail.assignees.iter().map(|name| Line::from(name.clone())));
    lines.extend(section("Labels"));
    lines.extend(detail.labels.iter().map(|label| Line::from(label.clone()).cyan()));
    lines.extend(check_section(detail));
    lines.extend(section("Activity"));
    lines.extend(detail.activity.iter().flat_map(|item| activity_lines(item, now)));
    lines
}

fn section(title: &str) -> Vec<Line<'static>> {
    vec![Line::from(""), Line::from(format!(" {title}")).add_modifier(Modifier::BOLD)]
}

fn reviewer_state_span(state: &str) -> Span<'static> {
    let span = Span::from(format!("({state})"));
    match state {
        "approved" => span.green(),
        "changes requested" => span.red(),
        "requested" => span.yellow(),
        _ => span.dark_gray(),
    }
}

/// Lists every check that is not passing, and counts the passing ones.
fn check_section(detail: &PrDetail) -> Vec<Line<'static>> {
    let mut lines = section("Checks");
    let passing = detail.checks.iter().filter(|check| check.state == CheckState::Passing).count();
    for check in detail.checks.iter().filter(|check| check.state != CheckState::Passing) {
        let (symbol, color) = check_symbol(check.state);
        lines.push(Line::from(vec![
            Span::styled(format!("{symbol} "), Style::new().fg(color)),
            Span::from(check.name.clone()),
        ]));
    }
    if passing > 0 {
        lines.push(Line::from(format!("✓ {passing} passing")).green());
    }
    lines
}

fn activity_lines(item: &crate::model::Activity, now: DateTime<Utc>) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::from(item.author.clone()).bold(),
        Span::from(format!(" {}, {}", item.action, relative_time(item.at, now))).dark_gray(),
    ])];
    if !item.excerpt.is_empty() {
        lines.push(Line::from(format!("  {}", item.excerpt)));
    }
    lines
}

// ---- Text helpers ----

pub fn relative_time(then: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let seconds = (now - then).num_seconds().max(0);
    match seconds {
        0..60 => "just now".to_string(),
        60..3_600 => format!("{}m ago", seconds / 60),
        3_600..86_400 => format!("{}h ago", seconds / 3_600),
        86_400..2_592_000 => format!("{}d ago", seconds / 86_400),
        _ => then.format("%Y-%m-%d").to_string(),
    }
}

/// Cuts by display width, so CJK titles stay aligned with the badge columns.
pub fn truncate_to_width(text: &str, width: usize) -> String {
    if display_width(text) <= width {
        return text.to_string();
    }
    let mut used = 0;
    let mut result = String::new();
    for character in text.chars() {
        let char_width = character.width().unwrap_or(0);
        if used + char_width > width.saturating_sub(1) {
            result.push('…');
            return result;
        }
        used += char_width;
        result.push(character);
    }
    result
}

fn pad_to_width(text: &str, width: usize) -> String {
    format!("{text}{}", " ".repeat(width.saturating_sub(display_width(text))))
}

fn display_width(text: &str) -> usize {
    text.chars().map(|character| character.width().unwrap_or(0)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn relative_time_picks_the_largest_unit() {
        let now = Utc.with_ymd_and_hms(2026, 11, 17, 12, 0, 0).unwrap();
        assert_eq!(relative_time(now, now), "just now");
        assert_eq!(relative_time(now - chrono::Duration::minutes(5), now), "5m ago");
        assert_eq!(relative_time(now - chrono::Duration::hours(3), now), "3h ago");
        assert_eq!(relative_time(now - chrono::Duration::days(2), now), "2d ago");
        assert_eq!(relative_time(now - chrono::Duration::days(60), now), "2026-09-18");
    }

    #[test]
    fn truncate_keeps_short_text_and_marks_cut_text() {
        assert_eq!(truncate_to_width("short", 10), "short");
        assert_eq!(truncate_to_width("exact", 5), "exact");
        assert_eq!(truncate_to_width("abcdefghij", 5), "abcd…");
    }

    #[test]
    fn truncate_counts_wide_characters_twice() {
        // Each CJK character takes two columns: two fit in 5 columns with the ellipsis.
        assert_eq!(truncate_to_width("會計模式發票", 5), "會計…");
        assert_eq!(pad_to_width("會計", 6), "會計  ");
    }
}
