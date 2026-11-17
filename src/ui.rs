use chrono::{DateTime, Utc};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use unicode_width::UnicodeWidthChar;

use crate::app::{App, DetailView, Row, Screen};
use crate::model::{Activity, CheckState, CheckSummary, Group, PrDetail, PrSummary, ReviewState};
use crate::theme::Theme;

const INBOX_HELP: &str =
    "j/k move | enter open | space fold | o browser | d diff | r refresh | q quit";
const DETAIL_HELP: &str = "j/k scroll | space/b page | o browser | d diff | r reload | esc back";

/// What every render function needs besides the data it draws.
#[derive(Clone, Copy)]
struct Paint<'a> {
    theme: &'a Theme,
    now: DateTime<Utc>,
}

impl Paint<'_> {
    fn span(&self, text: impl Into<String>, color: Color) -> Span<'static> {
        Span::styled(text.into(), Style::new().fg(color))
    }

    fn bold(&self, text: impl Into<String>, color: Color) -> Span<'static> {
        Span::styled(
            text.into(),
            Style::new().fg(color).add_modifier(Modifier::BOLD),
        )
    }
}

pub fn render(frame: &mut Frame, app: &mut App, theme: &Theme, now: DateTime<Utc>) {
    let paint = Paint { theme, now };
    let base = Style::new().bg(theme.background).fg(theme.foreground);
    frame.render_widget(Block::new().style(base), frame.area());
    match &app.screen {
        Screen::Inbox => render_inbox(frame, app, paint),
        Screen::Detail(view) => render_detail(frame, view, paint),
    }
}

// ---- Inbox ----

fn render_inbox(frame: &mut Frame, app: &mut App, paint: Paint) {
    let [top, body, bottom] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    frame.render_widget(Paragraph::new(inbox_status(app, paint)), top);

    let row_width = body.width.saturating_sub(2) as usize;
    let selected = app.list_state.selected();
    let items: Vec<ListItem> = app
        .rows()
        .into_iter()
        .enumerate()
        .map(|(index, row)| row_item(app, row, row_width, selected == Some(index), paint))
        .collect();
    let list = List::new(items).highlight_style(Style::new().bg(paint.theme.selection));
    frame.render_stateful_widget(list, body, &mut app.list_state);

    frame.render_widget(
        Paragraph::new(paint.span(INBOX_HELP, paint.theme.muted)),
        bottom,
    );
}

fn inbox_status(app: &App, paint: Paint) -> Line<'static> {
    let theme = paint.theme;
    let mut spans = vec![paint.bold(" GitHub Inbox ", theme.heading)];
    if app.loading {
        spans.push(paint.span(" loading...", theme.warning));
    } else if let Some(fetched_at) = app.fetched_at {
        spans.push(paint.span(
            format!(" fetched {}", relative_time(fetched_at, paint.now)),
            theme.muted,
        ));
    }
    if let Some(error) = &app.error {
        spans.push(paint.span(format!(" {error}"), theme.danger));
    }
    Line::from(spans)
}

fn row_item(app: &App, row: Row, width: usize, selected: bool, paint: Paint) -> ListItem<'static> {
    match row {
        Row::Header(group_index) => ListItem::new(header_line(&app.groups[group_index], paint)),
        Row::Pr(group_index, pr_index) => pr_item(
            &app.groups[group_index].prs[pr_index],
            width,
            selected,
            paint,
        ),
    }
}

fn header_line(group: &Group, paint: Paint) -> Line<'static> {
    let marker = if group.collapsed { "▸" } else { "▾" };
    let shown = group.prs.len() as u64;
    // A search past the page limit shows how many of the matches are listed.
    let count = if group.total > shown {
        format!("{shown} of {}", group.total)
    } else {
        group.total.to_string()
    };
    Line::from(vec![
        paint.bold(format!("{marker} {}  ", group.title), paint.theme.heading),
        paint.span(count, paint.theme.accent),
    ])
}

/// The selected row's secondary text uses the foreground color, because the muted
/// color has too little contrast against the selection background.
fn secondary_color(selected: bool, theme: &Theme) -> Color {
    if selected {
        theme.foreground
    } else {
        theme.muted
    }
}

fn pr_item(pr: &PrSummary, width: usize, selected: bool, paint: Paint) -> ListItem<'static> {
    let theme = paint.theme;
    let secondary = secondary_color(selected, theme);
    let badges = Line::from(badge_spans(pr, secondary, paint));
    let title_width = width.saturating_sub(badges.width() + 4);
    let title = pad_to_width(&truncate_to_width(&pr.title, title_width), title_width);

    let mut first = vec![
        Span::from("    "),
        paint.bold(title, theme.foreground),
        Span::from(" "),
    ];
    first.extend(badges.spans);
    let second = paint.span(
        format!(
            "    {}#{} by {}, updated {}",
            pr.repo,
            pr.number,
            pr.author,
            relative_time(pr.updated_at, paint.now)
        ),
        secondary,
    );
    ListItem::new(Text::from(vec![Line::from(first), Line::from(second)]))
}

fn badge_spans(pr: &PrSummary, secondary: Color, paint: Paint) -> Vec<Span<'static>> {
    let review = if pr.is_draft {
        paint.span("Draft", secondary)
    } else {
        review_span(pr.review, paint)
    };
    vec![
        pad_span(review, 18),
        pad_span(checks_span(pr.checks.as_ref(), secondary, paint), 9),
        paint.span(format!("{:>5} comments", pr.comments), secondary),
    ]
}

fn review_span(review: ReviewState, paint: Paint) -> Span<'static> {
    let theme = paint.theme;
    match review {
        ReviewState::Approved => paint.span("Approved", theme.success),
        ReviewState::ChangesRequested => paint.span("Changes requested", theme.danger),
        ReviewState::Awaiting => paint.span("Awaiting approval", theme.warning),
    }
}

fn checks_span(checks: Option<&CheckSummary>, secondary: Color, paint: Paint) -> Span<'static> {
    let Some(checks) = checks else {
        return paint.span("-", secondary);
    };
    let (symbol, color) = check_symbol(checks.state, paint.theme);
    paint.span(
        format!("{symbol} {}/{}", checks.passed, checks.total),
        color,
    )
}

fn check_symbol(state: CheckState, theme: &Theme) -> (&'static str, Color) {
    match state {
        CheckState::Passing => ("✓", theme.success),
        CheckState::Pending => ("●", theme.warning),
        CheckState::Failing => ("✗", theme.danger),
    }
}

fn pad_span(span: Span<'static>, width: usize) -> Span<'static> {
    let style = span.style;
    Span::styled(pad_to_width(&span.content, width), style)
}

// ---- Detail ----

fn render_detail(frame: &mut Frame, view: &DetailView, paint: Paint) {
    let theme = paint.theme;
    let [top, body, bottom] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    frame.render_widget(Paragraph::new(paint.span(DETAIL_HELP, theme.muted)), bottom);

    let detail = match &view.detail {
        None => {
            frame.render_widget(Paragraph::new(detail_title(&view.pr, paint)), top);
            frame.render_widget(
                Paragraph::new(paint.span("loading...", theme.warning)),
                body,
            );
            return;
        }
        Some(Err(error)) => {
            frame.render_widget(Paragraph::new(detail_title(&view.pr, paint)), top);
            frame.render_widget(
                Paragraph::new(paint.span(error.clone(), theme.danger)).wrap(Wrap { trim: false }),
                body,
            );
            return;
        }
        Some(Ok(detail)) => detail,
    };

    frame.render_widget(Paragraph::new(detail_header(detail, paint)), top);
    let [description, sidebar] =
        Layout::horizontal([Constraint::Percentage(65), Constraint::Percentage(35)]).areas(body);
    render_description(frame, detail, view.scroll, description, paint);
    let border = Block::new()
        .borders(Borders::LEFT)
        .border_style(Style::new().fg(theme.muted));
    let sidebar_text = Text::from(sidebar_lines(detail, paint));
    frame.render_widget(
        Paragraph::new(sidebar_text)
            .wrap(Wrap { trim: true })
            .block(border),
        sidebar,
    );
}

fn detail_title(pr: &PrSummary, paint: Paint) -> Text<'static> {
    Text::from(vec![
        Line::from(paint.bold(pr.title.clone(), paint.theme.foreground)),
        Line::from(paint.span(format!("{}#{}", pr.repo, pr.number), paint.theme.muted)),
    ])
}

fn detail_header(detail: &PrDetail, paint: Paint) -> Text<'static> {
    let theme = paint.theme;
    let pr = &detail.summary;
    let state = if pr.is_draft {
        paint.span("Draft", theme.muted)
    } else {
        paint.span("Open", theme.success)
    };
    let opened = format!(
        " by {}, opened {}. {} ← {}. ",
        pr.author,
        relative_time(detail.created_at, paint.now),
        detail.base,
        detail.head
    );
    Text::from(vec![
        Line::from(vec![
            paint.bold(format!("{}  ", pr.title), theme.foreground),
            paint.span(format!("#{}", pr.number), theme.muted),
        ]),
        Line::from(vec![
            state,
            Span::from(opened),
            paint.span(format!("+{}", detail.additions), theme.success),
            Span::from(" "),
            paint.span(format!("-{}", detail.deletions), theme.danger),
            Span::from(format!(" in {} files", detail.changed_files)),
        ]),
        Line::from(paint.span(pr.repo.clone(), theme.muted)),
    ])
}

fn render_description(frame: &mut Frame, detail: &PrDetail, scroll: u16, area: Rect, paint: Paint) {
    let body = if detail.body.trim().is_empty() {
        "_No description provided._"
    } else {
        detail.body.as_str()
    };
    let block = Block::new()
        .borders(Borders::TOP)
        .border_style(Style::new().fg(paint.theme.muted))
        .title(paint.bold(" Description ", paint.theme.heading));
    let paragraph = Paragraph::new(tui_markdown::from_str(body))
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0))
        .block(block);
    frame.render_widget(paragraph, area);
}

fn sidebar_lines(detail: &PrDetail, paint: Paint) -> Vec<Line<'static>> {
    let theme = paint.theme;
    let summary = &detail.summary;
    let mut lines = section("Status", paint);
    let review = if summary.is_draft {
        paint.span("Draft", theme.muted)
    } else {
        review_span(summary.review, paint)
    };
    lines.push(Line::from(vec![Span::from("Review: "), review]));
    lines.push(Line::from(vec![
        Span::from("Checks: "),
        checks_span(summary.checks.as_ref(), theme.muted, paint),
    ]));
    lines.push(Line::from(format!("Comments: {}", summary.comments)));

    lines.extend(section("Reviewers", paint));
    lines.extend(detail.reviewers.iter().map(|reviewer| {
        Line::from(vec![
            Span::from(format!("{} ", reviewer.name)),
            reviewer_state_span(&reviewer.state, paint),
        ])
    }));
    lines.extend(section("Assignees", paint));
    lines.extend(detail.assignees.iter().map(|name| Line::from(name.clone())));
    lines.extend(section("Labels", paint));
    lines.extend(
        detail
            .labels
            .iter()
            .map(|label| Line::from(paint.span(label.clone(), theme.accent))),
    );
    lines.extend(check_section(detail, paint));
    lines.extend(section("Activity", paint));
    lines.extend(
        detail
            .activity
            .iter()
            .flat_map(|item| activity_lines(item, paint)),
    );
    lines
}

fn section(title: &str, paint: Paint) -> Vec<Line<'static>> {
    vec![
        Line::from(""),
        Line::from(paint.bold(format!(" {title}"), paint.theme.heading)),
    ]
}

fn reviewer_state_span(state: &str, paint: Paint) -> Span<'static> {
    let theme = paint.theme;
    let color = match state {
        "approved" => theme.success,
        "changes requested" => theme.danger,
        "requested" => theme.warning,
        _ => theme.muted,
    };
    paint.span(format!("({state})"), color)
}

/// Lists every check that is not passing, and counts the passing ones.
fn check_section(detail: &PrDetail, paint: Paint) -> Vec<Line<'static>> {
    let mut lines = section("Checks", paint);
    let passing = detail
        .checks
        .iter()
        .filter(|check| check.state == CheckState::Passing)
        .count();
    for check in detail
        .checks
        .iter()
        .filter(|check| check.state != CheckState::Passing)
    {
        let (symbol, color) = check_symbol(check.state, paint.theme);
        lines.push(Line::from(vec![
            paint.span(format!("{symbol} "), color),
            Span::from(check.name.clone()),
        ]));
    }
    if passing > 0 {
        lines.push(Line::from(
            paint.span(format!("✓ {passing} passing"), paint.theme.success),
        ));
    }
    lines
}

fn activity_lines(item: &Activity, paint: Paint) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        paint.bold(item.author.clone(), paint.theme.foreground),
        paint.span(
            format!(" {}, {}", item.action, relative_time(item.at, paint.now)),
            paint.theme.muted,
        ),
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
    format!(
        "{text}{}",
        " ".repeat(width.saturating_sub(display_width(text)))
    )
}

fn display_width(text: &str) -> usize {
    text.chars()
        .map(|character| character.width().unwrap_or(0))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;
    use chrono::TimeZone;

    #[test]
    fn relative_time_picks_the_largest_unit() {
        let now = Utc.with_ymd_and_hms(2026, 11, 17, 12, 0, 0).unwrap();
        assert_eq!(relative_time(now, now), "just now");
        assert_eq!(
            relative_time(now - chrono::Duration::minutes(5), now),
            "5m ago"
        );
        assert_eq!(
            relative_time(now - chrono::Duration::hours(3), now),
            "3h ago"
        );
        assert_eq!(
            relative_time(now - chrono::Duration::days(2), now),
            "2d ago"
        );
        assert_eq!(
            relative_time(now - chrono::Duration::days(60), now),
            "2026-09-18"
        );
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

    #[test]
    fn selected_row_secondary_text_uses_the_foreground_color() {
        let theme = theme::by_name("dracula").unwrap();
        assert_eq!(secondary_color(true, &theme), theme.foreground);
        assert_eq!(secondary_color(false, &theme), theme.muted);
    }
}
