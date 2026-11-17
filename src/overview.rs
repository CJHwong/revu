//! The Overview tab as GitHub lays it out: a status card, the description, then the
//! conversation, oldest first. It builds plain rows; the UI only scrolls them.

use chrono::{DateTime, Utc};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::collections::HashSet;
use unicode_width::UnicodeWidthStr;

use crate::markdown::{self, Block, FoldId, Folds, TextRow, fold_key, line_width, truncate};
use crate::model::{
    CheckState, Comment, PrActivity, PrDetail, ReviewState, ThreadLine, TimelineItem,
};
use crate::theme::Theme;
use crate::ui::relative_time;

/// GitHub's controls over the Comments list: which items, and in which order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CommentView {
    pub filter: CommentFilter,
    pub newest_first: bool,
    /// Hide comments and reviews written by bots.
    pub no_bots: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CommentFilter {
    #[default]
    All,
    /// Reviews with an open thread, showing only their open threads.
    Unresolved,
    /// What you wrote, and the threads you took part in.
    Mine,
}

impl CommentFilter {
    pub const ALL: [CommentFilter; 3] = [Self::All, Self::Unresolved, Self::Mine];

    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Unresolved,
            Self::Unresolved => Self::Mine,
            Self::Mine => Self::All,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Unresolved => "Unresolved",
            Self::Mine => "Mine",
        }
    }

    /// The item as this filter shows it, or None when the filter hides it. Commits show
    /// only under All, as on GitHub.
    fn apply(self, item: &TimelineItem) -> Option<TimelineItem> {
        let TimelineItem::Comment(comment) = item else {
            return (self == Self::All).then(|| item.clone());
        };
        match self {
            Self::All => Some(item.clone()),
            // Your own comments and reviews only, not your replies in others' threads.
            Self::Mine => comment.mine.then(|| item.clone()),
            Self::Unresolved => {
                let threads: Vec<ThreadLine> = comment
                    .threads
                    .iter()
                    .filter(|thread| !thread.resolved)
                    .cloned()
                    .collect();
                (!threads.is_empty()).then(|| {
                    TimelineItem::Comment(Comment {
                        threads,
                        ..comment.clone()
                    })
                })
            }
        }
    }

    fn empty_text(self) -> &'static str {
        match self {
            Self::All => "No comments yet.",
            Self::Unresolved => "No unresolved threads.",
            Self::Mine => "Nothing from you here yet.",
        }
    }
}

pub struct Context<'a> {
    pub detail: &'a PrDetail,
    pub activity: Option<&'a Result<PrActivity, String>>,
    /// Folds the user flipped from their default state.
    pub toggled: &'a HashSet<FoldId>,
    pub comments: CommentView,
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
    let mut folds = Folds::new(context.toggled);
    folds.enter("description");
    let mut rows = status_card(context, width);
    rows.push(TextRow::blank());
    rows.extend(description_card(context, width, &mut folds));
    rows.push(TextRow::blank());
    rows.push(TextRow::plain(comments_header(context)));
    rows.push(TextRow::blank());
    rows.extend(timeline_rows(context, width, &mut folds));
    rows
}

/// A rounded box with the title in its top border. `content` is already laid out to
/// `width - 4`. The top border is an anchor, so `]` and `[` jump between cards.
/// A rounded box with the title in its top border. With `folds`, the card can collapse:
/// Enter on the top border folds it to that one line, which then counts the hidden lines.
fn card(
    title: Vec<Span<'static>>,
    content: Vec<TextRow>,
    width: usize,
    theme: &Theme,
    fold: Option<FoldId>,
    toggled: &HashSet<FoldId>,
) -> Vec<TextRow> {
    let border = Style::new().fg(theme.muted);
    let fold = fold.map(|id| (id, !toggled.contains(&id)));
    let open = fold.is_none_or(|(_, open)| open);
    let (start, end) = if open {
        ("╭─ ", "╮")
    } else {
        ("╶─ ", "╴")
    };
    let chevron = match fold {
        None => String::new(),
        Some(_) if open => "▼ ".to_string(),
        Some(_) => "▶ ".to_string(),
    };
    let hint = if open {
        String::new()
    } else {
        format!(" {} lines, enter opens ─", content.len() + 1)
    };
    let used = 3 + chevron.width() + line_width(&Line::from(title.clone())) + 1 + hint.width() + 1;
    let mut top = vec![
        Span::styled(start, border),
        Span::styled(
            chevron,
            Style::new().fg(theme.fold).add_modifier(Modifier::BOLD),
        ),
    ];
    top.extend(title);
    top.push(Span::styled(
        format!(" {}", "─".repeat(width.saturating_sub(used))),
        border,
    ));
    top.push(Span::styled(hint, Style::new().fg(theme.fold)));
    top.push(Span::styled(end, border));
    let top = TextRow {
        line: Line::from(top),
        fold: fold.map(|(id, _)| id),
        anchor: true,
    };
    if !open {
        return vec![top];
    }
    let inner = width.saturating_sub(4);
    let mut rows = vec![top];
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
        None,
        context.toggled,
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
    card(
        title,
        body,
        width,
        context.theme,
        Some(fold_key("description:card")),
        context.toggled,
    )
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
    let filter = context.comments.filter;
    let mut items: Vec<TimelineItem> = activity
        .timeline
        .iter()
        .filter(|item| !(context.comments.no_bots && is_bot(item)))
        .filter_map(|item| filter.apply(item))
        .collect();
    if items.is_empty() {
        return notice(filter.empty_text().to_string(), theme.muted);
    }
    if context.comments.newest_first {
        items.reverse();
    }
    let mut rows = Vec::new();
    if activity.timeline_cut {
        let text = "Older comments are not shown here. o opens the pull request on GitHub.";
        rows.extend(notice(text.to_string(), theme.muted));
        rows.push(TextRow::blank());
    }
    for item in &items {
        rows.extend(item_rows(context, item, width, folds));
    }
    rows
}

/// " Comments   All  Unresolved  Mine    Oldest first      f filter  F order", with the
/// current filter as a chip.
fn is_bot(item: &TimelineItem) -> bool {
    matches!(item, TimelineItem::Comment(comment) if comment.author.bot)
}

fn comments_header(context: &Context) -> Line<'static> {
    let theme = context.theme;
    let mut spans = vec![context.bold(" Comments   ", theme.heading)];
    for filter in CommentFilter::ALL {
        let style = if filter == context.comments.filter {
            Style::new()
                .bg(theme.selection)
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme.muted)
        };
        spans.push(Span::styled(format!(" {} ", filter.label()), style));
    }
    let order = if context.comments.newest_first {
        "Newest first"
    } else {
        "Oldest first"
    };
    spans.push(context.span(format!("    ↕ {order}"), theme.accent));
    let bots = if context.comments.no_bots {
        "    ☑ No bots"
    } else {
        "    ☐ No bots"
    };
    spans.push(context.span(bots, theme.accent));
    spans.push(context.span("      f filter  F order  n bots", theme.muted));
    Line::from(spans)
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
    let key = format!(
        "comment:{}:{}",
        comment.author.login,
        comment.at.timestamp()
    );
    // A review made only of inline comments has no text of its own to show.
    if !comment.body.trim().is_empty() || comment.threads.is_empty() {
        folds.enter(&key);
        content = body_rows(context, &comment.body, inner, folds, "No text.");
        content.push(TextRow::blank());
    }
    for thread in &comment.threads {
        // Its own scope, so a filter that hides other threads leaves its ID alone.
        let first = thread
            .comments
            .first()
            .map_or(0, |reply| reply.at.timestamp());
        folds.enter(&format!("thread:{}:{}:{first}", thread.path, thread.lines));
        let block = thread_block(context, thread);
        content.extend(markdown::layout(&[block], inner, folds, context.theme));
    }
    trim_blank_rows(&mut content);
    let mut rows = card(
        comment_title(context, comment),
        content,
        width,
        context.theme,
        Some(fold_key(&format!("{key}:card"))),
        context.toggled,
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

    fn rendered(open: &[FoldId]) -> Vec<String> {
        rendered_with(open, CommentView::default())
    }

    fn rendered_with(open: &[FoldId], comments: CommentView) -> Vec<String> {
        let theme = theme::by_name("dracula").unwrap();
        let detail = sample_detail();
        let activity = Ok(sample_activity());
        let open: HashSet<FoldId> = open.iter().copied().collect();
        let now = detail.created_at + chrono::Duration::days(3);
        let context = Context {
            detail: &detail,
            activity: Some(&activity),
            toggled: &open,
            comments,
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
    fn the_comment_filters_and_order_match_github() {
        let shows = |lines: &[String], text: &str| lines.iter().any(|line| line.contains(text));
        let view = |filter, newest_first| CommentView {
            filter,
            newest_first,
            no_bots: false,
        };

        let unresolved = rendered_with(&[], view(CommentFilter::Unresolved, false));
        assert!(shows(&unresolved, "Why here?"));
        assert!(
            !shows(&unresolved, "lines 827-830"),
            "the resolved thread is hidden"
        );
        assert!(
            !shows(&unresolved, "Looks fine"),
            "a comment with no open thread is hidden"
        );
        assert!(!shows(&unresolved, "aaa111"), "commits show only under All");

        let mine = rendered_with(&[], view(CommentFilter::Mine, false));
        assert_eq!(
            mine.last().map(|line| line.trim()),
            Some("Nothing from you here yet."),
            "a reply in someone else's thread is not Mine"
        );
        let activity = sample_activity();
        let own = activity.timeline.iter().find_map(|item| match item {
            TimelineItem::Comment(comment) if comment.author.login == "lead" => {
                Some(TimelineItem::Comment(Comment {
                    mine: true,
                    ..comment.clone()
                }))
            }
            _ => None,
        });
        let own = own.expect("the lead review");
        assert_eq!(
            CommentFilter::Mine.apply(&own),
            Some(own.clone()),
            "my own review is Mine, threads and all"
        );

        let humans = rendered_with(
            &[],
            CommentView {
                no_bots: true,
                ..CommentView::default()
            },
        );
        assert!(!shows(&humans, "Looks fine"), "the bot's comment is hidden");
        assert!(shows(&humans, "lead requested"));

        let newest = rendered_with(&[], view(CommentFilter::All, true));
        let position = |lines: &[String], text: &str| {
            lines.iter().position(|line| line.contains(text)).unwrap()
        };
        assert!(position(&newest, "lead requested") < position(&newest, "Looks fine"));
        assert!(shows(&newest, "Newest first"));
    }

    #[test]
    fn a_closed_card_and_thread_stay_closed_under_another_filter() {
        let theme = theme::by_name("dracula").unwrap();
        let detail = sample_detail();
        let activity = Ok(sample_activity());
        let none = HashSet::new();
        let context = Context {
            detail: &detail,
            activity: Some(&activity),
            toggled: &none,
            comments: CommentView::default(),
            theme: &theme,
            now: detail.created_at,
        };
        let rows = build(&context, 60);
        let fold_of = |text: &str| {
            rows.iter()
                .find(|row| {
                    row.line
                        .spans
                        .iter()
                        .any(|span| span.content.contains(text))
                })
                .and_then(|row| row.fold)
                .unwrap()
        };
        let thread = fold_of("● Unresolved");
        let shows = |lines: &[String], text: &str| lines.iter().any(|line| line.contains(text));
        let unresolved = CommentView {
            filter: CommentFilter::Unresolved,
            ..CommentView::default()
        };
        assert!(shows(&rendered_with(&[], unresolved), "Why here?"));
        assert!(
            !shows(&rendered_with(&[thread], unresolved), "Why here?"),
            "the thread closed under All is still closed under Unresolved"
        );
    }

    #[test]
    fn enter_on_a_card_border_folds_the_card_to_one_line() {
        let open = rendered(&[]);
        let border = open
            .iter()
            .position(|line| line.contains("▼ ") && line.contains("opened"))
            .unwrap();
        let theme = theme::by_name("dracula").unwrap();
        let detail = sample_detail();
        let activity = Ok(sample_activity());
        let toggled = HashSet::new();
        let context = Context {
            detail: &detail,
            activity: Some(&activity),
            toggled: &toggled,
            comments: CommentView::default(),
            theme: &theme,
            now: detail.created_at,
        };
        let id = build(&context, 60)[border]
            .fold
            .expect("the description card folds");
        let closed = rendered(&[id]);
        assert!(closed[border].starts_with("╶─ ▶ "), "{}", closed[border]);
        assert!(closed[border].contains("lines, enter opens"));
        assert!(closed.len() < open.len());
        assert_eq!(closed[border].chars().count(), 60);
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
                .any(|line| line.starts_with("╭─ ▼ coderabbitai  bot  commented"))
        );
        assert!(
            lines.iter().any(|line| line.contains("│ Looks fine")),
            "the HTML comment is dropped"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("╭─ ▼ lead requested changes"))
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
