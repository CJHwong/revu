//! The Overview tab as GitHub lays it out: a status card, the description, then the
//! conversation, oldest first. It builds plain rows; the UI only scrolls them.

use chrono::{DateTime, Utc};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::collections::HashSet;
use unicode_width::UnicodeWidthStr;

use crate::markdown::{self, Block, Folds, TextRow, line_width, truncate};
use crate::model::{
    CheckState, Comment, PrActivity, PrDetail, ReviewState, ThreadLine, TimelineItem,
};
use crate::theme::Theme;
use crate::ui::relative_time;

pub struct Context<'a> {
    pub detail: &'a PrDetail,
    pub activity: Option<&'a Result<PrActivity, String>>,
    /// Folds the user flipped from their default state.
    pub toggled: &'a HashSet<usize>,
    pub theme: &'a Theme,
    pub now: DateTime<Utc>,
}

impl Context<'_> {
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

pub fn build(context: &Context, width: usize) -> Vec<TextRow> {
    let mut folds = Folds {
        next: 0,
        toggled: context.toggled,
    };
    let mut rows = status_card(context, width);
    rows.push(TextRow::blank());
    rows.extend(description_card(context, width, &mut folds));
    rows.push(TextRow::blank());
    rows.push(TextRow::plain(Line::from(
        context.bold(" Comments", context.theme.heading),
    )));
    rows.push(TextRow::blank());
    rows.extend(timeline_rows(context, width, &mut folds));
    rows
}

/// A rounded box with the title in its top border. `content` is already laid out to
/// `width - 4`. The top border is an anchor, so `]` and `[` jump between cards.
fn card(
    title: Vec<Span<'static>>,
    content: Vec<TextRow>,
    width: usize,
    theme: &Theme,
) -> Vec<TextRow> {
    let border = Style::new().fg(theme.muted);
    let inner = width.saturating_sub(4);
    let title_width: usize = title.iter().map(|span| span.content.width()).sum();
    let mut top = vec![Span::styled("╭─ ", border)];
    top.extend(title);
    let fill = width.saturating_sub(title_width + 5);
    top.push(Span::styled(format!(" {}╮", "─".repeat(fill)), border));
    let mut rows = vec![TextRow::anchor(Line::from(top))];
    for row in content {
        let padding = inner.saturating_sub(line_width(&row.line));
        let mut spans = vec![Span::styled("│ ", border)];
        spans.extend(row.line.spans);
        spans.push(Span::raw(" ".repeat(padding)));
        spans.push(Span::styled(" │", border));
        rows.push(TextRow {
            line: Line::from(spans),
            ..row
        });
    }
    rows.push(TextRow::plain(Line::styled(
        format!("╰{}╯", "─".repeat(width.saturating_sub(2))),
        border,
    )));
    rows
}

fn status_card(context: &Context, width: usize) -> Vec<TextRow> {
    let theme = context.theme;
    let summary = &context.detail.summary;
    let (title, color) = match summary.review {
        _ if summary.is_draft => ("◌ Draft", theme.muted),
        ReviewState::Approved => ("✓ Approved", theme.success),
        ReviewState::ChangesRequested => ("✗ Changes requested", theme.danger),
        ReviewState::Awaiting => ("◷ Approval required", theme.warning),
    };
    let approvals = context
        .detail
        .reviewers
        .iter()
        .filter(|reviewer| reviewer.state == "approved")
        .count();
    let mut line = vec![context.span("Checks ", theme.muted)];
    line.push(match &summary.checks {
        None => context.span("none", theme.muted),
        Some(checks) => {
            let (symbol, color) = check_symbol(checks.state, theme);
            context.span(
                format!("{symbol} {}/{}", checks.passed, checks.total),
                color,
            )
        }
    });
    let counts = [
        ("Comments", summary.comments),
        ("Commits", context.detail.commit_count),
        ("Approvals", approvals as u64),
    ];
    for (name, count) in counts {
        line.push(context.span(format!("    {name} "), theme.muted));
        line.push(context.span(count.to_string(), theme.foreground));
    }
    card(
        vec![context.bold(title, color)],
        vec![TextRow::plain(Line::from(line))],
        width,
        theme,
    )
}

fn check_symbol(state: CheckState, theme: &Theme) -> (&'static str, Color) {
    match state {
        CheckState::Passing => ("✓", theme.success),
        CheckState::Pending => ("●", theme.warning),
        CheckState::Failing => ("✗", theme.danger),
    }
}

fn description_card(context: &Context, width: usize, folds: &mut Folds) -> Vec<TextRow> {
    let detail = context.detail;
    let title = vec![
        context.bold(detail.summary.author.clone(), context.theme.foreground),
        context.span(
            format!(" opened {}", relative_time(detail.created_at, context.now)),
            context.theme.muted,
        ),
    ];
    let body = body_rows(
        context,
        &detail.body,
        width.saturating_sub(4),
        folds,
        "No description provided.",
    );
    card(title, body, width, context.theme)
}

fn body_rows(
    context: &Context,
    body: &str,
    width: usize,
    folds: &mut Folds,
    empty: &str,
) -> Vec<TextRow> {
    let rows = markdown::layout(
        &markdown::parse(body, context.theme),
        width,
        folds,
        context.theme,
    );
    if rows.is_empty() {
        let style = Style::new()
            .fg(context.theme.muted)
            .add_modifier(Modifier::ITALIC);
        return vec![TextRow::plain(Line::styled(empty.to_string(), style))];
    }
    rows
}

fn timeline_rows(context: &Context, width: usize, folds: &mut Folds) -> Vec<TextRow> {
    let theme = context.theme;
    let notice = |text: String, color: Color| {
        vec![TextRow::plain(Line::from(
            context.span(format!(" {text}"), color),
        ))]
    };
    let activity = match context.activity {
        None => return notice("loading comments...".to_string(), theme.warning),
        Some(Err(error)) => return notice(error.clone(), theme.danger),
        Some(Ok(activity)) => activity,
    };
    if activity.timeline.is_empty() {
        return notice("No comments yet.".to_string(), theme.muted);
    }
    let mut rows = Vec::new();
    if activity.timeline_cut {
        let text = "Older comments are not shown here. o opens the pull request on GitHub.";
        rows.extend(notice(text.to_string(), theme.muted));
        rows.push(TextRow::blank());
    }
    for item in &activity.timeline {
        rows.extend(item_rows(context, item, width, folds));
    }
    rows
}

fn item_rows(
    context: &Context,
    item: &TimelineItem,
    width: usize,
    folds: &mut Folds,
) -> Vec<TextRow> {
    let comment = match item {
        TimelineItem::Commit { oid, headline, at } => {
            return vec![commit_row(context, oid, headline, *at, width)];
        }
        TimelineItem::Comment(comment) => comment,
    };
    // An approval with no text is one line on GitHub, not a card.
    if comment.body.trim().is_empty() && comment.threads.is_empty() {
        return vec![
            TextRow::anchor(Line::from(comment_title(context, comment))),
            TextRow::blank(),
        ];
    }
    let inner = width.saturating_sub(4);
    let mut content = Vec::new();
    // A review made only of inline comments has no text of its own to show.
    if !comment.body.trim().is_empty() || comment.threads.is_empty() {
        content = body_rows(context, &comment.body, inner, folds, "No text.");
        content.push(TextRow::blank());
    }
    for thread in &comment.threads {
        let block = thread_block(context, thread);
        content.extend(markdown::layout(&[block], inner, folds, context.theme));
    }
    trim_blank_rows(&mut content);
    let mut rows = card(
        comment_title(context, comment),
        content,
        width,
        context.theme,
    );
    rows.push(TextRow::blank());
    rows
}

fn comment_title(context: &Context, comment: &Comment) -> Vec<Span<'static>> {
    let theme = context.theme;
    let mut spans = vec![context.bold(comment.author.login.clone(), theme.foreground)];
    if comment.author.bot {
        let chip = Style::new().fg(theme.background).bg(theme.muted);
        spans.push(Span::raw(" "));
        spans.push(Span::styled(" bot ", chip));
    }
    let verb_color = match comment.verb.as_str() {
        "approved" => theme.success,
        "requested changes" => theme.danger,
        _ => theme.muted,
    };
    spans.push(context.span(format!(" {}", comment.verb), verb_color));
    spans.push(context.span(
        format!(" {}", relative_time(comment.at, context.now)),
        theme.muted,
    ));
    spans
}

/// A review thread as a fold, like GitHub: the bar names the file, the lines and whether
/// it is resolved, and the body holds the thread's comments. An unresolved thread starts
/// open; a resolved one starts closed.
fn thread_block(context: &Context, thread: &ThreadLine) -> Block {
    let theme = context.theme;
    let status = if thread.resolved {
        "✓ Resolved"
    } else {
        "● Unresolved"
    };
    // The status comes first, so a narrow bar cuts the path, not the status.
    let summary = vec![Span::raw(format!(
        "{status}   {}  {}",
        thread.path, thread.lines
    ))];
    let mut body = Vec::new();
    for comment in &thread.comments {
        let title = vec![
            context.bold(comment.author.login.clone(), theme.foreground),
            context.span(
                format!(" {}", relative_time(comment.at, context.now)),
                theme.muted,
            ),
        ];
        body.push(Block::Line {
            spans: title,
            indent: 0,
        });
        body.extend(markdown::parse(&comment.body, theme));
        body.push(Block::Blank);
    }
    body.pop();
    Block::Details {
        summary,
        body,
        open: !thread.resolved,
    }
}

fn trim_blank_rows(rows: &mut Vec<TextRow>) {
    while rows
        .last()
        .is_some_and(|row| markdown::line_width(&row.line) == 0)
    {
        rows.pop();
    }
}

fn commit_row(
    context: &Context,
    oid: &str,
    headline: &str,
    at: DateTime<Utc>,
    width: usize,
) -> TextRow {
    let theme = context.theme;
    let short: String = oid.chars().take(7).collect();
    let tail = format!("  {short}  {}", relative_time(at, context.now));
    let headline = truncate(headline, width.saturating_sub(tail.width() + 4));
    TextRow::plain(Line::from(vec![
        context.span("  ○ ", theme.muted),
        context.span(headline, theme.foreground),
        context.span(format!("  {short}"), theme.accent),
        context.span(format!("  {}", relative_time(at, context.now)), theme.muted),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::{sample_activity, sample_detail};
    use crate::theme;

    fn rendered(open: &[usize]) -> Vec<String> {
        let theme = theme::by_name("dracula").unwrap();
        let detail = sample_detail();
        let activity = Ok(sample_activity());
        let open: HashSet<usize> = open.iter().copied().collect();
        let now = detail.created_at + chrono::Duration::days(3);
        let context = Context {
            detail: &detail,
            activity: Some(&activity),
            toggled: &open,
            theme: &theme,
            now,
        };
        build(&context, 60)
            .iter()
            .map(|row| {
                row.line
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn cards_are_closed_boxes_of_the_full_width() {
        let lines = rendered(&[]);
        assert!(lines[0].starts_with("╭─ ◷ Approval required"));
        for line in lines
            .iter()
            .filter(|line| line.starts_with('╭') || line.starts_with('│') || line.starts_with('╰'))
        {
            assert_eq!(line.width(), 60, "{line:?}");
        }
    }

    #[test]
    fn the_timeline_shows_commits_bot_comments_and_review_threads() {
        let lines = rendered(&[]);
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("  ○ first  aaa111"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("╭─ coderabbitai  bot  commented"))
        );
        assert!(
            lines.iter().any(|line| line.contains("│ Looks fine")),
            "the HTML comment is dropped"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("╭─ lead requested changes"))
        );
        let thread = lines
            .iter()
            .find(|line| line.contains("✓ Resolved   a.md"))
            .unwrap();
        assert!(thread.contains("lines 8"), "{thread}");
        assert!(
            !lines.iter().any(|line| line.contains("Rename this.")),
            "a resolved thread starts closed"
        );
        assert!(
            lines.iter().any(|line| line.contains("│ Why here?")),
            "an unresolved thread starts open"
        );
        assert!(lines.iter().any(|line| line.contains("│ Because.")));
        assert!(
            !lines.iter().any(|line| line.contains("No text.")),
            "a review of only threads shows no placeholder"
        );
    }
}
