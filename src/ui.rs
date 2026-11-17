use chrono::{DateTime, Utc};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, ListState, Padding, Paragraph, Wrap,
};
use unicode_width::UnicodeWidthChar;

use crate::app::{
    App, Click, ClickTarget, ComposeKind, Composer, DetailView, DiffOutput, Overlay,
    OverviewLayout, Prompt, PromptKind, RepoState, Row, SETTINGS, Screen, Tab,
};
use crate::editor::Editor;
use crate::file_tree::FileRow;
use crate::line_diff::RowKind;
use crate::model::{
    ChangedFile, CheckLine, CheckState, CheckSummary, CommitLine, Group, Label, Lifecycle,
    PrActivity, PrDetail, PrRef, PrSummary, RateLimit, ReviewState, Reviewer, Side, SinceCounts,
    Verdict, ViewedState,
};
use crate::repo::{GrepHit, MAX_GREP_HITS};
use crate::theme::{ON_STATE, PrState, Theme};
use crate::{markdown, overview};

const INBOX_HELP: &str = "j/k move | l/enter open | space fold | tab next group | o browser | r refresh | ctrl-l link | ? keys";

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
    app.inbox_clicks.clear();
    if let Screen::Detail(view) = &mut app.screen {
        view.clicks.clear();
    }
    frame.render_widget(Block::new().style(base), frame.area());
    if let Screen::Detail(view) = &mut app.screen {
        render_detail(
            frame,
            view,
            app.error.as_deref(),
            &app.renderer_names,
            paint,
        );
    } else {
        render_inbox(frame, app, paint);
    }
    if let Some(limit) = app.rate_limit {
        render_rate_limit(frame, limit, paint);
    }
    if let Some(selected) = app.settings_menu {
        render_settings(frame, app, selected, paint);
    }
    if let Some(selected) = app.theme_menu {
        render_theme_menu(frame, selected, app.theme_name, paint);
    }
    if app.show_help {
        render_help(frame, paint);
    }
    if let Some(prompt) = &app.prompt {
        render_prompt(frame, prompt, paint);
    }
    if let Some(composer) = &app.composer {
        render_composer(frame, composer, paint);
    }
}

/// The review or line-comment box: what it sends, the text, and the keys.
fn render_composer(frame: &mut Frame, composer: &Composer, paint: Paint) {
    let theme = paint.theme;
    let area = frame.area();
    let width = area.width.saturating_sub(4).min(100);
    let height = (area.height * 3 / 5).max(10).min(area.height);
    let box_area = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    let (title, header) = composer_header(composer, paint);
    let footer = match (&composer.error, composer.sending, composer.confirming) {
        (Some(error), ..) => paint.span(format!(" {error}"), theme.danger),
        (None, true, _) => paint.span(" sending to GitHub...", theme.warning),
        (None, false, true) => paint.bold(format!(" {}", composer.confirm_text()), theme.warning),
        _ if composer.cancel_asked => paint.span(
            " esc again throws the text away; any other key keeps it",
            theme.warning,
        ),
        _ => paint.span(
            " ctrl+s send, esc cancel, shift+arrows select, ctrl+a all, ctrl+c copy, ctrl+x cut",
            theme.muted,
        ),
    };
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(theme.heading))
        .title(paint.bold(title, theme.heading))
        .style(Style::new().bg(theme.background).fg(theme.foreground));
    let inner = block.inner(box_area);
    frame.render_widget(Clear, box_area);
    frame.render_widget(block, box_area);
    let [header_area, text_area, footer_area] = Layout::vertical([
        Constraint::Length(header.len() as u16 + 1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    frame.render_widget(Paragraph::new(header), header_area);
    let (lines, cursor_row) = editor_lines(&composer.editor, text_area.width, paint);
    let scroll = cursor_row.saturating_sub(text_area.height.saturating_sub(1));
    let text = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(text, text_area);
    frame.render_widget(Paragraph::new(Line::from(footer)), footer_area);
}

/// The box's title and the lines above the text: the verdict choice for a review.
fn composer_header(composer: &Composer, paint: Paint) -> (String, Vec<Line<'static>>) {
    let theme = paint.theme;
    match &composer.kind {
        ComposeKind::Review {
            verdict,
            own,
            drafts,
        } => {
            let mut choice = vec![paint.span(" ", theme.muted)];
            let verdicts = match own {
                true => &[Verdict::Comment][..],
                false => &[Verdict::Comment, Verdict::Approve, Verdict::RequestChanges][..],
            };
            for option in verdicts {
                let label = format!(" {} ", option.label());
                choice.push(match option == verdict {
                    true => Span::styled(
                        label,
                        Style::new()
                            .fg(theme.background)
                            .bg(theme.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    false => paint.span(label, theme.muted),
                });
                choice.push(Span::raw(" "));
            }
            let note = match own {
                true => "  GitHub takes only Comment on your own PR",
                false => "  tab switches",
            };
            choice.push(paint.span(note, theme.muted));
            let drafts = match drafts {
                0 => " no line comments; c in Files changed adds them".to_string(),
                1 => " 1 line comment from your pending review goes with it".to_string(),
                count => format!(" {count} line comments from your pending review go with it"),
            };
            let header = vec![
                Line::from(choice),
                Line::from(paint.span(drafts, theme.muted)),
            ];
            (" Review ".to_string(), header)
        }
        ComposeKind::LineComment(target) => {
            let lines = match target.start_line {
                Some(start) => format!("lines {start}-{}", target.line),
                None => format!("line {}", target.line),
            };
            let side = match target.side {
                Side::Left => ", the old side",
                Side::Right => "",
            };
            let title = format!(" Comment on {} {lines}{side} ", target.path);
            let note = " it joins your pending review; R submits the review";
            (title, vec![Line::from(paint.span(note, theme.muted))])
        }
    }
}

/// The editor's text with the selection and the cursor marked, and the row the cursor
/// is on once wrapped to `width`, so the box can scroll to it.
fn editor_lines(editor: &Editor, width: u16, paint: Paint) -> (Vec<Line<'static>>, u16) {
    let theme = paint.theme;
    let selected = Style::new().bg(theme.selection).fg(theme.foreground);
    let cursor = Style::new().fg(theme.background).bg(theme.accent);
    let plain = Style::new().fg(theme.foreground);
    let selection = editor.selection();
    let width = usize::from(width.max(1));
    let (mut lines, mut cursor_row, mut rows_before) = (Vec::new(), 0, 0);
    let mut offset = 0;
    for text in editor.text.split('\n') {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut columns = 0;
        for (index, character) in text.char_indices() {
            let at = offset + index;
            let style = match selection {
                _ if at == editor.cursor => cursor,
                Some((start, end)) if (start..end).contains(&at) => selected,
                _ => plain,
            };
            if at == editor.cursor {
                cursor_row = rows_before + columns / width;
            }
            columns += character.width().unwrap_or(0);
            push_char(&mut spans, character, style);
        }
        let end = offset + text.len();
        if editor.cursor == end {
            cursor_row = rows_before + columns / width;
            spans.push(Span::styled(" ", cursor));
            columns += 1;
        }
        rows_before += columns.div_ceil(width).max(1);
        lines.push(Line::from(spans));
        offset = end + 1;
    }
    (lines, cursor_row as u16)
}

/// Adds a character to the last span when it has the same style, so a line stays a
/// few spans long.
fn push_char(spans: &mut Vec<Span<'static>>, character: char, style: Style) {
    if let Some(last) = spans.last_mut()
        && last.style == style
    {
        last.content.to_mut().push(character);
        return;
    }
    spans.push(Span::styled(character.to_string(), style));
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
    let heights: Vec<usize> = items.iter().map(ListItem::height).collect();
    let list = List::new(items).highlight_style(Style::new().bg(paint.theme.selection));
    frame.render_stateful_widget(list, body, &mut app.list_state);
    app.inbox_clicks = list_targets(body, &heights, app.list_state.offset());

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
        Row::Header(group_index) => {
            let loading = app.group_loading.get(group_index).copied().unwrap_or(false);
            ListItem::new(header_line(&app.groups[group_index], loading, paint))
        }
        Row::Pr(group_index, pr_index) => pr_item(
            &app.groups[group_index].prs[pr_index],
            width,
            selected,
            paint,
        ),
    }
}

fn header_line(group: &Group, loading: bool, paint: Paint) -> Line<'static> {
    let marker = if group.collapsed { "▸" } else { "▾" };
    let shown = group.prs.len() as u64;
    // A search past the page limit shows how many of the matches are listed.
    let count = if group.total > shown {
        format!("{shown} of {}", group.total)
    } else {
        group.total.to_string()
    };
    let mut spans = vec![
        paint.bold(format!("{marker} {}  ", group.title), paint.theme.heading),
        paint.span(count, paint.theme.accent),
    ];
    if loading {
        spans.push(paint.span("  loading...", paint.theme.warning));
    }
    Line::from(spans)
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
        paint.span("Draft", paint.theme.github_state(PrState::Draft))
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

fn detail_help(tab: Tab) -> &'static str {
    match tab {
        Tab::Overview => {
            "j/k next comment | enter fold | f/F/n filter, order, bots | o browser | h back | ? keys"
        }
        Tab::Checks => "j/k move | enter/o browser | r reload | h back | ? keys",
        Tab::Commits => {
            "j/k commit | J/K scroll diff | m renderer | / find | o browser | h back | ? keys"
        }
        Tab::Files => {
            "j/k file | enter whole file | c comment | v viewed | ]/[ unviewed | t tree | z list | m renderer | h back | ? keys"
        }
    }
}

/// Every key, by where it works. `?` shows this list.
const KEY_LIST: &[(&str, &[(&str, &str)])] = &[
    (
        "Anywhere",
        &[
            (
                "ctrl-l, cmd-l, paste",
                "open a pull request by link or owner/repo#123",
            ),
            (
                ",",
                "settings: theme, workspace folder, how far back the inbox looks",
            ),
            ("?", "this list; any key closes it"),
        ],
    ),
    (
        "Inbox",
        &[
            ("j/k, g/G", "move, top, bottom"),
            ("enter, l", "open the pull request, or fold a group header"),
            ("space", "fold the current group"),
            ("tab, shift-tab", "next or previous group"),
            ("o, d", "open in the browser, run the open_diff command"),
            ("r, q", "reload the config and refresh, quit"),
        ],
    ),
    (
        "Pull request",
        &[
            ("1-4, tab, shift-tab", "switch tab"),
            ("esc, h, q", "back to the inbox"),
            ("g/G, space/b", "top, bottom, page down, page up"),
            ("s", "search every file in the repo at the PR head"),
            ("o, d, r", "open in the browser, run open_diff, fetch again"),
            (
                "R",
                "review: Comment, Approve or Request changes, with your line comments",
            ),
            ("X", "discard your pending review and its line comments"),
        ],
    ),
    (
        "Overview",
        &[
            (
                "j/k, wheel",
                "next or previous comment, thread or fold; a long one by a third of a screen",
            ),
            ("enter", "open or close the folded block under the cursor"),
            ("], [", "next or previous card or folded block"),
            (
                "f, F, n",
                "filter comments (all, unresolved, mine), flip the order, hide bots",
            ),
        ],
    ),
    (
        "Files changed",
        &[
            ("enter", "whole file with its changes; on a folder, fold it"),
            ("v, V", "toggle Viewed (syncs to GitHub), hide viewed files"),
            ("], [", "next or previous unviewed file"),
            ("t, z", "tree or flat list, hide or show the list"),
            (
                "c",
                "comment mode: j/k pick a line, V a range, enter writes a comment into your pending review",
            ),
        ],
    ),
    (
        "Files changed, Commits",
        &[
            ("J/K, space/b", "scroll the diff"),
            (
                "w",
                "wrap long lines, or not; unwrapped, H/L scroll sideways",
            ),
            ("m", "switch the diff renderer"),
            ("/, n/N", "find in the diff, next and previous match"),
        ],
    ),
    (
        "Commits, Checks",
        &[("enter", "open the selected item in the browser")],
    ),
    (
        "Search results, file view",
        &[("enter, esc", "open the file at the match, go back")],
    ),
];

fn render_help(frame: &mut Frame, paint: Paint) {
    let theme = paint.theme;
    let key_width = KEY_LIST
        .iter()
        .flat_map(|(_, keys)| keys.iter())
        .map(|(keys, _)| display_width(keys))
        .max()
        .unwrap_or(0);
    let mut lines = Vec::new();
    for (section, keys) in KEY_LIST {
        lines.push(Line::from(paint.bold(section.to_string(), theme.heading)));
        lines.extend(keys.iter().map(|(keys, action)| {
            Line::from(vec![
                paint.span(
                    format!("  {}", pad_to_width(keys, key_width + 2)),
                    theme.accent,
                ),
                paint.span(action.to_string(), theme.foreground),
            ])
        }));
        lines.push(Line::from(""));
    }
    lines.pop();
    let area = frame.area();
    // A short terminal gets the list without the blank lines between sections.
    if lines.len() as u16 + 2 > area.height {
        lines.retain(|line| line.width() > 0);
    }
    let width = area.width.saturating_sub(4).min(84);
    let height = (lines.len() as u16 + 2).min(area.height);
    let box_area = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(theme.heading))
        .title(paint.bold(" Keys ", theme.heading))
        .title_bottom(paint.span(" any key closes ", theme.muted))
        .style(Style::new().bg(theme.background).fg(theme.foreground));
    frame.render_widget(Clear, box_area);
    frame.render_widget(Paragraph::new(lines).block(block), box_area);
}

fn render_detail(
    frame: &mut Frame,
    view: &mut DetailView,
    error: Option<&str>,
    renderer_names: &[String],
    paint: Paint,
) {
    let theme = paint.theme;
    let header = match &view.detail {
        Some(Ok(detail)) => detail_header(detail, frame.area().width as usize, paint),
        _ => detail_title(&view.pr, paint),
    };
    let [top, tabs, body, bottom] = Layout::vertical([
        Constraint::Length(header.len() as u16),
        Constraint::Length(2),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    // An error, such as a failed Viewed sync, replaces the help line until the next action.
    let footer = match error {
        Some(message) => paint.span(message.to_string(), theme.danger),
        None => paint.span(
            overlay_help(view.overlays.last()).unwrap_or(detail_help(view.tab)),
            theme.muted,
        ),
    };
    frame.render_widget(Paragraph::new(footer), bottom);

    let Some(Ok(detail)) = &view.detail else {
        frame.render_widget(Paragraph::new(header), top);
        let message = match &view.detail {
            Some(Err(error)) => paint.span(error.clone(), theme.danger),
            _ => paint.span("loading...", theme.warning),
        };
        frame.render_widget(Paragraph::new(message).wrap(Wrap { trim: false }), body);
        return;
    };
    frame.render_widget(Paragraph::new(header), top);
    let tab_lines = vec![
        tab_bar(view.tab, detail, paint),
        tab_rule(view.tab, detail, tabs.width, paint),
    ];
    frame.render_widget(Paragraph::new(tab_lines), tabs);
    view.body_width = body.width;
    view.clicks.extend(tab_targets(detail, tabs, paint));
    if let Some(overlay) = view.overlays.last_mut() {
        let targets = render_overlay(frame, overlay, body, paint);
        view.clicks.extend(targets);
        return;
    }
    match view.tab {
        Tab::Overview => render_overview(frame, view, body, paint),
        Tab::Checks => match &view.activity {
            Some(Ok(activity)) => {
                let targets = render_checks(frame, activity, &mut view.checks, body, paint);
                view.clicks.extend(targets);
            }
            other => render_activity_state(frame, other.as_ref(), body, paint),
        },
        Tab::Commits => {
            // The full message shows above the diff, so the list needs only the subjects.
            let list_width = (body.width / 4).max(30).min(body.width / 2);
            let [list_area, diff_area] =
                Layout::horizontal([Constraint::Length(list_width), Constraint::Fill(1)])
                    .areas(body);
            let Some(Ok(activity)) = &view.activity else {
                render_activity_state(frame, view.activity.as_ref(), list_area, paint);
                return;
            };
            let targets = render_commits(
                frame,
                activity,
                detail.commit_count,
                &mut view.commits,
                list_area,
                paint,
            );
            view.clicks.extend(targets);
            let commit = view
                .commits
                .selected()
                .and_then(|index| activity.commits.get(index));
            let diff_area = match commit {
                Some(commit) => render_commit_message(frame, commit, diff_area, paint),
                None => diff_area,
            };
            render_diff_pane(frame, view, diff_area, renderer_names, paint);
        }
        Tab::Files => {
            let list_width = if view.file_list_hidden {
                0
            } else {
                file_list_width(view, body.width)
            };
            let [list_area, diff_area] =
                Layout::horizontal([Constraint::Length(list_width), Constraint::Fill(1)])
                    .areas(body);
            let listed = view.file_list_hidden || render_file_list(frame, view, list_area, paint);
            if listed && view.loaded_files().is_some() {
                render_diff_pane(frame, view, diff_area, renderer_names, paint);
            }
        }
    }
}

fn overlay_help(overlay: Option<&Overlay>) -> Option<&'static str> {
    match overlay? {
        Overlay::Search { .. } => Some("j/k move | enter open the file at the line | esc back"),
        Overlay::File { .. } => Some("j/k, space/b scroll | g/G top/bottom | esc back"),
    }
}

/// Returns what a click can hit: the search hits.
fn render_overlay(
    frame: &mut Frame,
    overlay: &mut Overlay,
    area: Rect,
    paint: Paint,
) -> Vec<ClickTarget> {
    let theme = paint.theme;
    let width = area.width as usize;
    match overlay {
        Overlay::Search { query, hits, state } => {
            let (title, items) = match hits {
                None => (format!(" searching for \"{query}\"... "), Vec::new()),
                Some(Err(error)) => (format!(" {error} "), Vec::new()),
                Some(Ok(hits)) => (
                    search_title(query, hits.len()),
                    grep_items(hits, width, paint),
                ),
            };
            let heights: Vec<usize> = items.iter().map(ListItem::height).collect();
            let block = Block::new().title(paint.bold(title, theme.heading));
            let inner = block.inner(area);
            let list = List::new(items)
                .highlight_style(Style::new().bg(theme.selection))
                .block(block);
            frame.render_stateful_widget(list, area, state);
            return list_targets(inner, &heights, state.offset());
        }
        Overlay::File {
            path,
            lines,
            scroll,
            ..
        } => {
            let block =
                Block::new().title(paint.bold(format!(" {path} at the PR head "), theme.heading));
            let inner = block.inner(area);
            frame.render_widget(block, area);
            let text = match lines {
                None => Text::from(paint.span(" loading...", theme.warning)),
                Some(Err(error)) => Text::from(paint.span(format!(" {error}"), theme.danger)),
                // Clone only the visible lines; a whole file is redrawn every frame.
                Some(Ok(text)) => Text::from(
                    text.lines
                        .iter()
                        .skip(*scroll as usize)
                        .take(inner.height as usize)
                        .cloned()
                        .collect::<Vec<_>>(),
                ),
            };
            frame.render_widget(Paragraph::new(text), inner);
        }
    }
    Vec::new()
}

fn search_title(query: &str, count: usize) -> String {
    let capped = if count == MAX_GREP_HITS {
        "the first "
    } else {
        ""
    };
    format!(" {capped}{count} matches for \"{query}\" at the PR head ")
}

fn grep_items(hits: &[GrepHit], width: usize, paint: Paint) -> Vec<ListItem<'static>> {
    hits.iter()
        .map(|hit| grep_item(hit, width, paint))
        .collect()
}

/// "src/app.rs:12  let x = 1;", the location in accent and the code as found.
fn grep_item(hit: &GrepHit, width: usize, paint: Paint) -> ListItem<'static> {
    let theme = paint.theme;
    let location = format!(" {}:{}  ", hit.path, hit.line);
    let code = truncate_to_width(
        &hit.text.replace('\t', "    "),
        width.saturating_sub(display_width(&location)),
    );
    ListItem::new(Line::from(vec![
        paint.span(location, theme.accent),
        paint.span(code, theme.foreground),
    ]))
}

/// What the Commits or Checks tab shows before its data arrives, or when it failed.
fn render_activity_state(
    frame: &mut Frame,
    activity: Option<&Result<PrActivity, String>>,
    area: Rect,
    paint: Paint,
) {
    let message = match activity {
        Some(Err(error)) => paint.span(format!(" {error}"), paint.theme.danger),
        _ => paint.span(" loading...", paint.theme.warning),
    };
    frame.render_widget(Paragraph::new(message).wrap(Wrap { trim: false }), area);
}

/// Draws the file list. Returns false when there is no list yet, so no diff pane either.
fn render_file_list(frame: &mut Frame, view: &mut DetailView, area: Rect, paint: Paint) -> bool {
    let theme = paint.theme;
    let files = match &view.files {
        None => {
            frame.render_widget(
                Paragraph::new(paint.span(files_loading_text(view), theme.warning))
                    .wrap(Wrap { trim: false }),
                area,
            );
            return false;
        }
        Some(Err(error)) => {
            let message = paint.span(format!(" {error}"), theme.danger);
            frame.render_widget(Paragraph::new(message).wrap(Wrap { trim: false }), area);
            return false;
        }
        Some(Ok(files)) => files,
    };
    let viewed = files
        .iter()
        .filter(|file| file.viewed == ViewedState::Viewed)
        .count();
    let hidden_note = if view.hide_viewed {
        ", viewed hidden"
    } else {
        ""
    };
    let title = format!(" {viewed}/{} viewed{hidden_note} ", files.len());
    let width = area.width as usize;
    let selected = view.file_state.selected();
    let items: Vec<ListItem> = view
        .file_rows()
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let item = ListItem::new(file_row_line(row, files, view.tree_view, width, paint));
            // Styled here, not by the list, so it shows while the wheel scrolls the list
            // away from the selection.
            match Some(index) == selected {
                true => item.style(Style::new().bg(theme.selection)),
                false => item,
            }
        })
        .collect();
    let heights: Vec<usize> = items.iter().map(ListItem::height).collect();
    let list_area = Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(1),
        ..area
    };
    // A list taller than its space keeps its last line for the "more below" count.
    let overflows = heights.iter().sum::<usize>() > list_area.height as usize;
    let list_area = Rect {
        height: list_area.height - u16::from(overflows),
        ..list_area
    };
    view.file_list_height = list_area.height as usize;
    let offset = match view.file_list_top {
        Some(top) => {
            let mut state = ListState::default().with_offset(top);
            frame.render_stateful_widget(List::new(items), list_area, &mut state);
            state.offset()
        }
        None => {
            frame.render_stateful_widget(List::new(items), list_area, &mut view.file_state);
            view.file_state.offset()
        }
    };
    let (above, below) = rows_beyond(&heights, offset, list_area.height as usize);
    let more = |count: usize, arrow: &str| match count {
        0 => Line::default(),
        _ => Line::from(paint.span(format!("{arrow} {count} more "), theme.muted)).right_aligned(),
    };
    let title_row = Rect { height: 1, ..area };
    frame.render_widget(Paragraph::new(paint.span(title, theme.heading)), title_row);
    frame.render_widget(more(above, "↑"), title_row);
    if overflows {
        let bottom_row = Rect {
            y: list_area.bottom(),
            height: 1,
            ..area
        };
        frame.render_widget(more(below, "↓"), bottom_row);
    }
    view.clicks
        .extend(list_targets(list_area, &heights, offset));
    true
}

/// The rows above the list's top and below its last shown row.
fn rows_beyond(heights: &[usize], offset: usize, space: usize) -> (usize, usize) {
    let mut used = 0;
    let shown = heights
        .iter()
        .skip(offset)
        .take_while(|height| {
            used += **height;
            used <= space
        })
        .count();
    (offset, heights.len().saturating_sub(offset + shown))
}

/// What the file list waits for: GitHub sends it 100 files at a time.
fn files_loading_text(view: &DetailView) -> String {
    match view.files_progress {
        None => " fetching the file list from GitHub...".to_string(),
        Some((loaded, total)) => format!(" fetching the file list: {loaded} of {total} files"),
    }
}

/// The rendered diff for the selected file or commit, or what is still missing for it.
/// Records the pane width, which the next diff request renders to.
fn render_diff_pane(
    frame: &mut Frame,
    view: &mut DetailView,
    area: Rect,
    renderer_names: &[String],
    paint: Paint,
) {
    if view.line_mode.is_some() {
        render_line_mode(frame, view, area, paint);
        return;
    }
    let theme = paint.theme;
    let renderer = renderer_names
        .get(view.renderer)
        .map_or("diff", String::as_str);
    let block = Block::new()
        .borders(Borders::LEFT)
        .border_style(Style::new().fg(theme.muted))
        .title(paint.bold(format!(" {renderer} "), theme.heading))
        .title(paint.span("m switches ", theme.muted))
        .title(paint.span(wrap_note(view), theme.muted))
        .title(find_note(view, paint))
        .title(paint.bold(diff_pane_path(view), theme.foreground));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    view.diff_width = inner.width;
    view.diff_left = area.x;
    view.diff_height = inner.height;

    let notice = |text: String, color: Color| {
        Paragraph::new(paint.span(text, color)).wrap(Wrap { trim: false })
    };
    if view.tab == Tab::Files && view.selected_file().is_none() {
        let text = " a folder: enter folds or opens it".to_string();
        frame.render_widget(notice(text, theme.muted), inner);
        return;
    }
    let content = match &view.repo {
        RepoState::Unknown => notice(" looking for a local clone...".to_string(), theme.warning),
        RepoState::Busy(message) => notice(format!(" {message}..."), theme.warning),
        RepoState::Failed(message) => notice(format!(" {message}"), theme.danger),
        RepoState::Ready(_) => match view.current_diff_key().and_then(|key| view.diffs.get(&key)) {
            None | Some(DiffOutput::Pending) => notice(" rendering...".to_string(), theme.warning),
            Some(DiffOutput::Done(Err(error))) => notice(format!(" {error}"), theme.danger),
            Some(DiffOutput::Done(Ok(text))) if text.lines.is_empty() => notice(
                " this renderer shows no changes here".to_string(),
                theme.muted,
            ),
            Some(DiffOutput::Done(Ok(text))) => {
                // Clone only the visible lines; a long diff is redrawn every frame. A
                // wrapped line takes more rows, and the paragraph clips the rest.
                let current = view.current_find_hit().map(|hit| hit.line);
                let marks = |index: usize| {
                    let strong = Some(index) == current;
                    match strong {
                        true => Style::new().fg(theme.background).bg(theme.warning),
                        false => Style::new().fg(theme.warning).bg(theme.selection),
                    }
                };
                let first = view.diff_scroll as usize;
                let pattern = view.find.as_deref().map(crate::find::Pattern::parse);
                let lines: Vec<Line> = text
                    .lines
                    .iter()
                    .enumerate()
                    .skip(first)
                    .take(inner.height as usize)
                    .map(|(index, line)| match &pattern {
                        Some(pattern) if pattern.matches_drawn(line) => {
                            mark_matches(line.clone(), &pattern.text, marks(index))
                        }
                        _ => line.clone(),
                    })
                    .collect();
                if view.diff_wrap {
                    Paragraph::new(lines).wrap(Wrap { trim: false })
                } else {
                    // Stop where the longest visible line ends at the right edge.
                    let longest = lines.iter().map(Line::width).max().unwrap_or(0);
                    let last = longest.saturating_sub(inner.width as usize) as u16;
                    view.diff_shift = view.diff_shift.min(last);
                    Paragraph::new(lines).scroll((0, view.diff_shift))
                }
            }
        },
    };
    frame.render_widget(content, inner);
}

/// Comment mode: the file's plain diff with old and new line numbers, the cursor and
/// the range V marked, and a dot on lines your pending review already comments on.
fn render_line_mode(frame: &mut Frame, view: &mut DetailView, area: Rect, paint: Paint) {
    let theme = paint.theme;
    let drafts: Vec<(Side, u64, u64)> = match (&view.activity, &view.line_mode) {
        (Some(Ok(activity)), Some(mode)) => activity
            .drafts
            .iter()
            .filter(|draft| draft.path == mode.path)
            .map(|draft| {
                (
                    draft.side,
                    draft.start_line.unwrap_or(draft.line),
                    draft.line,
                )
            })
            .collect(),
        _ => Vec::new(),
    };
    let Some(mode) = view.line_mode.as_mut() else {
        return;
    };
    let block = Block::new()
        .borders(Borders::LEFT)
        .border_style(Style::new().fg(theme.accent))
        .title(paint.bold(" comment mode ", theme.accent))
        .title(paint.span(
            "j/k move, V range, enter comment, R review, esc back ",
            theme.muted,
        ))
        .title(
            paint
                .bold(format!(" {} ", mode.path), theme.foreground)
                .into_right_aligned_line(),
        );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = match &mode.rows {
        None => {
            frame.render_widget(
                Paragraph::new(paint.span(" reading the diff...", theme.warning)),
                inner,
            );
            return;
        }
        Some(Err(error)) => {
            let text = Paragraph::new(paint.span(format!(" {error}"), theme.danger))
                .wrap(Wrap { trim: false });
            frame.render_widget(text, inner);
            return;
        }
        Some(Ok(rows)) => rows,
    };
    let height = usize::from(inner.height.max(1));
    mode.scroll = mode
        .scroll
        .min(mode.cursor)
        .max((mode.cursor + 1).saturating_sub(height));
    let (first, last) = mode.marked();
    let number = |line: Option<u64>| line.map_or("     ".to_string(), |line| format!("{line:>5}"));
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(mode.scroll)
        .take(height)
        .map(|(index, row)| {
            let commented = row.target().is_some_and(|(side, line)| {
                drafts.iter().any(|&(draft_side, start, end)| {
                    draft_side == side && (start..=end).contains(&line)
                })
            });
            let mark = match (index == mode.cursor, commented) {
                (true, _) => paint.bold("›", theme.accent),
                (false, true) => paint.span("●", theme.accent),
                (false, false) => Span::raw(" "),
            };
            let (sign, color) = match row.kind {
                RowKind::Added => ("+", theme.success),
                RowKind::Removed => ("-", theme.danger),
                RowKind::Context => (" ", theme.foreground),
                RowKind::Hunk | RowKind::Note => ("", theme.heading),
            };
            let line = Line::from(vec![
                mark,
                paint.span(
                    format!("{} {} ", number(row.old), number(row.new)),
                    theme.muted,
                ),
                paint.span(format!("{sign}{}", row.text), color),
            ]);
            match (first..=last).contains(&index) {
                true => line.style(Style::new().bg(theme.selection)),
                false => line,
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// " /query  2 of 5 lines ", at the right of the diff pane's title, so the find text and
/// the place among its matches stay in view. In another file it says n finds there.
fn find_note(view: &DetailView, paint: Paint) -> Line<'static> {
    let Some(query) = &view.find else {
        return Line::default();
    };
    let here = match view.current_find_hit() {
        Some(hit) => format!("match {} of {}", hit.ordinal, hit.total),
        None => "no match".to_string(),
    };
    let place = match (view.find_file_place(), &view.find_files, view.tab) {
        (_, _, tab) if tab != Tab::Files => format!("{here} in this commit"),
        (Some((Some(at), files)), _, _) => format!("{here} in file {at} of {files}"),
        (Some((None, files)), _, _) => {
            format!("not in this file; {files} files have it, n goes to the next")
        }
        (None, None, _) => format!("{here} in this file, looking in the others..."),
        (None, _, _) => format!("{here} in this file, other files not searched"),
    };
    Line::from(vec![
        paint.bold(format!(" /{query} "), paint.theme.warning),
        paint.span(format!(" {place}  n/N next/previous "), paint.theme.muted),
    ])
    .right_aligned()
}

/// Marks every place the find text appears in a line, ignoring case. The line of the
/// current match gets the stronger mark.
pub fn mark_matches(line: Line<'static>, query: &str, mark: Style) -> Line<'static> {
    let cells: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|span| {
            let style = line.style.patch(span.style);
            span.content
                .chars()
                .map(move |character| (character, style))
        })
        .collect();
    let lower: Vec<char> = cells
        .iter()
        .map(|(character, _)| character.to_lowercase().next().unwrap_or(*character))
        .collect();
    let query: Vec<char> = query.chars().collect();
    let mut marked = vec![false; cells.len()];
    let mut start = 0;
    while !query.is_empty() && start + query.len() <= lower.len() {
        if lower[start..start + query.len()] == query[..] {
            marked[start..start + query.len()].fill(true);
            start += query.len();
        } else {
            start += 1;
        }
    }
    if !marked.contains(&true) {
        return line;
    }
    let mut spans: Vec<Span<'static>> = Vec::new();
    for ((character, style), is_match) in cells.into_iter().zip(marked) {
        let style = if is_match { style.patch(mark) } else { style };
        match spans.last_mut() {
            Some(span) if span.style == style => span.content.to_mut().push(character),
            _ => spans.push(Span::styled(character.to_string(), style)),
        }
    }
    Line::from(spans)
}

/// "w wraps", or how far an unwrapped diff is scrolled sideways.
fn wrap_note(view: &DetailView) -> String {
    match (view.diff_wrap, view.diff_shift) {
        (true, _) => "w unwraps ".to_string(),
        (false, 0) => "w wraps  H/L sideways ".to_string(),
        (false, shift) => format!("w wraps  H/L sideways  → {shift} "),
    }
}

/// "✓ src/app/ main.rs  +3 -1", with the directory muted so the file name stands out.
/// With the file list hidden, the diff pane names the file it shows.
fn diff_pane_path(view: &DetailView) -> String {
    let path = view
        .loaded_files()
        .zip(view.selected_file())
        .and_then(|(files, index)| files.get(index));
    match path {
        Some(file) if view.tab == Tab::Files && view.file_list_hidden => format!(" {} ", file.path),
        _ => String::new(),
    }
}

/// The list sizes itself to its longest row, up to 40% of the screen, so short paths leave
/// the diff more room.
fn file_list_width(view: &DetailView, total: u16) -> u16 {
    let Some(files) = view.loaded_files() else {
        return total / 3;
    };
    let longest = view
        .file_rows()
        .iter()
        .map(|row| match row {
            FileRow::Dir { label, depth, .. } => depth * 2 + display_width(label) + 4,
            FileRow::File { index, depth } => {
                let file = &files[*index];
                let name = if view.tree_view {
                    file_name(&file.path)
                } else {
                    &file.path
                };
                let counts = format!(" +{} -{}", file.additions, file.deletions);
                depth * 2 + 3 + display_width(name) + display_width(&counts) + 2
            }
        })
        .max()
        .unwrap_or(0)
        .max(24);
    (longest as u16).min(total * 2 / 5)
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn file_row_line(
    row: &FileRow,
    files: &[ChangedFile],
    tree: bool,
    width: usize,
    paint: Paint,
) -> Line<'static> {
    let theme = paint.theme;
    match row {
        FileRow::Dir {
            label,
            depth,
            folded,
            ..
        } => {
            let chevron = if *folded { "▸" } else { "▾" };
            Line::from(vec![
                Span::raw(format!(" {}", "  ".repeat(*depth))),
                paint.span(format!("{chevron} "), theme.fold),
                paint.span(format!("{label}/"), theme.accent),
            ])
        }
        FileRow::File { index, depth } => {
            let indent = "  ".repeat(*depth);
            let mut line = file_line(
                &files[*index],
                tree,
                width.saturating_sub(indent.len()),
                paint,
            );
            line.spans.insert(0, Span::raw(indent));
            line
        }
    }
}

fn file_line(file: &ChangedFile, tree: bool, width: usize, paint: Paint) -> Line<'static> {
    let theme = paint.theme;
    let (marker, marker_color) = match file.viewed {
        ViewedState::Viewed => ("✓", theme.success),
        ViewedState::Dismissed => ("!", theme.warning),
        ViewedState::Unviewed => (" ", theme.muted),
    };
    // In the tree the folder rows carry the directory.
    let (directory, name) = match file.path.rsplit_once('/') {
        Some((_, name)) if tree => (String::new(), name.to_string()),
        Some((directory, name)) => (format!("{directory}/"), name.to_string()),
        None => (String::new(), file.path.clone()),
    };
    let counts = format!(" +{} -{}", file.additions, file.deletions);
    let path_width = width.saturating_sub(display_width(&counts) + 4);
    let name_width = display_width(&name).min(path_width);
    let directory = truncate_to_width(&directory, path_width - name_width);
    let padding = path_width.saturating_sub(display_width(&directory) + name_width);
    Line::from(vec![
        paint.span(format!(" {marker} "), marker_color),
        paint.span(directory, theme.muted),
        paint.bold(truncate_to_width(&name, name_width), theme.foreground),
        Span::from(" ".repeat(padding)),
        paint.span(format!(" +{}", file.additions), theme.success),
        paint.span(format!(" -{}", file.deletions), theme.danger),
    ])
}

fn detail_title(pr: &PrRef, paint: Paint) -> Vec<Line<'static>> {
    vec![
        Line::from(paint.bold(format!("{}#{}", pr.repo, pr.number), paint.theme.foreground)),
        Line::from(paint.span(pr.url(), paint.theme.muted)),
    ]
}

/// GitHub's order: the repo and number, the title, then a state pill with the branches
/// and the size of the change, and a blank line before the tabs. The title and the
/// branch line wrap; the API budget sits at the right of the first line.
fn detail_header(detail: &PrDetail, width: usize, paint: Paint) -> Vec<Line<'static>> {
    let theme = paint.theme;
    let pr = &detail.summary;
    let mut lines = vec![Line::from(vec![
        paint.span(format!(" {} ", pr.repo), theme.muted),
        paint.span(format!("#{}", pr.number), theme.muted),
    ])];
    let title = [paint.bold(format!(" {}", pr.title), theme.foreground)];
    lines.extend(markdown::wrap(&title, width, 1));
    let (state, state_color, verb) = match &detail.lifecycle {
        Lifecycle::Merged { .. } => (" Merged ", PrState::Merged, " merged into "),
        Lifecycle::Closed { .. } => (" Closed ", PrState::Closed, " wanted to merge into "),
        Lifecycle::Draft => (" Draft ", PrState::Draft, " wants to merge into "),
        Lifecycle::Open => (" Open ", PrState::Open, " wants to merge into "),
    };
    let state_color = theme.github_state(state_color);
    let pill = Style::new()
        .fg(ON_STATE)
        .bg(state_color)
        .add_modifier(Modifier::BOLD);
    let branch = Style::new().fg(theme.accent).bg(theme.selection);
    let merge = [
        Span::raw(" "),
        Span::styled(state, pill),
        paint.bold(format!(" {}", pr.author), theme.foreground),
        paint.span(verb, theme.muted),
        Span::styled(format!(" {} ", detail.base), branch),
        paint.span(" from ", theme.muted),
        Span::styled(format!(" {} ", detail.head), branch),
        paint.span(format!("   +{}", detail.additions), theme.success),
        paint.span(format!(" -{}", detail.deletions), theme.danger),
        paint.span(format!(", {} files", detail.changed_files), theme.muted),
    ];
    lines.extend(markdown::wrap(&merge, width, state.len() + 1));
    lines.push(Line::default());
    lines
}

/// GitHub's tab bar: the title, then a count badge, or the check state for Checks. The
/// digit keys still switch tabs; `?` lists them.
fn tab_bar(current: Tab, detail: &PrDetail, paint: Paint) -> Line<'static> {
    let theme = paint.theme;
    let mut spans = Vec::new();
    for tab in Tab::ALL {
        let selected = tab == current;
        let title_style = if selected {
            Style::new().fg(theme.heading).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme.foreground)
        };
        spans.push(Span::styled(
            format!("{TAB_LEFT}{}", tab.title()),
            title_style,
        ));
        spans.extend(tab_badge(tab, detail, paint));
        spans.push(Span::raw(TAB_RIGHT));
    }
    Line::from(spans)
}

fn tab_badge(tab: Tab, detail: &PrDetail, paint: Paint) -> Vec<Span<'static>> {
    let theme = paint.theme;
    let badge = |count: u64| vec![paint.span(format!(" {count}"), theme.muted)];
    match tab {
        Tab::Overview => Vec::new(),
        Tab::Commits => badge(detail.commit_count),
        Tab::Files => badge(detail.changed_files),
        Tab::Checks => match &detail.summary.checks {
            None => Vec::new(),
            Some(checks) => {
                let (symbol, color) = check_symbol(checks.state, theme);
                vec![paint.span(
                    format!(" {symbol} {}/{}", checks.passed, checks.total),
                    color,
                )]
            }
        },
    }
}

/// One target per visible list item, from the list's scroll offset and item heights.
fn list_targets(area: Rect, heights: &[usize], offset: usize) -> Vec<ClickTarget> {
    let mut targets = Vec::new();
    let mut top = area.y;
    for (index, height) in heights.iter().enumerate().skip(offset) {
        if top >= area.bottom() {
            break;
        }
        let height = (*height as u16).min(area.bottom() - top);
        let item = Rect::new(area.x, top, area.width, height);
        targets.push(ClickTarget {
            area: item,
            click: Click::Row(index),
        });
        top += height;
    }
    targets
}

/// One target per tab label, at the same widths the bar draws.
fn tab_targets(detail: &PrDetail, area: Rect, paint: Paint) -> Vec<ClickTarget> {
    let mut left = area.x;
    let mut targets = Vec::new();
    for tab in Tab::ALL {
        let width = tab_bar_width(tab, detail, paint) as u16;
        let label = Rect::new(
            left,
            area.y,
            width.min(area.right().saturating_sub(left)),
            1,
        );
        targets.push(ClickTarget {
            area: label,
            click: Click::Tab(tab),
        });
        left += width;
    }
    targets
}

/// A thin rule with a thick bar under the current tab, like GitHub's underline.
fn tab_rule(current: Tab, detail: &PrDetail, width: u16, paint: Paint) -> Line<'static> {
    let theme = paint.theme;
    let mut start = 0;
    let mut bar = 0;
    for tab in Tab::ALL {
        let label_width: usize = tab_bar_width(tab, detail, paint);
        if tab == current {
            // Under the title and badge, not the padding around them.
            start += TAB_LEFT.len();
            bar = label_width.saturating_sub(TAB_LEFT.len() + TAB_RIGHT.len());
            break;
        }
        start += label_width;
    }
    let rest = (width as usize).saturating_sub(start + bar);
    Line::from(vec![
        paint.span("─".repeat(start), theme.selection),
        paint.span("━".repeat(bar), theme.heading),
        paint.span("─".repeat(rest), theme.selection),
    ])
}

/// The width of one tab's label in the bar, so the underline sits under it.
fn tab_bar_width(tab: Tab, detail: &PrDetail, paint: Paint) -> usize {
    let badge: usize = tab_badge(tab, detail, paint)
        .iter()
        .map(|span| display_width(&span.content))
        .sum();
    TAB_LEFT.len() + display_width(tab.title()) + badge + TAB_RIGHT.len()
}

/// The space before and after a tab label; two tabs sit three columns apart.
const TAB_LEFT: &str = "  ";
const TAB_RIGHT: &str = " ";

/// The sidebar shows when the terminal is wide enough to keep the main column readable.
const SIDEBAR_WIDTH: u16 = 32;
const SIDEBAR_MIN_TOTAL: u16 = 110;

fn render_overview(frame: &mut Frame, view: &mut DetailView, area: Rect, paint: Paint) {
    let sidebar_width = if area.width >= SIDEBAR_MIN_TOTAL {
        SIDEBAR_WIDTH
    } else {
        0
    };
    let [main, sidebar] =
        Layout::horizontal([Constraint::Min(20), Constraint::Length(sidebar_width)]).areas(area);
    let main = main.inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    let DetailView {
        detail: Some(Ok(detail)),
        activity,
        toggled_folds,
        comments,
        overview: layout,
        cursor,
        scroll,
        page,
        clicks,
        ..
    } = view
    else {
        return;
    };
    let width = main.width as usize;
    if layout.as_ref().map(|layout| layout.width) != Some(width) {
        let context = overview::Context {
            detail,
            activity: activity.as_ref(),
            toggled: toggled_folds,
            comments: *comments,
            theme: paint.theme,
            now: paint.now,
        };
        *layout = Some(OverviewLayout {
            width,
            rows: overview::build(&context, width),
        });
    }
    let Some(layout) = layout.as_ref() else {
        return;
    };
    let height = main.height as usize;
    *page = height;
    *cursor = (*cursor).min(layout.rows.len().saturating_sub(1));
    *scroll = keep_cursor_in_view(*scroll, *cursor, height, layout.rows.len());
    let lines: Vec<Line> = layout
        .rows
        .iter()
        .enumerate()
        .skip(*scroll)
        .take(height)
        .map(|(index, row)| {
            if index == *cursor {
                highlight(row, width, paint.theme)
            } else {
                row.line.clone()
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), main);
    clicks.push(ClickTarget {
        area: main,
        click: Click::Page { first: *scroll },
    });
    if sidebar_width > 0 {
        let activity = activity
            .as_ref()
            .and_then(|activity| activity.as_ref().ok());
        render_sidebar(frame, detail, activity, sidebar, paint);
    }
}

/// The keys move the page; this only repairs it after the layout changes size, such as
/// when a fold closes or the terminal shrinks.
fn keep_cursor_in_view(scroll: usize, cursor: usize, height: usize, total: usize) -> usize {
    let scroll = scroll.min(total.saturating_sub(height));
    scroll.clamp((cursor + 1).saturating_sub(height), cursor)
}

/// The cursor row across the full width. On a fold bar it turns the accent color, so the
/// bar under the cursor differs from the others.
fn highlight(row: &markdown::TextRow, width: usize, theme: &Theme) -> Line<'static> {
    let background = if row.fold.is_some() {
        theme.accent
    } else {
        theme.selection
    };
    let mut line = row.line.clone();
    let padding = width.saturating_sub(markdown::line_width(&line));
    line.spans.push(Span::raw(" ".repeat(padding)));
    for span in &mut line.spans {
        span.style = span.style.bg(background);
    }
    line
}

fn render_sidebar(
    frame: &mut Frame,
    detail: &PrDetail,
    activity: Option<&PrActivity>,
    area: Rect,
    paint: Paint,
) {
    let border = Block::new()
        .borders(Borders::LEFT)
        .border_style(Style::new().fg(paint.theme.selection));
    let inner = border.inner(area);
    frame.render_widget(border, area);
    // The text sits inside a one-cell margin on each side.
    let lines = sidebar_lines(
        detail,
        activity,
        inner.width.saturating_sub(2) as usize,
        paint,
    );
    frame.render_widget(
        Paragraph::new(lines),
        inner.inner(Margin {
            horizontal: 1,
            vertical: 0,
        }),
    );
}

fn render_commits(
    frame: &mut Frame,
    activity: &PrActivity,
    commit_count: u64,
    state: &mut ListState,
    area: Rect,
    paint: Paint,
) -> Vec<ClickTarget> {
    let theme = paint.theme;
    let selected = state.selected();
    let items: Vec<ListItem> = activity
        .commits
        .iter()
        .enumerate()
        .map(|(index, commit)| {
            let secondary = secondary_color(selected == Some(index), theme);
            let symbol = match commit.checks {
                Some(checks) => check_symbol(checks, theme),
                None => (" ", theme.muted),
            };
            let short_oid: String = commit.oid.chars().take(7).collect();
            let first = Line::from(vec![
                paint.span(format!(" {} ", symbol.0), symbol.1),
                paint.span(format!("{short_oid}  "), theme.accent),
                paint.bold(commit.headline.clone(), theme.foreground),
            ]);
            let second = paint.span(
                format!(
                    "            {}, {}",
                    commit.author,
                    relative_time(commit.authored_at, paint.now)
                ),
                secondary,
            );
            ListItem::new(Text::from(vec![first, Line::from(second)]))
        })
        .collect();
    let heights: Vec<usize> = items.iter().map(ListItem::height).collect();
    let mut list = List::new(items).highlight_style(Style::new().bg(theme.selection));
    let mut inner = area;
    if commit_count > activity.commits.len() as u64 {
        let title = format!(
            " first {} of {} commits ",
            activity.commits.len(),
            commit_count
        );
        let block = Block::new().title(paint.span(title, theme.warning));
        inner = block.inner(area);
        list = list.block(block);
    }
    frame.render_stateful_widget(list, area, state);
    list_targets(inner, &heights, state.offset())
}

/// The selected commit's whole message above its diff. It takes at most a third of the
/// pane; a longer message ends in a note. Returns the area left for the diff.
fn render_commit_message(frame: &mut Frame, commit: &CommitLine, area: Rect, paint: Paint) -> Rect {
    let theme = paint.theme;
    let width = area.width.saturating_sub(2) as usize;
    let mut lines = markdown::wrap(
        &[paint.bold(commit.headline.clone(), theme.foreground)],
        width,
        0,
    );
    if !commit.body.is_empty() {
        lines.push(Line::from(""));
    }
    for paragraph in commit.body.lines() {
        if paragraph.trim().is_empty() {
            lines.push(Line::from(""));
            continue;
        }
        lines.extend(markdown::wrap(
            &[paint.span(paragraph.to_string(), theme.foreground)],
            width,
            0,
        ));
    }
    let room = (area.height / 3).max(3) as usize;
    if lines.len() > room {
        lines.truncate(room - 1);
        lines.push(Line::from(
            paint.span("… o opens the full message on GitHub", theme.muted),
        ));
    }
    let [message_area, rest] = Layout::vertical([
        Constraint::Length(lines.len() as u16 + 1),
        Constraint::Fill(1),
    ])
    .areas(area);
    let block = Block::new()
        .borders(Borders::LEFT | Borders::BOTTOM)
        .border_style(Style::new().fg(theme.muted))
        .padding(Padding::left(1));
    frame.render_widget(Paragraph::new(lines).block(block), message_area);
    rest
}

fn render_checks(
    frame: &mut Frame,
    activity: &PrActivity,
    state: &mut ListState,
    area: Rect,
    paint: Paint,
) -> Vec<ClickTarget> {
    let theme = paint.theme;
    if activity.checks.is_empty() {
        frame.render_widget(
            Paragraph::new(paint.span(" No checks ran on the head commit.", theme.muted)),
            area,
        );
        return Vec::new();
    }
    let selected = state.selected();
    let items: Vec<ListItem> = activity
        .checks
        .iter()
        .enumerate()
        .map(|(index, check)| {
            let secondary = secondary_color(selected == Some(index), theme);
            let (symbol, color) = check_symbol(check.state, theme);
            let first = Line::from(vec![
                paint.span(format!(" {symbol} "), color),
                paint.bold(check.name.clone(), theme.foreground),
            ]);
            ListItem::new(Text::from(vec![
                first,
                Line::from(paint.span(check_details(check), secondary)),
            ]))
        })
        .collect();
    let heights: Vec<usize> = items.iter().map(ListItem::height).collect();
    let list = List::new(items).highlight_style(Style::new().bg(theme.selection));
    frame.render_stateful_widget(list, area, state);
    list_targets(area, &heights, state.offset())
}

/// "CI, took 2m 30s" for a finished check run; status contexts have no workflow.
fn check_details(check: &CheckLine) -> String {
    let duration = match (check.started_at, check.completed_at) {
        (Some(start), Some(end)) => Some(format!("took {}", format_duration(end - start))),
        (Some(_), None) if check.state == CheckState::Pending => Some("running".to_string()),
        _ => None,
    };
    let parts: Vec<String> = check.workflow.iter().cloned().chain(duration).collect();
    format!("   {}", parts.join(", "))
}

fn format_duration(duration: chrono::Duration) -> String {
    let seconds = duration.num_seconds().max(0);
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3_600 => format!("{}m {}s", seconds / 60, seconds % 60),
        _ => format!("{}h {}m", seconds / 3_600, seconds % 3_600 / 60),
    }
}

// ---- Link box ----

fn render_prompt(frame: &mut Frame, prompt: &Prompt, paint: Paint) {
    let theme = paint.theme;
    let area = frame.area();
    let width = area.width.saturating_sub(4).min(100);
    let box_area = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + 2,
        width,
        height: 4,
    };
    let (title, hint) = match &prompt.kind {
        PromptKind::OpenLink => (
            " Open a pull request ".to_string(),
            "paste a PR link or owner/repo#123, enter to open, esc to cancel".to_string(),
        ),
        PromptKind::Workspace => (
            " Workspace folder ".to_string(),
            "the folder that holds your clones. enter saves it to the config, esc cancels"
                .to_string(),
        ),
        PromptKind::SearchRepo => (
            " Search the repo at the PR head ".to_string(),
            "finds text in every file, not only the changed ones. case-insensitive".to_string(),
        ),
        PromptKind::UpdatedWithin => (
            " Updated within, in days ".to_string(),
            "the inbox shows PRs updated in this many days. enter saves it and reloads, esc keeps it"
                .to_string(),
        ),
        PromptKind::FindInDiff => (
            " Find in the diff ".to_string(),
            "tab completes a word from the diff. enter finds it in every changed file; n next, N previous"
                .to_string(),
        ),
        PromptKind::ConfirmDiscard { .. } => (
            " Discard your pending review? ".to_string(),
            "y and enter deletes it and every line comment in it on GitHub; esc keeps it"
                .to_string(),
        ),
        PromptKind::ConfirmClone { pr, dir } => (
            format!(" Clone {}? ", pr.repo),
            format!(
                "no clone in the workspace. y and enter clones it into {}",
                dir.display()
            ),
        ),
    };
    let hint = match &prompt.error {
        Some(error) => paint.span(error.clone(), theme.danger),
        None => paint.span(hint, theme.muted),
    };
    let text = Text::from(vec![
        Line::from(vec![
            paint.span("> ", theme.accent),
            Span::from(prompt.text.clone()),
            paint.span("▏", theme.accent),
            paint.span(prompt.suggestion().unwrap_or_default(), theme.muted),
        ]),
        Line::from(hint),
    ]);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(theme.heading))
        .title(paint.bold(title, theme.heading))
        .style(Style::new().bg(theme.background).fg(theme.foreground));
    frame.render_widget(Clear, box_area);
    frame.render_widget(Paragraph::new(text).block(block), box_area);
}

/// GitHub's sidebar: what changed since you last looked, then the reviewers, and the
/// other sections only when they hold something.
fn sidebar_lines(
    detail: &PrDetail,
    activity: Option<&PrActivity>,
    width: usize,
    paint: Paint,
) -> Vec<Line<'static>> {
    let theme = paint.theme;
    let mut lines = Vec::new();
    if let Some(activity) = activity {
        lines.extend(since_lines(&activity.since, width, paint));
    }
    if !detail.reviewers.is_empty() {
        lines.extend(section("Reviewers", paint));
        lines.extend(reviewer_lines(&detail.reviewers, paint));
    }
    if !detail.assignees.is_empty() {
        lines.extend(section("Assignees", paint));
        let names = detail.assignees.iter();
        lines.extend(names.map(|name| Line::from(paint.span(name.clone(), theme.foreground))));
    }
    if !detail.labels.is_empty() {
        lines.extend(section("Labels", paint));
        lines.extend(label_chips(&detail.labels, width));
    }
    if let Some(milestone) = &detail.milestone {
        lines.extend(section("Milestone", paint));
        lines.push(Line::from(paint.span(milestone.clone(), theme.foreground)));
    }
    if !detail.linked_issues.is_empty() {
        lines.extend(section("Development", paint));
        lines.extend(detail.linked_issues.iter().map(|issue| {
            Line::from(vec![
                paint.span(format!("#{} ", issue.number), theme.accent),
                paint.span(issue.title.clone(), theme.foreground),
            ])
        }));
    }
    // The first section needs no blank line above it.
    if lines.first().is_some_and(|line| line.width() == 0) {
        lines.remove(0);
    }
    lines
}

/// Counts of what others did after your latest comment, review or reply, with the count
/// at the right edge as GitHub draws it.
fn since_lines(since: &SinceCounts, width: usize, paint: Paint) -> Vec<Line<'static>> {
    let theme = paint.theme;
    let title = if since.since.is_some() {
        "Since you last looked"
    } else {
        "Activity"
    };
    let mut lines = section(title, paint);
    if let Some(at) = since.since {
        let when = format!("you last commented {}", relative_time(at, paint.now));
        lines.push(Line::from(paint.span(when, theme.muted)));
    }
    let counts = [
        ("Commits", since.commits),
        ("Force pushes", since.pushes),
        ("Review comments", since.review_comments),
        ("Comments", since.comments),
        ("Review requests", since.review_requests),
    ];
    let shown: Vec<Line> = counts
        .into_iter()
        .filter(|(_, count)| *count > 0)
        .map(|(name, count)| {
            let count = count.to_string();
            let gap = width.saturating_sub(name.len() + count.len()).max(1);
            Line::from(vec![
                paint.span(format!("{name}{}", " ".repeat(gap)), theme.foreground),
                paint.bold(count, theme.foreground),
            ])
        })
        .collect();
    if shown.is_empty() {
        lines.push(Line::from(paint.span("Nothing new", theme.muted)));
    }
    lines.extend(shown);
    lines
}

/// One line per reviewer. A member who reviewed for a team sits under that team.
fn reviewer_lines(reviewers: &[Reviewer], paint: Paint) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for reviewer in reviewers.iter().filter(|reviewer| reviewer.team.is_none()) {
        lines.push(reviewer_line(reviewer, "", paint));
        let members = reviewers
            .iter()
            .filter(|member| member.team.as_ref() == Some(&reviewer.name));
        lines.extend(members.map(|member| reviewer_line(member, "└ ", paint)));
    }
    // A team that reviewed through a member but has no request of its own.
    let mut teams: Vec<&String> = reviewers
        .iter()
        .filter_map(|reviewer| reviewer.team.as_ref())
        .collect();
    teams.dedup();
    for team in teams
        .into_iter()
        .filter(|team| !reviewers.iter().any(|reviewer| &reviewer.name == *team))
    {
        lines.push(Line::from(
            paint.span(format!("  {team}"), paint.theme.foreground),
        ));
        let members = reviewers
            .iter()
            .filter(|member| member.team.as_ref() == Some(team));
        lines.extend(members.map(|member| reviewer_line(member, "└ ", paint)));
    }
    lines
}

fn reviewer_line(reviewer: &Reviewer, indent: &str, paint: Paint) -> Line<'static> {
    let theme = paint.theme;
    let (symbol, color) = reviewer_symbol(&reviewer.state, theme);
    let state = match reviewer.state.as_str() {
        "requested" => "pending",
        state => state,
    };
    let mut note = format!("  {state}");
    if reviewer.code_owner {
        note.push_str(", code owner");
    }
    Line::from(vec![
        paint.span(format!("{indent}{symbol} "), color),
        paint.span(reviewer.name.clone(), theme.foreground),
        paint.span(note, theme.muted),
    ])
}

/// Labels in their GitHub colors, as many per line as fit.
fn label_chips(labels: &[Label], width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut line: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    for label in labels {
        let text = format!(
            " {} ",
            truncate_to_width(&label.name, width.saturating_sub(2))
        );
        let text_width = display_width(&text);
        if used > 0 && used + 1 + text_width > width {
            lines.push(Line::from(std::mem::take(&mut line)));
            used = 0;
        }
        if used > 0 {
            line.push(Span::raw(" "));
            used += 1;
        }
        let (background, foreground) = markdown::label_colors(&label.color);
        line.push(Span::styled(
            text,
            Style::new().bg(background).fg(foreground),
        ));
        used += text_width;
    }
    if !line.is_empty() {
        lines.push(Line::from(line));
    }
    lines
}

fn section(title: &str, paint: Paint) -> Vec<Line<'static>> {
    vec![
        Line::from(""),
        Line::from(paint.bold(title.to_string(), paint.theme.heading)),
    ]
}

fn reviewer_symbol(state: &str, theme: &Theme) -> (&'static str, Color) {
    match state {
        "approved" => ("✓", theme.success),
        "changes requested" => ("✗", theme.danger),
        "requested" => ("●", theme.warning),
        "commented" => ("✎", theme.muted),
        _ => ("○", theme.muted),
    }
}

// ---- Text helpers ----

/// A small box of the built-in themes. The current one has a dot; enter applies the
/// selected one.
/// The settings, each with its value; Enter edits the selected one.
fn render_settings(frame: &mut Frame, app: &App, selected: usize, paint: Paint) {
    let theme = paint.theme;
    let mut lines: Vec<Line> = SETTINGS
        .iter()
        .enumerate()
        .map(|(index, setting)| {
            let line = Line::from(vec![
                paint.span(format!("  {:<18}", setting.label()), theme.foreground),
                paint.bold(app.setting_value(*setting), theme.accent),
            ]);
            match index == selected {
                true => line.style(Style::new().bg(theme.selection)),
                false => line,
            }
        })
        .collect();
    lines.push(Line::default());
    lines.push(Line::from(
        paint.span("  Saved to config.yaml as you change them", theme.muted),
    ));
    let area = frame.area();
    let width = 60.min(area.width);
    let height = (lines.len() as u16 + 2).min(area.height);
    let box_area = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(theme.heading))
        .title(paint.bold(" Settings ", theme.heading))
        .title_bottom(paint.span(" j/k move, enter edit, esc ", theme.muted))
        .style(Style::new().bg(theme.background).fg(theme.foreground));
    frame.render_widget(Clear, box_area);
    frame.render_widget(Paragraph::new(lines).block(block), box_area);
}

fn render_theme_menu(frame: &mut Frame, selected: usize, current: &str, paint: Paint) {
    let theme = paint.theme;
    let mut lines: Vec<Line> = Vec::new();
    let mut last_group = "";
    for (index, name) in crate::theme::THEME_NAMES.iter().enumerate() {
        let group = crate::theme::by_name(name).map_or("", |named| crate::theme::group_of(&named));
        if group != last_group {
            lines.push(Line::from(paint.bold(format!(" {group}"), theme.heading)));
            last_group = group;
        }
        let mark = if *name == current { "●" } else { " " };
        let line = Line::from(paint.span(format!("  {mark} {name} "), theme.foreground));
        lines.push(match index == selected {
            true => line.style(Style::new().bg(theme.selection)),
            false => line,
        });
    }
    let area = frame.area();
    let width = 36.min(area.width);
    let height = (lines.len() as u16 + 2).min(area.height);
    let box_area = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(theme.heading))
        .title(paint.bold(" Theme ", theme.heading))
        .title_bottom(paint.span(" j/k move, enter apply, esc ", theme.muted))
        .style(Style::new().bg(theme.background).fg(theme.foreground));
    frame.render_widget(Clear, box_area);
    frame.render_widget(Paragraph::new(lines).block(block), box_area);
}

/// The API budget at the right end of the top line, on every screen. It turns yellow under
/// a fifth left and red under a twentieth.
fn render_rate_limit(frame: &mut Frame, limit: RateLimit, paint: Paint) {
    let theme = paint.theme;
    let left = limit.remaining * 100 / limit.limit.max(1);
    let color = match left {
        0..5 => theme.danger,
        5..20 => theme.warning,
        _ => theme.muted,
    };
    let reset = limit.reset_at.with_timezone(&chrono::Local).format("%H:%M");
    let text = format!(
        " API: {}/{} left, resets {reset} ",
        limit.remaining, limit.limit
    );
    let width = (text.chars().count() as u16).min(frame.area().width);
    let area = frame.area();
    let spot = Rect::new(area.right() - width, area.y, width, 1);
    frame.render_widget(Clear, spot);
    frame.render_widget(
        Paragraph::new(paint.span(text, color)).style(Style::new().bg(theme.background)),
        spot,
    );
}

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

    fn texts(lines: &[Line]) -> Vec<String> {
        let text = |line: &Line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        };
        lines.iter().map(text).collect()
    }

    #[test]
    fn the_sidebar_nests_team_reviews_and_hides_empty_sections() {
        let theme = theme::by_name("dracula").unwrap();
        let paint = Paint {
            theme: &theme,
            now: Utc::now(),
        };
        let mut detail = crate::model::tests::sample_detail();
        detail.assignees.clear();
        detail.reviewers = vec![
            Reviewer {
                name: "DATA".into(),
                state: "requested".into(),
                code_owner: true,
                team: None,
            },
            Reviewer {
                name: "joey".into(),
                state: "commented".into(),
                code_owner: false,
                team: Some("Core".into()),
            },
        ];
        let lines = texts(&sidebar_lines(&detail, None, 30, paint));
        assert_eq!(lines[0], "Reviewers");
        assert_eq!(lines[1], "● DATA  pending, code owner");
        assert_eq!(lines[2], "  Core");
        assert_eq!(lines[3], "└ ✎ joey  commented");
        assert!(
            !lines.iter().any(|line| line == "Assignees"),
            "no one assigned, no section"
        );
    }

    #[test]
    fn the_sidebar_counts_only_what_is_new() {
        let theme = theme::by_name("dracula").unwrap();
        let paint = Paint {
            theme: &theme,
            now: Utc::now(),
        };
        let since = SinceCounts {
            since: Some(Utc::now()),
            commits: 5,
            comments: 1,
            ..Default::default()
        };
        let lines = texts(&since_lines(&since, 20, paint));
        assert_eq!(lines[1], "Since you last looked");
        assert_eq!(lines[3], format!("Commits{}5", " ".repeat(12)));
        assert_eq!(lines.len(), 5, "rows with nothing new stay out: {lines:?}");
    }

    #[test]
    fn a_shrunk_layout_keeps_the_cursor_on_screen() {
        assert_eq!(keep_cursor_in_view(170, 198, 30, 200), 170);
        assert_eq!(
            keep_cursor_in_view(170, 120, 30, 140),
            110,
            "a fold closed under the page"
        );
        assert_eq!(keep_cursor_in_view(50, 10, 30, 200), 10);
    }

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

    #[test]
    fn durations_use_two_units_at_most() {
        assert_eq!(format_duration(chrono::Duration::seconds(42)), "42s");
        assert_eq!(format_duration(chrono::Duration::seconds(150)), "2m 30s");
        assert_eq!(format_duration(chrono::Duration::seconds(3_900)), "1h 5m");
    }
}
