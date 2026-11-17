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
    App, DetailView, DiffOutput, Overlay, OverviewLayout, Prompt, PromptKind, RepoState, Row,
    Screen, Tab,
};
use crate::file_tree::FileRow;
use crate::model::{
    ChangedFile, CheckLine, CheckState, CheckSummary, CommitLine, Group, Label, PrActivity,
    PrDetail, PrRef, PrSummary, ReviewState, ViewedState,
};
use crate::repo::{GrepHit, MAX_GREP_HITS};
use crate::theme::Theme;
use crate::{markdown, overview};

const INBOX_HELP: &str = "j/k move | enter open | space fold | tab next group | o browser | r refresh | ctrl-l link | ? keys";

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
    if app.show_help {
        render_help(frame, paint);
    }
    if let Some(prompt) = &app.prompt {
        render_prompt(frame, prompt, paint);
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

fn detail_help(tab: Tab) -> &'static str {
    match tab {
        Tab::Overview => {
            "j/k next comment | enter fold | f/F/n filter, order, bots | o browser | ? keys"
        }
        Tab::Checks => "j/k move | enter/o browser | r reload | esc back | ? keys",
        Tab::Commits => {
            "j/k commit | J/K scroll diff | m renderer | / find | o browser | esc back | ? keys"
        }
        Tab::Files => {
            "j/k file | enter whole file | v viewed | ]/[ unviewed | J/K scroll | m renderer | ? keys"
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
            ("enter", "show the whole file at the PR head"),
            ("v, H", "toggle Viewed (syncs to GitHub), hide viewed files"),
            ("], [", "next or previous unviewed file"),
        ],
    ),
    (
        "Files changed, Commits",
        &[
            ("J/K, space/b", "scroll the diff"),
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
    let [top, tabs, body, bottom] = Layout::vertical([
        Constraint::Length(3),
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
        frame.render_widget(Paragraph::new(detail_title(&view.pr, paint)), top);
        let message = match &view.detail {
            Some(Err(error)) => paint.span(error.clone(), theme.danger),
            _ => paint.span("loading...", theme.warning),
        };
        frame.render_widget(Paragraph::new(message).wrap(Wrap { trim: false }), body);
        return;
    };
    frame.render_widget(Paragraph::new(detail_header(detail, paint)), top);
    let tab_lines = vec![
        tab_bar(view.tab, detail, paint),
        tab_rule(view.tab, detail, tabs.width, paint),
    ];
    frame.render_widget(Paragraph::new(tab_lines), tabs);
    if let Some(overlay) = view.overlays.last_mut() {
        render_overlay(frame, overlay, body, paint);
        return;
    }
    match view.tab {
        Tab::Overview => render_overview(frame, view, body, paint),
        Tab::Checks => match &view.activity {
            Some(Ok(activity)) => render_checks(frame, activity, &mut view.checks, body, paint),
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
            render_commits(
                frame,
                activity,
                detail.commit_count,
                &mut view.commits,
                list_area,
                paint,
            );
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

fn render_overlay(frame: &mut Frame, overlay: &mut Overlay, area: Rect, paint: Paint) {
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
            let list = List::new(items)
                .highlight_style(Style::new().bg(theme.selection))
                .block(Block::new().title(paint.bold(title, theme.heading)));
            frame.render_stateful_widget(list, area, state);
        }
        Overlay::File {
            path,
            line,
            lines,
            scroll,
        } => {
            let block =
                Block::new().title(paint.bold(format!(" {path} at the PR head "), theme.heading));
            let inner = block.inner(area);
            frame.render_widget(block, area);
            let text = match lines {
                None => Text::from(paint.span(" loading...", theme.warning)),
                Some(Err(error)) => Text::from(paint.span(format!(" {error}"), theme.danger)),
                Some(Ok(lines)) => {
                    file_text(lines, *line, *scroll as usize, inner.height as usize, paint)
                }
            };
            frame.render_widget(Paragraph::new(text), inner);
        }
    }
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

/// The visible lines of a file with line numbers. The marked line gets the selection color.
fn file_text(
    lines: &[String],
    marked: usize,
    scroll: usize,
    height: usize,
    paint: Paint,
) -> Text<'static> {
    let theme = paint.theme;
    let gutter = lines.len().to_string().len();
    let visible = lines
        .iter()
        .enumerate()
        .skip(scroll)
        .take(height)
        .map(|(index, code)| {
            let number = index + 1;
            let line = Line::from(vec![
                paint.span(format!("{number:>gutter$} "), theme.muted),
                paint.span(code.clone(), theme.foreground),
            ]);
            if number == marked {
                line.style(Style::new().bg(theme.selection))
            } else {
                line
            }
        });
    Text::from(visible.collect::<Vec<_>>())
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
                Paragraph::new(paint.span(" loading files...", theme.warning)),
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
    let items: Vec<ListItem> = view
        .file_rows()
        .iter()
        .map(|row| ListItem::new(file_row_line(row, files, view.tree_view, width, paint)))
        .collect();
    let list = List::new(items)
        .highlight_style(Style::new().bg(theme.selection))
        .block(Block::new().title(paint.span(title, theme.heading)));
    frame.render_stateful_widget(list, area, &mut view.file_state);
    true
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
    let theme = paint.theme;
    let renderer = renderer_names
        .get(view.renderer)
        .map_or("diff", String::as_str);
    let block = Block::new()
        .borders(Borders::LEFT)
        .border_style(Style::new().fg(theme.muted))
        .title(paint.bold(format!(" {renderer} "), theme.heading))
        .title(paint.span("m switches ", theme.muted))
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
                // Clone only the visible lines; a long diff is redrawn every frame.
                let lines = text
                    .lines
                    .iter()
                    .skip(view.diff_scroll as usize)
                    .take(inner.height as usize);
                Paragraph::new(Text::from(lines.cloned().collect::<Vec<_>>()))
            }
        },
    };
    frame.render_widget(content, inner);
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

fn detail_title(pr: &PrRef, paint: Paint) -> Text<'static> {
    Text::from(vec![
        Line::from(paint.bold(format!("{}#{}", pr.repo, pr.number), paint.theme.foreground)),
        Line::from(paint.span(pr.url(), paint.theme.muted)),
    ])
}

/// Title, then a GitHub-style state pill with the branches and the size of the change.
fn detail_header(detail: &PrDetail, paint: Paint) -> Text<'static> {
    let theme = paint.theme;
    let pr = &detail.summary;
    let (state, state_color) = if pr.is_draft {
        (" Draft ", theme.muted)
    } else {
        (" Open ", theme.success)
    };
    let pill = Style::new()
        .fg(theme.background)
        .bg(state_color)
        .add_modifier(Modifier::BOLD);
    let branch = Style::new().fg(theme.accent).bg(theme.selection);
    Text::from(vec![
        Line::from(vec![
            paint.bold(format!(" {}  ", pr.title), theme.foreground),
            paint.span(format!("#{}", pr.number), theme.muted),
        ]),
        Line::from(vec![
            Span::raw(" "),
            Span::styled(state, pill),
            paint.bold(format!(" {}", pr.author), theme.foreground),
            paint.span(" wants to merge into ", theme.muted),
            Span::styled(format!(" {} ", detail.base), branch),
            paint.span(" from ", theme.muted),
            Span::styled(format!(" {} ", detail.head), branch),
            paint.span(format!("   +{}", detail.additions), theme.success),
            paint.span(format!(" -{}", detail.deletions), theme.danger),
            paint.span(format!(" in {} files", detail.changed_files), theme.muted),
        ]),
        Line::from(paint.span(format!(" {}", pr.repo), theme.muted)),
    ])
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
        spans.push(Span::styled(format!("  {}", tab.title()), title_style));
        spans.extend(tab_badge(tab, detail, paint));
        spans.push(Span::raw("  "));
    }
    Line::from(spans)
}

fn tab_badge(tab: Tab, detail: &PrDetail, paint: Paint) -> Vec<Span<'static>> {
    let theme = paint.theme;
    let badge = |count: u64| {
        vec![
            Span::raw(" "),
            Span::styled(
                format!(" {count} "),
                Style::new().fg(theme.foreground).bg(theme.selection),
            ),
        ]
    };
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

/// A thin rule with a thick bar under the current tab, like GitHub's underline.
fn tab_rule(current: Tab, detail: &PrDetail, width: u16, paint: Paint) -> Line<'static> {
    let theme = paint.theme;
    let mut start = 0;
    let mut bar = 0;
    for tab in Tab::ALL {
        let label_width: usize = tab_bar_width(tab, detail, paint);
        if tab == current {
            // Under the title and badge, not the padding around them.
            start += 2;
            bar = label_width.saturating_sub(4);
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
    2 + display_width(tab.title()) + badge + 2
}

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
    if sidebar_width > 0 {
        render_sidebar(frame, detail, sidebar, paint);
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

fn render_sidebar(frame: &mut Frame, detail: &PrDetail, area: Rect, paint: Paint) {
    let border = Block::new()
        .borders(Borders::LEFT)
        .border_style(Style::new().fg(paint.theme.selection));
    let inner = border.inner(area);
    frame.render_widget(border, area);
    let lines = sidebar_lines(detail, inner.width.saturating_sub(1) as usize, paint);
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
) {
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
    let mut list = List::new(items).highlight_style(Style::new().bg(theme.selection));
    if commit_count > activity.commits.len() as u64 {
        let title = format!(
            " first {} of {} commits ",
            activity.commits.len(),
            commit_count
        );
        list = list.block(Block::new().title(paint.span(title, theme.warning)));
    }
    frame.render_stateful_widget(list, area, state);
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
) {
    let theme = paint.theme;
    if activity.checks.is_empty() {
        frame.render_widget(
            Paragraph::new(paint.span(" No checks ran on the head commit.", theme.muted)),
            area,
        );
        return;
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
    let list = List::new(items).highlight_style(Style::new().bg(theme.selection));
    frame.render_stateful_widget(list, area, state);
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
            "the folder that holds your clones. enter saves it to the config, esc skips"
                .to_string(),
        ),
        PromptKind::SearchRepo => (
            " Search the repo at the PR head ".to_string(),
            "finds text in every file, not only the changed ones. case-insensitive".to_string(),
        ),
        PromptKind::FindInDiff => (
            " Find in the diff ".to_string(),
            "jumps to the first line with this text. n next, N previous".to_string(),
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

fn sidebar_lines(detail: &PrDetail, width: usize, paint: Paint) -> Vec<Line<'static>> {
    let theme = paint.theme;
    let mut lines = section("Reviewers", paint);
    lines.extend(detail.reviewers.iter().map(|reviewer| {
        let (symbol, color) = reviewer_symbol(&reviewer.state, theme);
        Line::from(vec![
            paint.span(format!("{symbol} "), color),
            paint.span(reviewer.name.clone(), theme.foreground),
            paint.span(format!("  {}", reviewer.state), theme.muted),
        ])
    }));
    if detail.reviewers.is_empty() {
        lines.push(Line::from(paint.span("No reviews", theme.muted)));
    }
    lines.extend(section("Assignees", paint));
    lines.extend(
        detail
            .assignees
            .iter()
            .map(|name| Line::from(paint.span(name.clone(), theme.foreground))),
    );
    if detail.assignees.is_empty() {
        lines.push(Line::from(paint.span("No one", theme.muted)));
    }
    lines.extend(section("Labels", paint));
    lines.extend(label_chips(&detail.labels, width));
    if detail.labels.is_empty() {
        lines.push(Line::from(paint.span("None yet", theme.muted)));
    }
    lines
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
        _ => ("○", theme.muted),
    }
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
