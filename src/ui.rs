use chrono::{DateTime, Local, Utc};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Padding, Paragraph, Wrap,
};
use unicode_width::UnicodeWidthChar;

use crate::app::{
    App, Click, ClickTarget, ComposeKind, Composer, DetailView, DiffOutput, EditorChoice,
    EditorMenu, LabelMenu, MergeItem, MergeMenu, OpenMenu, Overlay, OverviewLayout, Prompt,
    PromptKind, RepoState, Row, SETTINGS, SaveNote, Screen, Tab,
};
use crate::editor::Editor;
use crate::file_tree::FileRow;
use crate::gallery::{self, Gallery, Loaded};
use crate::github::GhProblem;
use crate::line_diff::{DiffRow, RowKind};
use crate::model::{
    ChangedFile, CheckLine, CheckState, CheckSummary, CommitLine, Group, Label, Lifecycle,
    MergeMethod, MergeRequest, PrActivity, PrDetail, PrRef, PrSummary, RateLimit, ReviewState,
    Reviewer, Side, SinceCounts, ThreadLine, Verdict, ViewedState,
};
use crate::theme::{ON_STATE, PrState, Theme};
use crate::{markdown, overview};
use ratatui_image::picker::Picker;

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
    if let Some(problem) = &app.gh_problem {
        render_gh_problem(frame, problem, app.gh_checked_at, paint);
        return;
    }
    if let Screen::Detail(view) = &mut app.screen {
        // A renderer warning waits for a tab that shows a diff, where it applies.
        let diff_tab = matches!(view.tab, Tab::Files | Tab::Commits);
        let notice = app.notice.as_deref().filter(|_| diff_tab);
        app.notice_seen |= notice.is_some();
        let status = status_span(
            app.error.as_deref(),
            app.saving,
            app.saved.as_deref(),
            notice,
            paint,
        );
        render_detail(frame, view, status, &app.renderer_names, paint);
    } else {
        render_inbox(frame, app, paint);
    }
    if let Some(limit) = app.rate_limit {
        render_rate_limit(frame, limit, paint);
    }
    if let Some(selected) = app.settings_menu {
        render_settings(frame, app, selected, paint);
    }
    if let Some(menu) = &app.label_menu {
        render_label_menu(frame, menu, paint);
    }
    if let Some(menu) = &app.merge_menu {
        render_merge_menu(frame, menu, paint);
    }
    if let Some(menu) = &app.open_menu {
        render_open_menu(frame, menu, paint);
    }
    if let Some(menu) = &app.editor_menu {
        render_editor_menu(frame, menu, paint);
    }
    if let Some(selected) = app.theme_menu {
        render_theme_menu(frame, selected, app.theme_name, paint);
    }
    if app.show_help {
        app.help_scroll = render_help(frame, app.help_scroll, paint);
    }
    if let Some(prompt) = &app.prompt {
        render_prompt(frame, prompt, paint);
    }
    if let Some(composer) = &app.composer {
        render_composer(frame, composer, paint);
    }
    if let (Some(gallery), Some(picker)) = (app.gallery.as_mut(), &app.picker) {
        render_gallery(frame, gallery, picker, paint);
    }
}

/// The gallery covers the screen: the image centered, its place in the list on top, and
/// the keys at the bottom.
fn render_gallery(frame: &mut Frame, gallery: &mut Gallery, picker: &Picker, paint: Paint) {
    let theme = paint.theme;
    let screen = frame.area();
    frame.render_widget(Clear, screen);
    let title = format!(" Image {} of {} ", gallery.at + 1, gallery.urls.len());
    let frame_block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.accent))
        .title(Line::from(paint.bold(title, theme.heading)))
        .style(Style::new().bg(theme.background));
    frame.render_widget(frame_block, screen);
    let keys = Rect {
        x: screen.x + 1,
        y: screen.bottom().saturating_sub(2),
        width: screen.width.saturating_sub(2),
        height: 1,
    };
    frame.render_widget(
        Paragraph::new(paint.span(
            " h/l previous/next  o open in browser  esc close",
            theme.muted,
        )),
        keys,
    );
    let area = gallery::image_area(screen);
    let url = gallery.current().to_string();
    let message = match gallery.loaded.get_mut(&url) {
        None | Some(Loaded::Loading) => Some((format!("loading {url}"), theme.warning)),
        Some(Loaded::Failed(error)) => Some((
            format!("{error}\n{url}\no opens it in the browser"),
            theme.danger,
        )),
        Some(Loaded::Ready(shown)) => draw_image(frame, shown, picker, area)
            .err()
            .map(|error| (error, theme.danger)),
    };
    if let Some((text, color)) = message {
        let paragraph =
            Paragraph::new(Text::styled(text, Style::new().fg(color))).wrap(Wrap { trim: false });
        frame.render_widget(paragraph, area);
    }
}

/// Draws the image in the middle of `area`, encoding it again when the space changed.
fn draw_image(
    frame: &mut Frame,
    shown: &mut gallery::Shown,
    picker: &Picker,
    area: Rect,
) -> Result<(), String> {
    if shown.fit != area.as_size() {
        gallery::refit(shown, picker, area.as_size())?;
    }
    let size = shown.protocol.size();
    let centered = Rect {
        x: area.x + area.width.saturating_sub(size.width) / 2,
        y: area.y + area.height.saturating_sub(size.height) / 2,
        width: size.width.min(area.width),
        height: size.height.min(area.height),
    };
    frame.render_widget(ratatui_image::Image::new(&shown.protocol), centered);
    Ok(())
}

/// The review or line-comment box: what it sends, the text, and the keys.
fn render_composer(frame: &mut Frame, composer: &Composer, paint: Paint) {
    let theme = paint.theme;
    let area = frame.area();
    let width = area.width.saturating_sub(4).min(100);
    let (title, header) = composer_header(composer, paint);
    // The box fits the text: at least 3 text rows, growing to three fifths of the screen.
    let (_, _, text_rows) = editor_lines(&composer.editor, width.saturating_sub(2), paint);
    let wanted = header.len() as u16 + 1 + text_rows.max(3) + 1 + 2;
    let height = wanted.min((area.height * 3 / 5).max(8)).min(area.height);
    let box_area = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
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
        .border_type(BorderType::Rounded)
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
    let (lines, cursor_row, _) = editor_lines(&composer.editor, text_area.width, paint);
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
            comment_only,
            drafts,
        } => {
            let mut choice = vec![paint.span(" ", theme.muted)];
            let verdicts = match comment_only {
                Some(_) => &[Verdict::Comment][..],
                None => &[Verdict::Comment, Verdict::Approve, Verdict::RequestChanges][..],
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
            let note = format!("  {}", comment_only.unwrap_or("tab switches"));
            choice.push(paint.span(note, theme.muted));
            let drafts = match drafts {
                0 => {
                    " no line comments; S in Files changed selects lines to comment on".to_string()
                }
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
        ComposeKind::Reply { author, place, .. } => {
            let title = format!(" Reply to {author} on {place} ");
            let note = match composer.pending_review {
                Some(_) => {
                    " you have a pending review, so the reply joins it; R submits the review"
                }
                None => " GitHub posts the reply at once",
            };
            (title, vec![Line::from(paint.span(note, theme.muted))])
        }
    }
}

/// The editor's text with the selection and the cursor marked, the row the cursor is
/// on once wrapped to `width` so the box can scroll to it, and the rows in all.
fn editor_lines(editor: &Editor, width: u16, paint: Paint) -> (Vec<Line<'static>>, u16, u16) {
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
    (lines, cursor_row as u16, rows_before as u16)
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

    if let Some(errors) = app.inbox_failure() {
        render_inbox_failure(frame, &errors, body, paint);
        frame.render_widget(
            Paragraph::new(hints_line(INBOX_HINTS, usize::from(bottom.width), paint)),
            bottom,
        );
        return;
    }
    if app.groups.is_empty() && !app.loading {
        let hint = "No groups in config.yaml. Add some under groups:, or remove the groups \
                    key for the built-in ones. r reloads the config.";
        frame.render_widget(
            Paragraph::new(paint.span(hint, paint.theme.muted)).wrap(Wrap { trim: true }),
            body.inner(Margin::new(1, 1)),
        );
    }
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
        Paragraph::new(hints_line(INBOX_HINTS, usize::from(bottom.width), paint)),
        bottom,
    );
}

fn inbox_status(app: &App, paint: Paint) -> Line<'static> {
    let theme = paint.theme;
    let mut spans = vec![paint.bold(" revu ", theme.heading)];
    if app.loading {
        spans.push(paint.span(" loading...", theme.warning));
    } else if let Some(fetched_at) = app.fetched_at {
        spans.push(paint.span(
            format!(" fetched {}", relative_time(fetched_at, paint.now)),
            theme.muted,
        ));
    }
    // An inbox where every group failed shows the error in full below.
    if let Some(error) = app.error.as_ref().filter(|_| app.inbox_failure().is_none()) {
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
        pad_span(checks_span(pr.checks.as_ref(), paint), 9),
        // No comments and no checks leave their cells blank, so the rows that have them
        // stand out. The blank keeps the cell's width, so every row's badges line up.
        pad_span(
            match pr.comments {
                0 => Span::raw(""),
                1 => paint.span("    1 comment", secondary),
                count => paint.span(format!("{count:>5} comments"), secondary),
            },
            14,
        ),
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

fn checks_span(checks: Option<&CheckSummary>, paint: Paint) -> Span<'static> {
    let Some(checks) = checks else {
        return Span::raw("");
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
        CheckState::Skipped => ("○", theme.muted),
    }
}

fn pad_span(span: Span<'static>, width: usize) -> Span<'static> {
    let style = span.style;
    Span::styled(pad_to_width(&span.content, width), style)
}

// ---- Detail ----

/// A footer's keys, most used first; the footer drops from the end what does not fit.
type Hints = &'static [(&'static str, &'static str)];

const INBOX_HINTS: Hints = &[
    ("j/k", "move"),
    ("enter", "open"),
    ("space", "fold"),
    ("tab", "next group"),
    ("o", "browser"),
    ("r", "refresh"),
    ("ctrl-l", "open a link"),
    (",", "settings"),
];

fn detail_hints(view: &DetailView) -> Hints {
    if let Some(overlay) = view.overlays.last() {
        return match overlay {
            Overlay::File { .. } if view.def_walk.is_some() => &[
                ("j/k", "scroll"),
                ("n/N", "next/previous definition"),
                ("o", "open"),
                ("esc", "back"),
            ],
            Overlay::File { .. } if view.repo_hit.is_some() => &[
                ("j/k", "scroll"),
                ("n/N", "next/previous match"),
                ("o", "open"),
                ("esc", "back to the diff"),
            ],
            Overlay::File { .. } => &[
                ("j/k", "scroll"),
                ("space", "page down"),
                ("g", "top"),
                ("G", "bottom"),
                ("o", "open"),
                ("esc", "back"),
            ],
        };
    }
    let threads = match (&view.line_mode, &view.activity) {
        (Some(mode), Some(Ok(activity))) => activity.threads_on(&mode.path).next().is_some(),
        _ => false,
    };
    if threads {
        return &[
            ("j/k", "line"),
            ("V", "range"),
            ("enter", "comment"),
            ("t", "threads"),
            ("]/[", "next thread"),
            ("r", "reply"),
            ("u", "resolve"),
            ("y", "copy"),
            ("D", "definition"),
            ("o", "open"),
            ("R", "review"),
            ("esc", "back to the diff"),
        ];
    }
    if view.line_mode.is_some() {
        return &[
            ("j/k", "line"),
            ("V", "range"),
            ("enter", "comment"),
            ("y", "copy"),
            ("D", "definition"),
            ("o", "open"),
            ("R", "review"),
            ("w", "wrap"),
            ("esc", "back to the diff"),
        ];
    }
    match view.tab {
        Tab::Overview => &[
            ("j/k", "comment"),
            ("enter", "fold"),
            ("R", "review"),
            ("e", "labels"),
            ("i", "images"),
            ("f", "filter"),
            ("F", "order"),
            ("n", "bots"),
            ("o", "browser"),
            ("h", "back"),
        ],
        Tab::Checks => &[
            ("j/k", "move"),
            ("enter", "open the run"),
            ("r", "reload"),
            ("h", "back"),
        ],
        Tab::Commits => &[
            ("j/k", "commit"),
            ("J", "scroll diff"),
            ("/", "find"),
            ("m", "renderer"),
            ("w", "wrap"),
            ("o", "browser"),
            ("h", "back"),
        ],
        Tab::Files => &[
            ("v", "viewed"),
            ("]", "next"),
            ("S", "select lines"),
            ("y", "copy path"),
            ("enter", "full file"),
            ("/", "find"),
            ("R", "review"),
            ("m", "view"),
            ("o", "open"),
            ("t", "tree"),
            ("z", "list"),
        ],
    }
}

/// The keys that fit in `width`, each key in the accent color, and "? all keys" last
/// so the dropped ones stay one key away.
fn hints_line(hints: Hints, width: usize, paint: Paint) -> Line<'static> {
    let theme = paint.theme;
    let pair = |key: &str, action: &str| {
        vec![
            paint.span(key.to_string(), theme.accent),
            paint.span(format!(" {action}"), theme.muted),
        ]
    };
    let tail = pair("?", "all keys");
    let separator = "  ";
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 1 + display_width("? all keys");
    for (key, action) in hints {
        let size = display_width(key) + 1 + display_width(action) + separator.len();
        if used + size > width {
            break;
        }
        used += size;
        spans.push(Span::raw(if spans.is_empty() { " " } else { separator }));
        spans.extend(pair(key, action));
    }
    spans.push(Span::raw(if spans.is_empty() { " " } else { separator }));
    spans.extend(tail);
    Line::from(spans)
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
                "settings: theme, workspace folders, how far back the inbox looks",
            ),
            ("?", "this list; j/k scroll it, any other key closes it"),
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
            ("h, q", "back to the inbox"),
            ("esc", "clear the find"),
            ("g/G, space/b", "top, bottom, page down, page up"),
            ("o, d, r", "open in the browser, run open_diff, fetch again"),
            ("o on a file", "open it on GitHub or in your editor"),
            (
                "R",
                "review: Comment, Approve or Request changes, with your line comments",
            ),
            ("X", "discard your pending review and its line comments"),
            (
                "M",
                "merge: pick merge commit, squash or rebase; auto-merge when blocked, or turn it off",
            ),
        ],
    ),
    (
        "Overview",
        &[
            (
                "j/k, wheel",
                "next or previous comment, thread or fold; a long one by a third of a screen",
            ),
            (
                "enter",
                "open or close the folded block under the cursor; on an image, show it",
            ),
            (
                "e",
                "labels: tick or untick the repo's labels, enter applies",
            ),
            (
                "i",
                "images: every image in the PR, h/l previous/next, o browser, esc closes",
            ),
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
            (
                "v, V",
                "toggle Viewed (syncs to GitHub; on a folder, every file in it), hide viewed files",
            ),
            ("], [", "next or previous unviewed file"),
            ("t, z", "tree or flat list, hide or show the list"),
            ("y", "copy the file's or folder's path"),
            (
                "o",
                "open GitHub's diff at the file, or at the line in select lines",
            ),
            (
                "S",
                "select lines: j/k or the wheel pick a line, J/K and space/b scroll, V a range; enter comments in your pending review, y copies them as a Markdown diff, D goes to the definition of a name on the line, t opens or closes every review thread, ] and [ go to the next and previous thread, r replies to the thread on the line and u resolves or reopens it, o opens them on GitHub",
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
            (
                "/, n/N",
                "find in the diff, then in the rest of the repo; next and previous match",
            ),
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

/// The key list, as wide as the screen allows, with long descriptions wrapped under
/// their own column. Returns the scroll kept within the list.
fn render_help(frame: &mut Frame, scroll: u16, paint: Paint) -> u16 {
    let theme = paint.theme;
    let area = frame.area();
    let width = area.width.saturating_sub(4).min(120);
    let key_width = KEY_LIST
        .iter()
        .flat_map(|(_, keys)| keys.iter())
        .map(|(keys, _)| display_width(keys))
        .max()
        .unwrap_or(0);
    let indent = key_width + 4;
    let mut lines = Vec::new();
    for (section, keys) in KEY_LIST {
        lines.push(Line::from(paint.bold(section.to_string(), theme.heading)));
        for (keys, action) in keys.iter() {
            let spans = [
                paint.span(
                    format!("  {}", pad_to_width(keys, key_width + 2)),
                    theme.accent,
                ),
                paint.span(action.to_string(), theme.foreground),
            ];
            lines.extend(markdown::wrap(
                &spans,
                usize::from(width.saturating_sub(2)),
                indent,
            ));
        }
        lines.push(Line::from(""));
    }
    lines.pop();
    let height = (lines.len() as u16 + 2).min(area.height);
    let last = (lines.len() as u16 + 2).saturating_sub(height);
    let scroll = scroll.min(last);
    let box_area = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    let hint = match last {
        0 => " any other key closes ".to_string(),
        _ => format!(
            " j/k, space/b scroll ({}/{}), any other key closes ",
            scroll, last
        ),
    };
    let block = popup_block(" Keys ", paint).title_bottom(paint.span(hint, theme.muted));
    frame.render_widget(Clear, box_area);
    frame.render_widget(
        Paragraph::new(lines).block(block).scroll((scroll, 0)),
        box_area,
    );
    scroll
}

/// Every popup's frame: rounded like the Overview cards, titled in the heading color.
fn popup_block(title: &str, paint: Paint) -> Block<'static> {
    let theme = paint.theme;
    Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.heading))
        .title(paint.bold(title.to_string(), theme.heading))
        .style(Style::new().bg(theme.background).fg(theme.foreground))
}

fn render_detail(
    frame: &mut Frame,
    view: &mut DetailView,
    status: Option<Span<'static>>,
    renderer_names: &[String],
    paint: Paint,
) {
    let theme = paint.theme;
    let width = frame.area().width as usize;
    let header = match (&view.detail, &view.summary) {
        (Some(Ok(detail)), _) => detail_header(detail, width, paint),
        (_, Some(summary)) => summary_header(summary, width, paint),
        _ => detail_title(&view.pr, paint),
    };
    let [top, tabs, body, bottom] = Layout::vertical([
        Constraint::Length(header.len() as u16),
        Constraint::Length(2),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    // An error, a save in progress, or a finished save replaces the help line.
    let footer = match status {
        Some(status) => Line::from(status),
        None => hints_line(detail_hints(view), usize::from(bottom.width), paint),
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
    let place = repo_place(view, paint);
    let finding = repo_finding(view);
    if let Some(overlay) = view.overlays.last_mut() {
        let targets = render_overlay(frame, overlay, body, place, finding.as_ref(), paint);
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

/// Returns what a click can hit: the search hits.
fn render_overlay(
    frame: &mut Frame,
    overlay: &mut Overlay,
    area: Rect,
    place: (Vec<Piece>, Vec<Piece>),
    finding: Option<&RepoFinding>,
    paint: Paint,
) -> Vec<ClickTarget> {
    let theme = paint.theme;
    match overlay {
        Overlay::File {
            path,
            lines,
            scroll,
            line,
        } => {
            let at_line = line.map(|line| format!(":{line}")).unwrap_or_default();
            let path_style = paint.bold("", theme.heading).style;
            let left = vec![
                path_piece(path_style, [" ", path, &format!("{at_line} ")]),
                Piece::new(paint.span("at the PR head ", theme.heading), RANK_HINT),
            ];
            let (right, below) = place;
            let title = TitleRows::new(usize::from(area.width), left, right, below);
            let block = Block::new().title(title.first.clone());
            let inner = block.inner(area);
            frame.render_widget(block, area);
            let inner = title.draw_second(frame, inner);
            let text = match lines {
                None => Text::from(paint.span(" loading...", theme.warning)),
                Some(Err(error)) => Text::from(paint.span(format!(" {error}"), theme.danger)),
                // Clone only the visible lines; a whole file is redrawn every frame.
                Some(Ok(text)) => Text::from(
                    text.lines
                        .iter()
                        .enumerate()
                        .skip(*scroll as usize)
                        .take(inner.height as usize)
                        .map(|(index, line)| match finding {
                            Some(finding) => finding.mark(index, line, paint),
                            None => line.clone(),
                        })
                        .collect::<Vec<_>>(),
                ),
            };
            // A file read at the PR head wraps, so a match past the right edge shows.
            let paragraph = match line {
                Some(_) => Paragraph::new(text).wrap(Wrap { trim: false }),
                None => Paragraph::new(text),
            };
            frame.render_widget(paragraph, inner);
        }
    }
    Vec::new()
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
            let activity = view
                .activity
                .as_ref()
                .and_then(|result| result.as_ref().ok());
            let line = file_row_line(row, files, activity, view.tree_view, width, paint);
            let item = ListItem::new(line);
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
    let path = diff_pane_path(view);
    let path = path.trim();
    let edge = if path.is_empty() { "" } else { " " };
    let path_style = paint.bold("", theme.foreground).style;
    let left = vec![
        Piece::new(
            paint.bold(format!(" {renderer} "), theme.heading),
            RANK_VIEW,
        ),
        Piece::new(paint.span("m switches ", theme.muted), RANK_HINT),
        Piece::new(paint.span(wrap_note(view), theme.muted), RANK_WRAP),
        path_piece(path_style, [edge, path, edge]),
    ];
    let right = find_note(view, paint);
    let mut below = Vec::new();
    let threads = selected_thread_note(view);
    if !threads.is_empty() {
        below.push(Piece::new(paint.span(threads, theme.accent), RANK_THREADS));
    }
    below.extend(view.hunk_place().map(|place| block_piece(&place, paint)));
    let title_width = usize::from(area.width.saturating_sub(1));
    let title = TitleRows::new(title_width, left, right, below);
    let block = Block::new()
        .borders(Borders::LEFT)
        .border_style(Style::new().fg(theme.muted))
        .title(title.first.clone());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let inner = title.draw_second(frame, inner);
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
                if !view.diff_wrap {
                    // Stop where the longest visible line ends at the right edge.
                    let longest = lines.iter().map(Line::width).max().unwrap_or(0);
                    let last = longest.saturating_sub(inner.width as usize) as u16;
                    view.diff_shift = view.diff_shift.min(last);
                }
                let shift = (!view.diff_wrap).then_some(view.diff_shift);
                render_diff_lines(frame, lines, inner, shift);
                return;
            }
        },
    };
    frame.render_widget(content, inner);
}

/// Draws diff lines one under another, wrapped, or cut and shifted right by `shift`. A
/// line with a background in its style, a code row with syntax colors, gets it across
/// every row it takes, to the pane's edge.
fn render_diff_lines(frame: &mut Frame, lines: Vec<Line<'static>>, area: Rect, shift: Option<u16>) {
    let mut top = area.y;
    for line in lines {
        if top >= area.bottom() {
            break;
        }
        let background = line.style.bg;
        let paragraph = match shift {
            Some(shift) => Paragraph::new(line).scroll((0, shift)),
            None => Paragraph::new(line).wrap(Wrap { trim: false }),
        };
        let rows = match shift {
            Some(_) => 1,
            None => paragraph.line_count(area.width).max(1) as u16,
        };
        let rect = Rect {
            y: top,
            height: rows.min(area.bottom() - top),
            ..area
        };
        if let Some(background) = background {
            frame
                .buffer_mut()
                .set_style(rect, Style::new().bg(background));
        }
        frame.render_widget(paragraph, rect);
        top += rows;
    }
}

/// The PR footer's status: an error first, then a write to GitHub in progress, then
/// the last write's success.
fn status_span(
    error: Option<&str>,
    saving: Option<SaveNote>,
    saved: Option<&str>,
    notice: Option<&str>,
    paint: Paint,
) -> Option<Span<'static>> {
    let theme = paint.theme;
    if let Some(error) = error {
        return Some(paint.span(error.to_string(), theme.danger));
    }
    if let Some(note) = saving {
        return Some(paint.span(format!("◷ {}", note.doing), theme.warning));
    }
    if let Some(done) = saved {
        return Some(paint.span(format!("✓ {done}"), theme.success));
    }
    notice.map(|text| paint.span(format!("! {text}"), theme.warning))
}

/// Every group failed for a reason other than gh, such as the rate limit or GitHub
/// itself, so the error takes the inbox's place in full.
fn render_inbox_failure(frame: &mut Frame, errors: &[String], area: Rect, paint: Paint) {
    let theme = paint.theme;
    let mut lines = vec![
        Line::from(paint.bold("The inbox could not load. GitHub said:", theme.danger)),
        Line::default(),
    ];
    for error in errors {
        lines.extend(
            error
                .lines()
                .map(|line| Line::from(paint.span(line.to_string(), theme.foreground))),
        );
        lines.push(Line::default());
    }
    lines.push(Line::from(paint.span("r tries again", theme.muted)));
    let area = area.inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

/// Without a working gh the app can fetch nothing, so the whole screen says what is
/// wrong and the command that fixes it, instead of an inbox of empty groups.
fn render_gh_problem(
    frame: &mut Frame,
    problem: &GhProblem,
    checked_at: Option<DateTime<Utc>>,
    paint: Paint,
) {
    let theme = paint.theme;
    let command = |text: &str| Line::from(paint.span(format!("  {text}"), theme.accent));
    let plain = |text: &str| Line::from(paint.span(text.to_string(), theme.foreground));
    let (title, mut lines) = match problem {
        GhProblem::Missing => (
            " The GitHub CLI is not installed ",
            vec![
                plain("This app reads GitHub through the gh command, and gh is not on PATH."),
                plain(""),
                plain("Install it, then log in:"),
                command(crate::github::GH_INSTALL),
                command("gh auth login"),
            ],
        ),
        GhProblem::LoggedOut(said) => {
            let mut lines = vec![
                plain("gh is installed but cannot reach GitHub. gh says:"),
                plain(""),
            ];
            lines.extend(
                said.lines()
                    .map(|line| Line::from(paint.span(format!("  {}", line.trim()), theme.muted))),
            );
            lines.extend([
                plain(""),
                plain("Log in, or log in again:"),
                command("gh auth login"),
            ]);
            (" The GitHub CLI is not logged in ", lines)
        }
        GhProblem::Offline(error) => (
            " Cannot reach GitHub ",
            vec![
                plain("gh could not connect to GitHub, so nothing can load. gh hit:"),
                plain(""),
                Line::from(paint.span(format!("  {error}"), theme.muted)),
                plain(""),
                plain("Check the network, a VPN or a proxy (HTTPS_PROXY), then press r."),
            ],
        ),
    };
    // The time changes on every r, so a retry that fails again still shows it ran.
    let checked = checked_at.map_or(String::new(), |at| {
        format!(
            "checked at {}. ",
            at.with_timezone(&Local).format("%H:%M:%S")
        )
    });
    lines.extend([
        plain(""),
        Line::from(paint.span(format!("{checked}r checks again, q quits"), theme.muted)),
    ]);
    let area = frame.area();
    let width = lines
        .iter()
        .map(Line::width)
        .max()
        .unwrap_or(0)
        .saturating_add(4)
        .min(usize::from(area.width)) as u16;
    // A line wider than the screen wraps, and each wrapped row needs room.
    let inner = usize::from(width.saturating_sub(2)).max(1);
    let rows: usize = lines
        .iter()
        .map(|line| line.width().div_ceil(inner).max(1))
        .sum();
    let height = (rows as u16 + 2).min(area.height);
    let popup = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    let block = popup_block(title, paint);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        popup,
    );
}

/// Select lines: the file's plain diff with old and new line numbers, the cursor and
/// the range V marked, and a dot on lines your pending review already comments on.
fn render_line_mode(frame: &mut Frame, view: &mut DetailView, area: Rect, paint: Paint) {
    let theme = paint.theme;
    let (top_threads, row_threads) =
        line_mode_threads(view, usize::from(area.width.saturating_sub(1)), paint);
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
    let place = view.line_place();
    let Some(mode) = view.line_mode.as_mut() else {
        return;
    };
    let path_style = paint.bold("", theme.foreground).style;
    let left = vec![
        Piece::new(paint.bold(" select lines ", theme.accent), RANK_VIEW),
        path_piece(path_style, [" ", &mode.path, " "]),
    ];
    let below = place
        .map(|place| block_piece(&place, paint))
        .into_iter()
        .collect();
    let title_width = usize::from(area.width.saturating_sub(1));
    let title = TitleRows::new(title_width, left, Vec::new(), below);
    let block = Block::new()
        .borders(Borders::LEFT)
        .border_style(Style::new().fg(theme.accent))
        .title(title.first.clone());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let inner = title.draw_second(frame, inner);
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
    let width = usize::from(inner.width);
    let wrap = view.diff_wrap;
    // A row's height counts the review threads under it; the first row's, the outdated
    // threads above it.
    let heights: Vec<usize> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let code = match wrap {
                true => split_to_width(&row.text, width.saturating_sub(SELECT_GUTTER)).len(),
                false => 1,
            };
            let above = if index == 0 { top_threads.len() } else { 0 };
            code + above + row_threads.get(index).map_or(0, Vec::len)
        })
        .collect();
    // Where each row's block starts, in screen rows, and where the code of the cursor's
    // row starts inside its block.
    let starts: Vec<usize> = heights
        .iter()
        .scan(0, |at, height| {
            let start = *at;
            *at += height;
            Some(start)
        })
        .collect();
    let total: usize = heights.iter().sum();
    if view.line_followed != Some(mode.cursor) {
        let block = starts[mode.cursor];
        let code = block
            + if mode.cursor == 0 {
                top_threads.len()
            } else {
                0
            };
        let shown = Followed {
            block,
            code,
            end: block + heights[mode.cursor],
        };
        mode.scroll = shown.scroll(mode.scroll, height);
        view.line_followed = Some(mode.cursor);
    }
    mode.scroll = mode.scroll.min(total.saturating_sub(height));
    let first_row = starts
        .partition_point(|start| *start <= mode.scroll)
        .saturating_sub(1);
    let skip = mode.scroll - starts[first_row];
    let (first, last) = mode.marked();
    let lines: Vec<Tagged> = rows
        .iter()
        .enumerate()
        .skip(first_row)
        .flat_map(|(index, row)| {
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
            let lines = select_row(row, mark, width, wrap, paint);
            let lines: Vec<Tagged> = match (first..=last).contains(&index) {
                true => lines
                    .into_iter()
                    .map(|line| (line.style(Style::new().bg(theme.selection)), None))
                    .collect(),
                false => lines.into_iter().map(|line| (line, None)).collect(),
            };
            let above = if index == 0 {
                top_threads.clone()
            } else {
                Vec::new()
            };
            let below = row_threads.get(index).cloned().unwrap_or_default();
            above.into_iter().chain(lines).chain(below)
        })
        .skip(skip)
        .take(height)
        .collect();
    for (offset, (_, thread)) in lines.iter().enumerate() {
        if let Some(index) = thread {
            view.clicks.push(ClickTarget {
                area: Rect::new(inner.x, inner.y + offset as u16, inner.width, 1),
                click: Click::Thread(*index),
            });
        }
    }
    let lines: Vec<Line> = lines.into_iter().map(|(line, _)| line).collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The review threads in select lines: the outdated ones, listed above the first row,
/// and each row's, drawn under it.
fn line_mode_threads(
    view: &DetailView,
    width: usize,
    paint: Paint,
) -> (Vec<Tagged>, Vec<Vec<Tagged>>) {
    let (Some(mode), Some(Ok(activity))) = (&view.line_mode, &view.activity) else {
        return (Vec::new(), Vec::new());
    };
    let Some(Ok(rows)) = &mode.rows else {
        return (Vec::new(), Vec::new());
    };
    let band = Style::new().bg(paint.theme.thread_band());
    // A thread's lines, on the band, each tagged with the thread's place so a click
    // finds it.
    let drawn = |thread: &ThreadLine, place: Option<&str>| -> Vec<Tagged> {
        let index = activity
            .threads_on(&mode.path)
            .position(|other| other.id == thread.id);
        let open = view.thread_open(&thread.id);
        thread_lines(thread, open, place, width, paint)
            .into_iter()
            .map(|mut line| {
                // Padded to the pane's width, so the band fills the row.
                let pad = width.saturating_sub(markdown::line_width(&line));
                line.spans.push(Span::raw(" ".repeat(pad)));
                (line.patch_style(band), index)
            })
            .collect()
    };
    let under: Vec<Vec<Tagged>> = rows
        .iter()
        .map(|row| {
            let threads = view.threads_on_row(&mode.path, row);
            threads
                .iter()
                .flat_map(|thread| drawn(thread, None))
                .collect()
        })
        .collect();
    let placed = |thread: &&ThreadLine| {
        let at = thread.at;
        at.is_some_and(|at| rows.iter().any(|row| crate::app::row_holds(row, at)))
    };
    let outdated: Vec<&ThreadLine> = activity
        .threads_on(&mode.path)
        .filter(|thread| !placed(thread))
        .collect();
    let mut top = Vec::new();
    if !outdated.is_empty() {
        let note = " outdated review threads, no longer on a line of this diff:";
        top.push((Line::from(paint.span(note, paint.theme.muted)), None));
        for thread in outdated {
            top.extend(drawn(thread, Some(&thread.lines)));
        }
    }
    (top, under)
}

/// A line in select lines, with the place of the review thread it belongs to.
type Tagged = (Line<'static>, Option<usize>);

/// A review thread under its line: folded to its first comment, or open with every
/// comment. `place` names the line of a thread that is not drawn under one.
fn thread_lines(
    thread: &ThreadLine,
    open: bool,
    place: Option<&str>,
    width: usize,
    paint: Paint,
) -> Vec<Line<'static>> {
    let theme = paint.theme;
    let Some(first) = thread.comments.first() else {
        return Vec::new();
    };
    let color = if thread.resolved {
        theme.muted
    } else {
        theme.accent
    };
    let text = if thread.resolved {
        theme.muted
    } else {
        theme.foreground
    };
    let indent = || Span::raw(" ".repeat(SELECT_GUTTER));
    let resolved = if thread.resolved {
        "  ✓ resolved"
    } else {
        ""
    };
    if !open {
        let more = match thread.comments.len() {
            1 => String::new(),
            count => format!("  ({} more)", count - 1),
        };
        let place = place.map(|place| format!("{place}, ")).unwrap_or_default();
        let gist = first_rendered_line(&first.body, width, theme);
        let used = SELECT_GUTTER
            + 2
            + display_width(&place)
            + display_width(&first.author.login)
            + 2
            + display_width(&more)
            + display_width(resolved);
        let gist = markdown::truncate(&gist, width.saturating_sub(used));
        return vec![Line::from(vec![
            indent(),
            paint.span("╭ ", color),
            paint.span(place, theme.muted),
            paint.bold(first.author.login.clone(), color),
            paint.span(format!(": {gist}"), text),
            paint.span(format!("{more}{resolved}"), theme.muted),
        ])];
    }
    let body_width = width.saturating_sub(SELECT_GUTTER + 2);
    let toggled = std::collections::HashSet::new();
    let mut lines = Vec::new();
    for (index, comment) in thread.comments.iter().enumerate() {
        let corner = if index == 0 { "╭ " } else { "├ " };
        let place = match (index, place) {
            (0, Some(place)) => format!("{place}, "),
            _ => String::new(),
        };
        lines.push(Line::from(vec![
            indent(),
            paint.span(corner, color),
            paint.span(place, theme.muted),
            paint.bold(comment.author.login.clone(), color),
            paint.span(
                format!("  {}", relative_time(comment.at, paint.now)),
                theme.muted,
            ),
        ]));
        let mut folds = markdown::Folds::new(&toggled);
        let body = markdown::layout(
            &markdown::parse(&comment.body, theme),
            body_width,
            &mut folds,
            theme,
        );
        for row in body {
            let mut spans = vec![indent(), paint.span("│ ", color)];
            spans.extend(
                row.line
                    .spans
                    .into_iter()
                    .map(|span| off_selection(span, theme)),
            );
            lines.push(Line::from(spans));
        }
    }
    lines.push(Line::from(vec![
        indent(),
        paint.span("╰", color),
        paint.span(resolved, theme.muted),
    ]));
    lines
}

/// Inline code keeps its color but not the selection background, which in select lines
/// means a marked range.
fn off_selection(mut span: Span<'static>, theme: &Theme) -> Span<'static> {
    if span.style.bg == Some(theme.selection) {
        span.style.bg = None;
    }
    span
}

/// A comment's first line as the Overview shows it, without its Markdown marks.
fn first_rendered_line(body: &str, width: usize, theme: &Theme) -> String {
    let toggled = std::collections::HashSet::new();
    let mut folds = markdown::Folds::new(&toggled);
    let rows = markdown::layout(&markdown::parse(body, theme), width, &mut folds, theme);
    rows.iter()
        .map(|row| {
            row.line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .map(|text| text.trim().to_string())
        .find(|text| !text.is_empty())
        .unwrap_or_default()
}

/// The columns before a row's code: the mark, both line numbers, and the +/- sign.
const SELECT_GUTTER: usize = 14;

/// One diff row in select lines. Wrapped, the rest of the code goes on rows indented
/// under it; unwrapped, the pane clips it.
fn select_row(
    row: &DiffRow,
    mark: Span<'static>,
    width: usize,
    wrap: bool,
    paint: Paint,
) -> Vec<Line<'static>> {
    let theme = paint.theme;
    let number = |line: Option<u64>| line.map_or("     ".to_string(), |line| format!("{line:>5}"));
    let (sign, color) = match row.kind {
        RowKind::Added => ("+", theme.success),
        RowKind::Removed => ("-", theme.danger),
        RowKind::Context => (" ", theme.foreground),
        RowKind::Hunk | RowKind::Note => ("", theme.heading),
    };
    let pieces = match wrap {
        true => split_to_width(&row.text, width.saturating_sub(SELECT_GUTTER)),
        false => vec![row.text.clone()],
    };
    let mut pieces = pieces.into_iter();
    let first = Line::from(vec![
        mark,
        paint.span(
            format!("{} {} ", number(row.old), number(row.new)),
            theme.muted,
        ),
        paint.span(
            format!("{sign}{}", pieces.next().unwrap_or_default()),
            color,
        ),
    ]);
    let indent = " ".repeat(SELECT_GUTTER);
    let rest = pieces.map(|piece| Line::from(paint.span(format!("{indent}{piece}"), color)));
    std::iter::once(first).chain(rest).collect()
}

/// Cuts code into pieces no wider than `width`, after the last space that fits, or at
/// the width when there is none. Every space stays: one at a break hangs past the edge,
/// where the pane clips it. A wide character that would cross the edge starts the next
/// piece.
fn split_to_width(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut pieces = Vec::new();
    let mut current = String::new();
    for character in text.chars() {
        let overflows = display_width(&current) + character.width().unwrap_or(0) > width;
        if overflows && character != ' ' && !current.is_empty() {
            let indent = current.len() - current.trim_start().len();
            let rest = match current[indent..].rfind(' ') {
                Some(at) => current.split_off(indent + at + 1),
                None => String::new(),
            };
            pieces.push(std::mem::replace(&mut current, rest));
        }
        current.push(character);
    }
    pieces.push(current);
    pieces
}

/// The cursor's row in select lines, in screen rows: its block, threads included, and
/// where its code starts.
struct Followed {
    block: usize,
    code: usize,
    end: usize,
}

impl Followed {
    /// The scroll that shows the whole block with the least move from `scroll`. A block
    /// taller than the pane starts at its code, so the cursor's line shows.
    fn scroll(&self, scroll: usize, height: usize) -> usize {
        let start = match self.end - self.block > height {
            true => self.code,
            false => self.block,
        };
        if start < scroll {
            return start;
        }
        scroll.max(self.end.saturating_sub(height)).min(start)
    }
}

/// " /query  2 of 5 lines ", at the right of the diff pane's title, so the find text and
/// the place among its matches stay in view. In another file it says n finds there.
/// "/text  repo match 3 of 155", over the file a repo match opened.
fn repo_place(view: &DetailView, paint: Paint) -> (Vec<Piece>, Vec<Piece>) {
    let muted = paint.theme.muted;
    let piece = |text: String, rank| Piece::new(paint.span(text, muted), rank);
    // The name, the long and short words for what is walked, its place, and a count.
    let (name, long, short, at, total, count) = if let Some(walk) = &view.def_walk {
        let more = walk.capped.then(|| ", more files not read".to_string());
        let total = walk.hits.len().to_string();
        (
            format!(" {} ", walk.name),
            "definition",
            "def",
            walk.at + 1,
            total,
            more,
        )
    } else {
        let (Some(query), Some(hit), Some(Ok(hits))) = (&view.find, view.repo_hit, &view.find_repo)
        else {
            return (Vec::new(), Vec::new());
        };
        let total = repo_count(view, hits.len());
        let in_diff = view
            .diff_match_count()
            .filter(|count| *count > 0)
            .map(|count| format!(", {count} in the diff"));
        (
            format!(" /{query} "),
            "repo match",
            "repo",
            hit + 1,
            total,
            in_diff,
        )
    };
    let place = paint.span(format!(" {long} {at} of {total}"), muted);
    let mut pieces = vec![
        Piece::new(paint.bold(name.clone(), paint.theme.warning), RANK_FIND),
        place_piece(
            place,
            Some(format!(" {short} {at}/{total}")),
            Some(format!(" {at}/{total}")),
        ),
    ];
    pieces.extend(count.map(|count| piece(count, RANK_COUNT)));
    pieces.push(piece("  n/N next/previous ".to_string(), RANK_HINT));
    // A definition's own name says nothing more; "Foo > bar" names its class.
    let block = view.file_view_place().filter(|place| *place != name.trim());
    let below = block
        .map(|place| block_piece(&place, paint))
        .into_iter()
        .collect();
    (pieces, below)
}

/// One piece of a pane's title row, with the steps it gives way by when the row is
/// too narrow: a shorter form, a shrink, or a drop. Each step has a rank, and the
/// row takes the lowest-ranked next step of any piece first.
struct Piece {
    span: Span<'static>,
    steps: Vec<(u8, GiveWay)>,
}

enum GiveWay {
    Form(String),
    Shrink(Shrink),
    Drop,
}

/// How a path or a block name shrinks: whole parts go from its start, so
/// "src/app/main.rs" becomes "…/app/main.rs" and "A > b > c" becomes "… > b > c".
struct Shrink {
    lead: String,
    body: String,
    tail: String,
    separator: &'static str,
}

impl Shrink {
    /// The text in `room` columns, or None below the floor.
    fn to(&self, room: usize) -> Option<String> {
        let edges = display_width(&self.lead) + display_width(&self.tail);
        if room < SHRINK_FLOOR || room <= edges {
            return None;
        }
        let body_room = room - edges;
        let parts: Vec<&str> = self.body.split(self.separator).collect();
        let fits = (1..parts.len())
            .map(|first| format!("…{}{}", self.separator, parts[first..].join(self.separator)))
            .find(|body| display_width(body) <= body_room);
        let body = fits.unwrap_or_else(|| truncate_start(&self.body, body_room));
        Some(format!("{}{body}{}", self.lead, self.tail))
    }
}

impl Piece {
    /// A piece that drops whole at `rank`.
    fn new(span: Span<'static>, rank: u8) -> Self {
        Piece {
            span,
            steps: vec![(rank, GiveWay::Drop)],
        }
    }

    /// A piece that takes the shorter forms at their ranks, then drops at `drop`.
    fn with_forms(span: Span<'static>, forms: Vec<(u8, String)>, drop: u8) -> Self {
        let mut steps: Vec<(u8, GiveWay)> = forms
            .into_iter()
            .map(|(rank, text)| (rank, GiveWay::Form(text)))
            .collect();
        steps.push((drop, GiveWay::Drop));
        Piece { span, steps }
    }

    /// `lead`, `body` and `tail` in one style; the body shrinks at `separator` at the
    /// rank `shrink`, and the piece drops at `drop`.
    fn shrinking(
        style: Style,
        [lead, body, tail]: [&str; 3],
        separator: &'static str,
        [shrink, drop]: [u8; 2],
    ) -> Self {
        let parts = Shrink {
            lead: lead.to_string(),
            body: body.to_string(),
            tail: tail.to_string(),
            separator,
        };
        Piece {
            span: Span::styled(format!("{lead}{body}{tail}"), style),
            steps: vec![(shrink, GiveWay::Shrink(parts)), (drop, GiveWay::Drop)],
        }
    }

    fn width(&self) -> usize {
        display_width(&self.span.content)
    }
}

// The order titles give way in, from first to last.
const RANK_HINT: u8 = 1;
const RANK_WRAP: u8 = 2;
const RANK_COUNT: u8 = 3;
const RANK_BLOCK: u8 = 4;
const RANK_THREADS: u8 = 5;
const RANK_PLACE_SHORT: u8 = 5;
const RANK_PATH_SHRINK: u8 = 6;
const RANK_PLACE_SHORTEST: u8 = 7;
const RANK_FIND: u8 = 8;
const RANK_PATH: u8 = 9;
const RANK_VIEW: u8 = 10;
/// A path or block name shrinks no narrower than this, then drops.
const SHRINK_FLOOR: usize = 16;

/// "in BaseApp > on_mount", which keeps its innermost names when it shrinks.
fn block_piece(place: &str, paint: Paint) -> Piece {
    let style = paint.span("", paint.theme.heading).style;
    Piece::shrinking(
        style,
        ["  in ", place, " "],
        " > ",
        [RANK_BLOCK, RANK_BLOCK],
    )
}

/// A file's path, which keeps its file name and nearest folders when it shrinks.
fn path_piece(style: Style, parts: [&str; 3]) -> Piece {
    Piece::shrinking(style, parts, "/", [RANK_PATH_SHRINK, RANK_PATH])
}

/// "match 41 of 50 in file 2 of 11", then "41/50 in file 2/11", then "41/50".
fn place_piece(span: Span<'static>, short: Option<String>, shortest: Option<String>) -> Piece {
    let mut forms = Vec::new();
    forms.extend(short.map(|text| (RANK_PLACE_SHORT, text)));
    forms.extend(shortest.map(|text| (RANK_PLACE_SHORTEST, text)));
    Piece::with_forms(span, forms, RANK_FIND)
}

/// A pane's title. It takes one row when that holds every piece but the key hints,
/// with `below` at the right end. Otherwise the first row keeps the view and the path,
/// and `right` and `below` move to a second row under it, so neither row is crowded.
struct TitleRows {
    first: Line<'static>,
    second: Option<Line<'static>>,
}

impl TitleRows {
    fn new(width: usize, left: Vec<Piece>, mut right: Vec<Piece>, below: Vec<Piece>) -> Self {
        let needed: usize = left
            .iter()
            .chain(&right)
            .chain(&below)
            .filter(|piece| piece.steps.first().is_some_and(|step| step.0 > RANK_WRAP))
            .map(Piece::width)
            .sum();
        if below.is_empty() || needed <= width {
            right.extend(below);
            return TitleRows {
                first: fit_title(width, left, right),
                second: None,
            };
        }
        right.extend(below);
        TitleRows {
            first: fit_title(width, left, Vec::new()),
            second: Some(fit_title(width, right, Vec::new())),
        }
    }

    /// Draws the second row, if there is one, at the top of `inner`, and returns the
    /// room left under it.
    fn draw_second(self, frame: &mut Frame, inner: Rect) -> Rect {
        let Some(second) = self.second else {
            return inner;
        };
        let row = Rect {
            height: inner.height.min(1),
            ..inner
        };
        frame.render_widget(Paragraph::new(second), row);
        Rect {
            y: inner.y + row.height,
            height: inner.height - row.height,
            ..inner
        }
    }
}

/// One title row `width` wide: the left pieces in order, then the right ones against
/// the right edge, after the pieces gave way as far as the row needs.
fn fit_title(width: usize, left: Vec<Piece>, right: Vec<Piece>) -> Line<'static> {
    let split = left.len();
    let mut pieces: Vec<Option<Piece>> = left.into_iter().chain(right).map(Some).collect();
    loop {
        let used: usize = pieces.iter().flatten().map(Piece::width).sum();
        if used <= width {
            break;
        }
        // The lowest-ranked next step goes first; of equal ranks, the later piece's.
        let next = pieces
            .iter()
            .enumerate()
            .filter_map(|(index, piece)| Some((piece.as_ref()?.steps.first()?.0, index)))
            .min_by_key(|(rank, index)| (*rank, usize::MAX - index));
        let Some((_, index)) = next else {
            break;
        };
        let Some(piece) = pieces[index].as_mut() else {
            break;
        };
        let room = piece.width().saturating_sub(used - width);
        let style = piece.span.style;
        match piece.steps.remove(0).1 {
            GiveWay::Form(text) => piece.span = Span::styled(text, style),
            GiveWay::Shrink(shrink) => {
                if let Some(text) = shrink.to(room) {
                    piece.span = Span::styled(text, style);
                }
            }
            GiveWay::Drop => pieces[index] = None,
        }
    }
    let used: usize = pieces.iter().flatten().map(Piece::width).sum();
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (index, piece) in pieces.into_iter().enumerate() {
        if index == split {
            spans.push(Span::raw(" ".repeat(width.saturating_sub(used))));
        }
        spans.extend(piece.map(|piece| piece.span));
    }
    Line::from(spans)
}

/// The find a repo match's file view marks, and the index of the match's line.
struct RepoFinding {
    pattern: crate::find::Pattern,
    current: Option<usize>,
}

impl RepoFinding {
    /// Marks the find text as the diff pane does; the match's own line gets the
    /// stronger mark.
    fn mark(&self, index: usize, line: &Line<'static>, paint: Paint) -> Line<'static> {
        if !self.pattern.matches_drawn(line) {
            return line.clone();
        }
        let theme = paint.theme;
        let mark = match Some(index) == self.current {
            true => Style::new().fg(theme.background).bg(theme.warning),
            false => Style::new().fg(theme.warning).bg(theme.selection),
        };
        mark_matches(line.clone(), &self.pattern.text, mark)
    }
}

fn repo_finding(view: &DetailView) -> Option<RepoFinding> {
    let (Some(query), Some(hit), Some(Ok(hits))) = (&view.find, view.repo_hit, &view.find_repo)
    else {
        return None;
    };
    Some(RepoFinding {
        pattern: crate::find::Pattern::parse(query),
        current: hits.get(hit).map(|hit| hit.line.saturating_sub(1)),
    })
}

/// "155", or "980+" when git stopped at its limit.
fn repo_count(view: &DetailView, count: usize) -> String {
    count_text(count, view.find_repo_capped)
}

fn count_text(count: usize, capped: bool) -> String {
    match capped {
        true => format!("{count}+"),
        false => count.to_string(),
    }
}

/// The footer when no changed file holds the text: what n does instead. `repo` is the
/// number of repo matches, None while git looks, or Err when the search failed.
fn not_in_diff_note(repo: Option<Result<usize, ()>>, capped: bool) -> String {
    match repo {
        None => "not in the diff, looking in the repo...".to_string(),
        Some(Err(())) => "not in the diff; repo not searched".to_string(),
        Some(Ok(0)) => "not in the diff or the repo".to_string(),
        Some(Ok(count)) => format!(
            "not in the diff; {} in the repo, n opens the first",
            count_text(count, capped)
        ),
    }
}

/// What n reaches after the diff's matches: ", 155 more in the repo" once git returns.
fn repo_note(view: &DetailView) -> String {
    match &view.find_repo {
        None => ", looking in the repo...".to_string(),
        Some(Err(_)) => ", repo not searched".to_string(),
        Some(Ok(hits)) if hits.is_empty() => String::new(),
        Some(Ok(hits)) => format!(", {} in the repo", repo_count(view, hits.len())),
    }
}

fn find_note(view: &DetailView, paint: Paint) -> Vec<Piece> {
    let Some(query) = &view.find else {
        return Vec::new();
    };
    let hit = view.current_find_hit();
    let here = match &hit {
        Some(hit) => format!("match {} of {}", hit.ordinal, hit.total),
        None => "no match".to_string(),
    };
    let fraction = hit.map(|hit| format!(" {}/{}", hit.ordinal, hit.total));
    let repo = view
        .find_repo
        .as_ref()
        .map(|result| result.as_ref().map(Vec::len).map_err(|_| ()));
    let place = match (view.find_file_place(), &view.find_files, view.tab) {
        (_, _, tab) if tab != Tab::Files => format!("{here} in this commit"),
        (Some((None, 0)), _, _) => not_in_diff_note(repo, view.find_repo_capped),
        (Some((Some(at), files)), _, _) => format!("{here} in file {at} of {files}"),
        (Some((None, files)), _, _) => {
            let have = match files {
                1 => "1 file has it".to_string(),
                count => format!("{count} files have it"),
            };
            format!("not in this file; {have}, n goes to the next")
        }
        (None, None, _) => format!("{here} in this file, looking in the others..."),
        (None, _, _) => format!("{here} in this file, other files not searched"),
    };
    let short = match (view.find_file_place(), &fraction, view.tab) {
        (Some((Some(at), files)), Some(fraction), Tab::Files) => {
            Some(format!("{fraction} in file {at}/{files}"))
        }
        _ => None,
    };
    let repo = match (view.tab, view.find_file_place()) {
        (_, Some((None, 0))) => String::new(),
        (Tab::Files, _) => repo_note(view),
        _ => String::new(),
    };
    let muted = paint.theme.muted;
    let mut pieces = vec![
        Piece::new(
            paint.bold(format!(" /{query} "), paint.theme.warning),
            RANK_FIND,
        ),
        place_piece(paint.span(format!(" {place}"), muted), short, fraction),
    ];
    if !repo.is_empty() {
        pieces.push(Piece::new(paint.span(repo, muted), RANK_COUNT));
    }
    pieces.push(Piece::new(
        paint.span("  n/N next/previous ", muted),
        RANK_HINT,
    ));
    pieces
}

/// Marks every place the find text appears in a line, ignoring case. The line of the
/// current match gets the stronger mark.
pub fn mark_matches(line: Line<'static>, query: &str, mark: Style) -> Line<'static> {
    // The line's own style carries a code row's background, which the pane fills.
    let line_style = line.style;
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
    Line::from(spans).style(line_style)
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
/// The selected file's review threads for the diff pane's title, on Files changed.
fn selected_thread_note(view: &DetailView) -> String {
    let (Some(Ok(activity)), Some(files), Some(index)) =
        (&view.activity, view.loaded_files(), view.selected_file())
    else {
        return String::new();
    };
    match files.get(index) {
        Some(file) if view.tab == Tab::Files => thread_note(activity, &file.path),
        _ => String::new(),
    }
}

/// " 4 threads (3 outdated), S to read ", or nothing when the file has none. Line
/// numbers would not help: most renderers draw none, and an outdated thread's line is
/// gone from the diff.
fn thread_note(activity: &PrActivity, path: &str) -> String {
    let (total, outdated) = activity
        .threads_on(path)
        .fold((0, 0), |(total, outdated), thread| {
            (total + 1, outdated + usize::from(thread.at.is_none()))
        });
    let noun = if total == 1 { "thread" } else { "threads" };
    match (total, outdated) {
        (0, _) => String::new(),
        (_, 0) => format!(" {total} {noun}, S to read "),
        _ if outdated == total => format!(" {total} outdated {noun}, S to read "),
        _ => format!(" {total} {noun} ({outdated} outdated), S to read "),
    }
}

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
    activity: Option<&PrActivity>,
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
            let used = 1 + depth * 2 + 2;
            let label = truncate_to_width(&format!("{label}/"), width.saturating_sub(used));
            Line::from(vec![
                Span::raw(format!(" {}", "  ".repeat(*depth))),
                paint.span(format!("{chevron} "), theme.fold),
                paint.span(label, theme.accent),
            ])
        }
        FileRow::File { index, depth } => {
            let indent = "  ".repeat(*depth);
            let file = &files[*index];
            let threads = activity.map_or(0, |activity| activity.threads_on(&file.path).count());
            let mut line = file_line(
                file,
                tree,
                threads,
                width.saturating_sub(indent.len()),
                paint,
            );
            line.spans.insert(0, Span::raw(indent));
            line
        }
    }
}

/// `threads` is the number of review threads on the file, shown as ●N before the counts.
fn file_line(
    file: &ChangedFile,
    tree: bool,
    threads: usize,
    width: usize,
    paint: Paint,
) -> Line<'static> {
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
    let mark = match threads {
        0 => String::new(),
        count => format!(" ●{count}"),
    };
    let counts = format!(" +{} -{}", file.additions, file.deletions);
    let path_width = width.saturating_sub(display_width(&counts) + display_width(&mark) + 4);
    let name_width = display_width(&name).min(path_width);
    let directory = truncate_to_width(&directory, path_width - name_width);
    let padding = path_width.saturating_sub(display_width(&directory) + name_width);
    Line::from(vec![
        paint.span(format!(" {marker} "), marker_color),
        paint.span(directory, theme.muted),
        paint.bold(truncate_to_width(&name, name_width), theme.foreground),
        Span::from(" ".repeat(padding)),
        paint.span(mark, theme.accent),
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

/// The header from the inbox row, drawn at once while the detail loads: the same lines
/// as `detail_header` without what only the detail knows (the branches, the size).
fn summary_header(pr: &PrSummary, width: usize, paint: Paint) -> Vec<Line<'static>> {
    let theme = paint.theme;
    let mut lines = vec![Line::from(vec![
        paint.span(format!(" {} ", pr.repo), theme.muted),
        paint.span(format!("#{}", pr.number), theme.muted),
    ])];
    let title = [paint.bold(format!(" {}", pr.title), theme.foreground)];
    lines.extend(markdown::wrap(&title, width, 1));
    let (state, state_color) = match pr.is_draft {
        true => (" Draft ", PrState::Draft),
        false => (" Open ", PrState::Open),
    };
    let pill = Style::new()
        .fg(ON_STATE)
        .bg(theme.github_state(state_color))
        .add_modifier(Modifier::BOLD);
    lines.push(Line::from(vec![
        Span::raw(" "),
        Span::styled(state, pill),
        paint.bold(format!(" {}", pr.author), theme.foreground),
    ]));
    lines.push(Line::default());
    lines
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
    // Long branch names get cut so the line stays on one row; the base keeps up to a
    // third of the room, the head the rest.
    let stats = format!(
        "   +{} -{}, {} files",
        detail.additions, detail.deletions, detail.changed_files
    );
    let fixed =
        1 + state.len() + 1 + display_width(&pr.author) + verb.len() + 2 + 6 + 2 + stats.len();
    let room = width.saturating_sub(fixed);
    let base = truncate_to_width(
        &detail.base,
        (room / 3).max(display_width(&detail.base).min(room / 2)),
    );
    let head = truncate_to_width(
        &detail.head,
        room.saturating_sub(display_width(&base)).max(8),
    );
    let merge = [
        Span::raw(" "),
        Span::styled(state, pill),
        paint.bold(format!(" {}", pr.author), theme.foreground),
        paint.span(verb, theme.muted),
        Span::styled(format!(" {base} "), branch),
        paint.span(" from ", theme.muted),
        Span::styled(format!(" {head} "), branch),
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
/// It takes what the main column does not need past a comfortable reading width.
const SIDEBAR_MIN: u16 = 32;
const SIDEBAR_MAX: u16 = 44;
const SIDEBAR_MIN_TOTAL: u16 = 110;
const READING_WIDTH: u16 = 104;

fn render_overview(frame: &mut Frame, view: &mut DetailView, area: Rect, paint: Paint) {
    let sidebar_width = match area.width {
        width if width >= SIDEBAR_MIN_TOTAL => width
            .saturating_sub(READING_WIDTH)
            .clamp(SIDEBAR_MIN, SIDEBAR_MAX),
        _ => 0,
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
            images: overview::images(&context),
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
                // The mark and the hash take 12 columns.
                paint.bold(
                    truncate_to_width(&commit.headline, usize::from(area.width).saturating_sub(12)),
                    theme.foreground,
                ),
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
        _ if check.state == CheckState::Skipped => Some("skipped".to_string()),
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
    let (title, hint) = match &prompt.kind {
        PromptKind::Definition { names } => (
            " Definition of ".to_string(),
            format!(
                "on this line: {}. tab completes; enter looks it up at the PR head",
                names.join(", ")
            ),
        ),
        PromptKind::OpenLink => (
            " Open a pull request ".to_string(),
            "paste a PR link or owner/repo#123, enter to open, esc to cancel".to_string(),
        ),
        PromptKind::Workspace => (
            " Workspace folder ".to_string(),
            "the folders that hold your clones, with commas between. new clones go in the first. enter saves, esc cancels"
                .to_string(),
        ),
        PromptKind::Editor { .. } => (
            " Editor command ".to_string(),
            "such as myeditor {file}:{line}. {file} is the file, {line} the line, {dir} the clone. empty uses your shell's editor. enter saves, esc keeps it"
                .to_string(),
        ),
        PromptKind::UpdatedWithin => (
            " Updated within, in days ".to_string(),
            "PRs updated in this many days, or empty for no limit. enter saves and reloads, esc keeps it"
                .to_string(),
        ),
        PromptKind::FindInDiff => (
            " Find in the diff ".to_string(),
            "tab completes a word from the diff. enter finds it in every changed file, then n goes on through the rest of the repo; N goes back"
                .to_string(),
        ),
        PromptKind::ConfirmDiscard { .. } => (
            " Discard your pending review? ".to_string(),
            "y and enter deletes it and every line comment in it on GitHub; esc keeps it"
                .to_string(),
        ),
        PromptKind::ConfirmMerge {
            request,
            base,
            bypass,
            ..
        } => merge_question(*request, base, *bypass),
        PromptKind::ConfirmClone { pr, dir } => (
            format!(" Clone {}? ", pr.repo),
            format!(
                "no clone in the workspace. y and enter clones it into {}",
                dir.display()
            ),
        ),
    };
    let (hint, color) = match &prompt.error {
        Some(error) => (error.clone(), theme.danger),
        None => (hint, theme.muted),
    };
    // The hint wraps under the input, and the box grows to hold it.
    let hint_rows = split_to_width(&hint, usize::from(width.saturating_sub(2)));
    let height = u16::try_from(hint_rows.len() + 3).unwrap_or(u16::MAX);
    let box_area = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + 2,
        width,
        height: height.min(area.height.saturating_sub(2)),
    };
    let mut lines = vec![Line::from(vec![
        paint.span("> ", theme.accent),
        Span::from(prompt.text.clone()),
        paint.span("▏", theme.accent),
        paint.span(prompt.suggestion().unwrap_or_default(), theme.muted),
    ])];
    lines.extend(
        hint_rows
            .into_iter()
            .map(|row| Line::from(paint.span(row, color))),
    );
    let text = Text::from(lines);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
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
        lines.extend(reviewer_lines(&detail.reviewers, width, paint));
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
        "Activity so far"
    };
    let mut lines = section(title, paint);
    let when = match since.since {
        Some(at) => format!("you last commented {}", relative_time(at, paint.now)),
        None => "counted since the PR opened".to_string(),
    };
    lines.push(Line::from(paint.span(when, theme.muted)));
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
fn reviewer_lines(reviewers: &[Reviewer], width: usize, paint: Paint) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for reviewer in reviewers.iter().filter(|reviewer| reviewer.team.is_none()) {
        lines.push(reviewer_line(reviewer, "", width, paint));
        let members = reviewers
            .iter()
            .filter(|member| member.team.as_ref() == Some(&reviewer.name));
        lines.extend(members.map(|member| reviewer_line(member, "└ ", width, paint)));
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
        lines.extend(members.map(|member| reviewer_line(member, "└ ", width, paint)));
    }
    lines
}

/// The name gives way to the state when the sidebar is narrow, since the state is
/// what the line is for.
fn reviewer_line(reviewer: &Reviewer, indent: &str, width: usize, paint: Paint) -> Line<'static> {
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
    let used = display_width(indent) + 2 + display_width(&note);
    let name = truncate_to_width(&reviewer.name, width.saturating_sub(used).max(6));
    Line::from(vec![
        paint.span(format!("{indent}{symbol} "), color),
        paint.span(name, theme.foreground),
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
/// The label checklist: a filter line, then each label with its tick and color.
fn render_label_menu(frame: &mut Frame, menu: &LabelMenu, paint: Paint) {
    let theme = paint.theme;
    let area = frame.area();
    let width = 50.min(area.width);
    let height = (area.height * 3 / 5).max(8).min(area.height);
    let box_area = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    let mut lines = vec![Line::from(vec![
        paint.span("> ", theme.accent),
        Span::from(menu.filter.clone()),
        paint.span("▏", theme.accent),
    ])];
    match &menu.labels {
        None => lines.push(Line::from(
            paint.span(" loading the repo's labels...", theme.warning),
        )),
        Some(Err(error)) => lines.push(Line::from(paint.span(format!(" {error}"), theme.danger))),
        Some(Ok(_)) => {
            let shown = menu.shown();
            let visible = usize::from(height.saturating_sub(3)).max(1);
            let first = (menu.selected + 1).saturating_sub(visible);
            if shown.is_empty() {
                lines.push(Line::from(
                    paint.span(" no label holds that text", theme.muted),
                ));
            }
            for (index, label) in shown.iter().enumerate().skip(first).take(visible) {
                let tick = match menu.chosen.contains(&label.name) {
                    true => "[x] ",
                    false => "[ ] ",
                };
                let (color, _) = markdown::label_colors(&label.color);
                let line = Line::from(vec![
                    paint.span(format!(" {tick}"), theme.foreground),
                    Span::styled("● ", Style::new().fg(color)),
                    paint.span(label.name.clone(), theme.foreground),
                ]);
                lines.push(match index == menu.selected {
                    true => line.style(Style::new().bg(theme.selection)),
                    false => line,
                });
            }
        }
    }
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.heading))
        .title(paint.bold(" Labels ", theme.heading))
        .title_bottom(paint.span(
            " type to filter, up/down move, space ticks, enter applies, esc ",
            theme.muted,
        ))
        .style(Style::new().bg(theme.background).fg(theme.foreground));
    frame.render_widget(Clear, box_area);
    frame.render_widget(Paragraph::new(lines).block(block), box_area);
}

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
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.heading))
        .title(paint.bold(" Settings ", theme.heading))
        .title_bottom(paint.span(" j/k move, enter edit, esc ", theme.muted))
        .style(Style::new().bg(theme.background).fg(theme.foreground));
    frame.render_widget(Clear, box_area);
    frame.render_widget(Paragraph::new(lines).block(block), box_area);
}

/// The confirm box's title and hint for a merge write.
fn merge_question(request: MergeRequest, base: &str, bypass: bool) -> (String, String) {
    let merge = |method: MergeMethod| match method {
        MergeMethod::Merge => format!("merge into {base} with a merge commit"),
        MergeMethod::Squash => format!("squash and merge into {base}"),
        MergeMethod::Rebase => format!("rebase and merge into {base}"),
    };
    match request {
        MergeRequest::Now(method) if bypass => (
            " Bypass requirements and merge? ".to_string(),
            format!(
                "y and enter: {} now, past the branch's requirements. esc cancels",
                merge(method)
            ),
        ),
        MergeRequest::Now(method) => (
            format!(" {}? ", capitalize(&merge(method))),
            "y and enter merges it on GitHub; esc cancels".to_string(),
        ),
        MergeRequest::Auto(method) => (
            " Enable auto-merge? ".to_string(),
            format!(
                "y and enter: GitHub will {} once the requirements pass. esc cancels",
                merge(method)
            ),
        ),
        MergeRequest::DisableAuto => (
            " Disable auto-merge? ".to_string(),
            "y and enter turns it off; the PR stays open. esc keeps it on".to_string(),
        ),
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// GitHub's line under a merge method, with the PR's commit count.
fn merge_method_note(method: MergeMethod, commits: u64) -> String {
    let these = match commits {
        1 => "The 1 commit".to_string(),
        count => format!("The {count} commits"),
    };
    match method {
        MergeMethod::Merge => {
            "All commits from this branch will be added to the base branch via a merge commit."
                .to_string()
        }
        MergeMethod::Squash if commits == 1 => {
            format!("{these} from this branch will be added to the base branch.")
        }
        MergeMethod::Squash => {
            format!("{these} from this branch will be combined into one commit in the base branch.")
        }
        MergeMethod::Rebase => {
            format!("{these} from this branch will be rebased and added to the base branch.")
        }
    }
}

/// The merge menu's lines: each row's title and GitHub's note under it.
fn merge_menu_lines(menu: &MergeMenu, paint: Paint) -> Vec<Line<'static>> {
    let theme = paint.theme;
    let mut lines = Vec::new();
    for (index, item) in menu.items.iter().enumerate() {
        let (mark, title, note, color) = match *item {
            MergeItem::Method(method) => {
                let mark = if menu.default_method == Some(method) {
                    "✓"
                } else {
                    " "
                };
                let note = merge_method_note(method, menu.commit_count);
                (mark, method.title(), note, theme.foreground)
            }
            MergeItem::Bypass => {
                lines.push(Line::from(""));
                let mark = if menu.bypass { "✓" } else { " " };
                let note =
                    "Merge now with the selected method, without waiting for the requirements.";
                (mark, "Bypass requirements", note.to_string(), theme.danger)
            }
            MergeItem::DisableAuto => {
                let note = "GitHub stops waiting to merge it. The PR stays open.";
                (
                    " ",
                    "Disable auto-merge",
                    note.to_string(),
                    theme.foreground,
                )
            }
        };
        let row = vec![
            Line::from(vec![
                paint.span(format!(" {mark} "), theme.success),
                paint.bold(title.to_string(), color),
            ]),
            Line::from(paint.span(format!("   {note}"), theme.muted)),
        ];
        let selected = index == menu.selected;
        lines.extend(row.into_iter().map(|line| match selected {
            true => line.style(Style::new().bg(theme.selection)),
            false => line,
        }));
    }
    if let Some(note) = menu.note {
        lines.push(Line::from(""));
        lines.push(Line::from(paint.span(format!(" {note}"), theme.warning)));
    }
    lines
}

/// The merge menu: GitHub's merge button and its dropdown.
fn render_merge_menu(frame: &mut Frame, menu: &MergeMenu, paint: Paint) {
    let lines = merge_menu_lines(menu, paint);
    let title = match (menu.items.first(), menu.ready, menu.bypass) {
        (Some(MergeItem::DisableAuto), ..) => " Auto-merge is on ".to_string(),
        (_, _, true) => " Bypass requirements and merge ".to_string(),
        (_, true, _) => format!(" Merge into {} ", menu.base),
        (_, false, _) if menu.can_auto_merge => " Enable auto-merge ".to_string(),
        _ => " Merging is blocked ".to_string(),
    };
    render_menu_box(frame, title, lines, paint);
}

/// o's choice: the file's GitHub page, or the clone's copy in the editor.
fn render_open_menu(frame: &mut Frame, menu: &OpenMenu, paint: Paint) {
    let theme = paint.theme;
    let editor_note = match &menu.editor {
        Ok(target) => {
            let at = match target.line {
                Some(line) => format!("line {line}"),
                None => "its first change".to_string(),
            };
            let mut spans = vec![paint.span(
                format!("   {} on your clone's copy, at {at}", menu.editor_name),
                theme.muted,
            )];
            if let Some(elsewhere) = &target.elsewhere {
                spans.push(paint.span(
                    format!(". The clone is on {elsewhere}, not the PR's head"),
                    theme.warning,
                ));
            }
            Line::from(spans)
        }
        Err(why) => Line::from(paint.span(format!("   {why}"), theme.danger)),
    };
    let items = [
        ("Editor", editor_note),
        (
            "GitHub",
            Line::from(paint.span(format!("   {}", menu.github_note), theme.muted)),
        ),
    ];
    let mut lines = Vec::new();
    for (index, (title, note)) in items.into_iter().enumerate() {
        let row = [
            Line::from(paint.bold(format!(" {title}"), theme.foreground)),
            note,
        ];
        let selected = index == menu.selected;
        lines.extend(row.into_iter().map(|line| match selected {
            true => line.style(Style::new().bg(theme.selection)),
            false => line,
        }));
    }
    let name = menu.file.rsplit('/').next().unwrap_or(&menu.file);
    render_menu_box(frame, format!(" Open {name} "), lines, paint);
}

/// A centered box as wide as its longest line, so notes wrap only on a narrow terminal.
fn render_menu_box(frame: &mut Frame, title: String, lines: Vec<Line<'static>>, paint: Paint) {
    let theme = paint.theme;
    let area = frame.area();
    let longest = lines.iter().map(Line::width).max().unwrap_or(0) + 1;
    let width = (longest as u16 + 2).min(area.width);
    let inner = usize::from(width.saturating_sub(2)).max(1);
    let rows: usize = lines
        .iter()
        .map(|line| line.width().div_ceil(inner).max(1))
        .sum();
    let height = (rows as u16 + 2).min(area.height);
    let box_area = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.heading))
        .title(paint.bold(title, theme.heading))
        .title_bottom(paint.span(" j/k move, enter picks, esc ", theme.muted))
        .style(Style::new().bg(theme.background).fg(theme.foreground));
    frame.render_widget(Clear, box_area);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(ratatui::widgets::Wrap { trim: false }),
        box_area,
    );
}

/// The editor list from settings, drawn like the theme list.
fn render_editor_menu(frame: &mut Frame, menu: &EditorMenu, paint: Paint) {
    let theme = paint.theme;
    let mut lines: Vec<Line> = menu
        .choices
        .iter()
        .enumerate()
        .map(|(index, choice)| {
            let name = match choice {
                EditorChoice::Shell => format!("From your shell: {}", menu.shell),
                EditorChoice::Known { label, .. } => label.to_string(),
                EditorChoice::Custom => "Custom command…".to_string(),
            };
            let mark = if index == menu.current { "●" } else { " " };
            let line = Line::from(paint.span(format!("  {mark} {name} "), theme.foreground));
            match index == menu.selected {
                true => line.style(Style::new().bg(theme.selection)),
                false => line,
            }
        })
        .collect();
    // Asked from o the first time: say the pick is kept, and where to change it.
    let title = match &menu.open {
        Some(target) => {
            lines.push(Line::default());
            lines.push(Line::from(
                paint.span("  revu remembers this. , changes it", theme.muted),
            ));
            let name = target.path.rsplit('/').next().unwrap_or(&target.path);
            format!(" Open {name} with ")
        }
        None => " Editor ".to_string(),
    };
    let area = frame.area();
    let width = 40.min(area.width);
    let height = (lines.len() as u16 + 2).min(area.height);
    let box_area = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.heading))
        .title(paint.bold(title, theme.heading))
        .title_bottom(paint.span(" j/k move, enter picks, esc ", theme.muted))
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
        .border_type(BorderType::Rounded)
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
    let text = format!(" API {} left, resets {reset} ", limit.remaining);
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

/// Cuts the start instead of the end, so a long path keeps its file name.
pub fn truncate_start(text: &str, width: usize) -> String {
    if display_width(text) <= width {
        return text.to_string();
    }
    let mut kept: Vec<char> = Vec::new();
    let mut used = 1;
    for character in text.chars().rev() {
        used += character.width().unwrap_or(0);
        if used > width {
            break;
        }
        kept.push(character);
    }
    std::iter::once('…').chain(kept.into_iter().rev()).collect()
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

    #[test]
    fn a_code_row_fills_its_background_to_the_edge_on_every_row_it_takes() {
        let gray = Style::new().bg(Color::Rgb(40, 40, 40));
        let lines = vec![
            Line::from("short").style(gray),
            Line::from("a line long enough to wrap").style(gray),
            Line::from("").style(gray),
            Line::from("header"),
        ];
        let area = Rect::new(0, 0, 12, 8);
        let backend = ratatui::backend::TestBackend::new(12, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_diff_lines(frame, lines, area, None))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let filled: Vec<bool> = (0..8)
            .map(|row| buffer[(11, row)].bg == Color::Rgb(40, 40, 40))
            .collect();
        // short, three wrapped rows, the blank line, then the header without one.
        assert_eq!(filled, [true, true, true, true, true, false, false, false]);
    }
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
    fn text_in_no_changed_file_says_n_opens_the_repo_matches() {
        assert_eq!(
            not_in_diff_note(Some(Ok(15)), false),
            "not in the diff; 15 in the repo, n opens the first"
        );
        assert_eq!(
            not_in_diff_note(Some(Ok(980)), true),
            "not in the diff; 980+ in the repo, n opens the first"
        );
        assert_eq!(
            not_in_diff_note(Some(Ok(0)), false),
            "not in the diff or the repo"
        );
        assert_eq!(
            not_in_diff_note(None, false),
            "not in the diff, looking in the repo..."
        );
        assert_eq!(
            not_in_diff_note(Some(Err(())), false),
            "not in the diff; repo not searched"
        );
    }

    #[test]
    fn code_splits_at_the_width_and_keeps_its_spaces() {
        assert_eq!(split_to_width("  abcdef", 3), ["  a", "bcd", "ef"]);
        assert_eq!(split_to_width("", 3), [""]);
        assert_eq!(
            split_to_width("日本語", 4),
            ["日本", "語"],
            "a wide character is not cut"
        );
        assert_eq!(
            split_to_width("aa bb cc", 5),
            ["aa bb ", "cc"],
            "breaks after a space"
        );
        assert_eq!(split_to_width("aaa bbbbbbb", 5), ["aaa ", "bbbbb", "bb"]);
        assert_eq!(
            split_to_width("    abcdefg", 6),
            ["    ab", "cdefg"],
            "not inside the indent"
        );
    }

    #[test]
    fn the_view_shows_the_cursors_line_and_its_threads() {
        let row = |block, code, end| Followed { block, code, end };
        assert_eq!(row(4, 4, 6).scroll(0, 10), 0, "already in view: no move");
        assert_eq!(
            row(12, 12, 15).scroll(0, 10),
            5,
            "below: its last row at the bottom"
        );
        assert_eq!(
            row(2, 2, 3).scroll(8, 10),
            2,
            "above: its first row at the top"
        );
        assert_eq!(
            row(0, 30, 31).scroll(0, 10),
            21,
            "outdated threads taller than the pane: the cursor's code shows at the bottom"
        );
        assert_eq!(
            row(5, 5, 40).scroll(0, 10),
            5,
            "a thread taller than the pane: its code at the top, J reads on"
        );
    }

    #[test]
    fn a_repo_match_marks_the_text_and_its_own_line_strongest() {
        let theme = theme::by_name("dracula").unwrap();
        let paint = Paint {
            theme: &theme,
            now: Utc::now(),
        };
        let finding = RepoFinding {
            pattern: crate::find::Pattern::parse("needle"),
            current: Some(1),
        };
        let line = Line::from(" 2 a Needle here");
        let marked_style = |index| {
            let marked = finding.mark(index, &line, paint);
            let span = marked.spans.iter().find(|span| span.content == "Needle");
            span.map(|span| span.style.bg)
        };
        assert_eq!(
            marked_style(1),
            Some(Some(theme.warning)),
            "the match's line"
        );
        assert_eq!(marked_style(5), Some(Some(theme.selection)), "another line");
        let plain = finding.mark(0, &Line::from(" 1 nothing"), paint);
        assert_eq!(plain, Line::from(" 1 nothing"));
    }

    #[test]
    fn a_narrow_title_drops_hints_then_shrinks_the_block_then_the_path() {
        let text = |line: &Line| -> String {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        };
        let row = |width| {
            let left = vec![
                Piece::new(Span::raw(" word "), RANK_VIEW),
                Piece::new(Span::raw("m switches "), RANK_HINT),
                path_piece(Style::new(), [" ", "src/app/main.rs", " "]),
            ];
            let right = vec![
                Piece::new(Span::raw(" /needle "), RANK_FIND),
                place_piece(
                    Span::raw(" match 1 of 2"),
                    Some(" 1/2 in file 1/3".to_string()),
                    Some(" 1/2".to_string()),
                ),
                Piece::new(Span::raw(", 9 in the repo"), RANK_COUNT),
                Piece::shrinking(
                    Style::new(),
                    ["  in ", "App > run_loop", " "],
                    " > ",
                    [RANK_BLOCK, RANK_BLOCK],
                ),
            ];
            text(&fit_title(width, left, right))
        };
        let wide = row(120);
        assert_eq!(wide.chars().count(), 120, "fills the row");
        assert!(
            wide.starts_with(" word m switches  src/app/main.rs "),
            "{wide}"
        );
        // As the row narrows: the hint drops, then the repo count; the block loses whole
        // names from its start, then cuts its last one, then drops; the place takes its
        // short forms; the place, then the query drop; the path goes last.
        let expected = [
            (
                80,
                " word  src/app/main.rs  /needle  match 1 of 2, 9 in the repo  in App > run_loop ",
            ),
            (
                65,
                " word  src/app/main.rs  /needle  match 1 of 2  in App > run_loop ",
            ),
            (
                63,
                " word  src/app/main.rs  /needle  match 1 of 2  in … > run_loop ",
            ),
            (
                62,
                " word  src/app/main.rs  /needle  match 1 of 2  in …> run_loop ",
            ),
            (46, " word  src/app/main.rs   /needle  match 1 of 2"),
            (40, " word  src/app/main.rs      /needle  1/2"),
            (34, " word  src/app/main.rs    /needle "),
            (30, " word  src/app/main.rs        "),
            (20, " word               "),
        ];
        for (width, row_text) in expected {
            assert_eq!(row(width), row_text, "at {width}");
        }
    }

    #[test]
    fn the_find_and_block_take_a_second_row_only_when_one_row_cannot_hold_them() {
        let text = |line: &Line| -> String {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        };
        let rows = |width| {
            let left = vec![
                Piece::new(Span::raw(" word "), RANK_VIEW),
                Piece::new(Span::raw("m switches "), RANK_HINT),
            ];
            let right = vec![Piece::new(Span::raw(" /needle "), RANK_FIND)];
            let below = vec![Piece::shrinking(
                Style::new(),
                ["  in ", "App > run_loop", " "],
                " > ",
                [RANK_BLOCK, RANK_BLOCK],
            )];
            let title = TitleRows::new(width, left, right, below);
            (text(&title.first), title.second.as_ref().map(text))
        };
        // 6 + 9 + 20 columns hold the view, the find and the block; the hint drops.
        assert_eq!(
            rows(36),
            (" word   /needle   in App > run_loop ".to_string(), None)
        );
        let (first, second) = rows(30);
        assert_eq!(first, " word m switches ", "the view keeps row 1");
        assert_eq!(
            second.as_deref(),
            Some(" /needle   in App > run_loop "),
            "the find and the block move under it"
        );
    }

    #[test]
    fn a_thread_folds_to_its_first_comment_and_opens_to_all() {
        let theme = theme::by_name("dracula").unwrap();
        let paint = Paint {
            theme: &theme,
            now: Utc::now(),
        };
        let activity = crate::model::tests::sample_activity();
        let thread = activity.threads_on("b.md").next().unwrap();
        let text = |lines: Vec<Line>| -> Vec<String> {
            lines
                .iter()
                .map(|line| {
                    line.spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect()
                })
                .collect()
        };
        let folded = text(thread_lines(thread, false, None, 80, paint));
        assert_eq!(folded.len(), 1);
        assert!(
            folded[0].ends_with("╭ lead: Why here?  (1 more)"),
            "{folded:?}"
        );
        let open = text(thread_lines(thread, true, None, 80, paint));
        let shown: Vec<&str> = open.iter().map(|line| line.trim_start()).collect();
        assert!(shown[0].starts_with("╭ lead  "), "{shown:?}");
        assert_eq!(shown[1], "│ Why here?", "the body is rendered Markdown");
        assert!(shown[2].starts_with("├ octo  "), "{shown:?}");
        assert_eq!(shown[3], "│ Because.");
        assert_eq!(shown[4], "╰");
        let outdated = activity.threads_on("a.md").next().unwrap();
        let place = text(thread_lines(
            outdated,
            false,
            Some("lines 827-830"),
            80,
            paint,
        ));
        assert!(
            place[0].contains("╭ lines 827-830, lead: Rename this.  ✓ resolved"),
            "{place:?}"
        );
        let open = text(thread_lines(
            outdated,
            true,
            Some("lines 827-830"),
            80,
            paint,
        ));
        assert!(open[0].contains("╭ lines 827-830, lead  "), "{open:?}");
        let code = ThreadLine {
            comments: vec![crate::model::ThreadComment {
                body: "Call `run` here.".to_string(),
                ..thread.comments[0].clone()
            }],
            ..thread.clone()
        };
        let marked = thread_lines(&code, true, None, 80, paint)
            .iter()
            .flat_map(|line| line.spans.clone())
            .any(|span| span.style.bg == Some(theme.selection));
        assert!(!marked, "inline code does not look like a marked range");
    }

    #[test]
    fn a_long_row_wraps_under_its_code_or_is_cut() {
        let theme = theme::by_name("dracula").unwrap();
        let paint = Paint {
            theme: &theme,
            now: Utc::now(),
        };
        let row = crate::line_diff::parse("@@ -1 +1 @@\n+abcdefghij\n").remove(1);
        let wrapped = texts(&select_row(&row, Span::raw(" "), 20, true, paint));
        assert_eq!(wrapped, ["           1 +abcdef", "              ghij"]);
        let cut = texts(&select_row(&row, Span::raw(" "), 20, false, paint));
        assert_eq!(cut, ["           1 +abcdefghij"], "the pane clips it");
    }

    #[test]
    fn a_file_with_review_threads_shows_their_count_and_lines() {
        let theme = theme::by_name("dracula").unwrap();
        let paint = Paint {
            theme: &theme,
            now: Utc::now(),
        };
        let activity = crate::model::tests::sample_activity();
        let file = ChangedFile {
            path: "a.md".to_string(),
            additions: 1,
            deletions: 2,
            change: "modified".to_string(),
            viewed: ViewedState::Unviewed,
        };
        let row = texts(&[file_line(&file, false, 2, 40, paint)]).remove(0);
        assert!(row.contains("●2 +1 -2"), "{row:?}");
        let none = texts(&[file_line(&file, false, 0, 40, paint)]).remove(0);
        assert!(!none.contains('●'), "{none:?}");
        assert_eq!(
            display_width(&row),
            display_width(&none),
            "the mark takes path room"
        );
        assert_eq!(
            thread_note(&activity, "a.md"),
            " 1 outdated thread, S to read "
        );
        assert_eq!(thread_note(&activity, "b.md"), " 1 thread, S to read ");
        assert_eq!(thread_note(&activity, "c.md"), "");
    }

    #[test]
    fn a_loading_pr_shows_its_inbox_row_as_the_header() {
        let theme = theme::by_name("dracula").unwrap();
        let paint = Paint {
            theme: &theme,
            now: Utc::now(),
        };
        let summary = crate::model::tests::sample_detail().summary;
        let lines = texts(&summary_header(&summary, 80, paint));
        assert_eq!(
            lines[0].trim(),
            format!("{} #{}", summary.repo, summary.number)
        );
        assert_eq!(lines[1].trim(), summary.title);
        assert!(lines[2].contains(&summary.author), "{lines:?}");
        assert!(lines[2].contains("Open") || lines[2].contains("Draft"));
    }

    #[test]
    fn an_inbox_with_no_groups_says_how_to_get_some() {
        let theme = theme::by_name("dracula").unwrap();
        let mut app = App::new();
        app.loading = false;
        let backend = ratatui::backend::TestBackend::new(100, 10);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render(frame, &mut app, &theme, Utc::now()))
            .unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("No groups in config.yaml"), "{screen}");
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
    fn inbox_badges_take_the_same_width_with_or_without_comments() {
        let theme = theme::by_name("dracula").unwrap();
        let paint = Paint {
            theme: &theme,
            now: Utc::now(),
        };
        let width = |comments: u64| {
            let pr = PrSummary {
                repo: "acme/api".into(),
                number: 1,
                title: "Fix".into(),
                url: String::new(),
                author: "joey".into(),
                updated_at: Utc::now(),
                is_draft: false,
                review: ReviewState::Awaiting,
                checks: None,
                comments,
            };
            Line::from(badge_spans(&pr, theme.muted, paint)).width()
        };
        assert_eq!(width(0), width(1));
        assert_eq!(width(0), width(12));
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
    fn a_path_cut_from_the_start_keeps_its_file_name() {
        assert_eq!(truncate_start("src/app/main.rs", 9), "…/main.rs");
        assert_eq!(truncate_start("a.rs", 9), "a.rs");
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
