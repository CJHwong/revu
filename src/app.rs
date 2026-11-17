//! Screen state and key handling. It does no I/O: keys that need I/O return an `Action`.

use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Text;
use ratatui::widgets::ListState;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::file_tree::{self, FileRow};
use crate::markdown::TextRow;
use crate::repo::{GrepHit, LocalRepo};

use crate::config::{GroupConfig, ViewPrefs};
use crate::model::{
    ChangedFile, Group, PrActivity, PrDetail, PrRef, PrSummary, ViewedState, parse_pr_ref,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Header(usize),
    /// Group index, then the PR index inside that group.
    Pr(usize, usize),
}

#[derive(Debug, Clone)]
pub enum Action {
    Quit,
    Refresh,
    FetchDetail(PrRef),
    OpenBrowser(String),
    RunDiff(PrRef),
    Grep {
        pr: PrRef,
        dir: PathBuf,
        commit: String,
        query: String,
    },
    /// The words of the PR's changed lines, for the find prompt to complete.
    FindWords {
        pr: PrRef,
        dir: PathBuf,
        base: String,
        head: String,
    },
    /// Which changed files hold the find text.
    FindFiles {
        pr: PrRef,
        dir: PathBuf,
        base: String,
        head: String,
        query: String,
    },
    /// A file at the PR head with syntax colors, the `line` marked (0 for none).
    ShowFile {
        pr: PrRef,
        dir: PathBuf,
        commit: String,
        path: String,
        line: usize,
    },
    /// A changed file in full, drawn by the diff renderer with its changes marked.
    ShowWholeDiff(DiffJob),
    SetWorkspace(String),
    /// Apply a built-in theme and save it to the config.
    SetTheme(&'static str),
    CloneRepo {
        pr: PrRef,
        dir: PathBuf,
    },
    SetViewed {
        pr: PrRef,
        pull_request_id: String,
        path: String,
        viewed: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Commits,
    Checks,
    Files,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Overview, Tab::Commits, Tab::Checks, Tab::Files];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Commits => "Commits",
            Tab::Checks => "Checks",
            Tab::Files => "Files changed",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0)
    }

    fn shifted(self, step: isize) -> Tab {
        let count = Self::ALL.len() as isize;
        Self::ALL[(self.index() as isize + step).rem_euclid(count) as usize]
    }
}

/// The user's local clone for the open PR, which the diff panes need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoState {
    /// Not looked for yet; the main loop picks this up.
    Unknown,
    /// Looking, cloning, fetching, or waiting on the user. The text says which.
    Busy(String),
    Ready(LocalRepo),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DiffTarget {
    File(String),
    Commit(String),
}

/// Where the last find landed: the diff it ran in, the line, and its place among the
/// lines that match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindHit {
    pub key: DiffKey,
    pub line: usize,
    pub ordinal: usize,
    pub total: usize,
}

/// Lines a find keeps above the match, so the match does not sit on the pane's edge.
const FIND_CONTEXT: usize = 3;

/// One rendered diff: a target drawn by one renderer at one pane width.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DiffKey {
    pub target: DiffTarget,
    pub renderer: usize,
    pub width: u16,
}

pub enum DiffOutput {
    Pending,
    Done(Result<Text<'static>, String>),
}

/// A screen region the last draw put something clickable in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClickTarget {
    pub area: ratatui::layout::Rect,
    pub click: Click,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Click {
    /// An item of the list on screen: inbox rows, files, commits, checks or search hits.
    Row(usize),
    Tab(Tab),
    /// The Overview page; `first` is the page row at the top of the area.
    Page {
        first: usize,
    },
}

/// What the main loop needs to render one diff off the UI thread.
#[derive(Debug, Clone)]
pub struct DiffJob {
    pub pr: PrRef,
    pub key: DiffKey,
    pub dir: PathBuf,
    pub base: String,
    pub head: String,
    /// One path, or the old and new path of a rename. Empty for a whole commit.
    pub paths: Vec<String>,
}

/// What the main loop needs to find or fetch the PR's commits in a local clone.
pub struct RepoJob {
    pub pr: PrRef,
    pub base_ref: String,
    pub base_oid: String,
    pub head_oid: String,
}

/// A view over the tab, read from the clone at the PR's head. Esc goes back one level.
pub enum Overlay {
    Search {
        query: String,
        hits: Option<Result<Vec<GrepHit>, String>>,
        state: ListState,
    },
    File {
        path: String,
        /// The file with the renderer's or bat's colors.
        lines: Option<Result<Text<'static>, String>>,
        scroll: u16,
    },
}

/// How far a movement key moves a list or a page.
fn step(key: KeyCode) -> Option<i16> {
    match key {
        KeyCode::Char('j') | KeyCode::Down => Some(1),
        KeyCode::Char('k') | KeyCode::Up => Some(-1),
        KeyCode::Char(' ') | KeyCode::PageDown => Some(20),
        KeyCode::Char('b') | KeyCode::PageUp => Some(-20),
        KeyCode::Char('g') | KeyCode::Home => Some(i16::MIN),
        KeyCode::Char('G') | KeyCode::End => Some(i16::MAX),
        _ => None,
    }
}

pub struct OverviewLayout {
    pub width: usize,
    pub rows: Vec<TextRow>,
}

pub struct DetailView {
    pub pr: PrRef,
    /// `None` while the request runs.
    pub detail: Option<Result<PrDetail, String>>,
    /// Checks, commits and the conversation, which arrive separately.
    pub activity: Option<Result<PrActivity, String>>,
    pub tab: Tab,
    /// The Overview's cursor row and its first visible row.
    pub cursor: usize,
    pub scroll: usize,
    /// Folds the user opened or closed, away from their default state.
    pub toggled_folds: HashSet<crate::markdown::FoldId>,
    pub comments: crate::overview::CommentView,
    /// The Overview laid out at the last drawn width. Cleared when its content changes.
    pub overview: Option<OverviewLayout>,
    /// The Overview's visible height at the last draw, for paging.
    pub page: usize,
    pub commits: ListState,
    pub checks: ListState,
    /// `None` until the Files tab is first shown and its request returns.
    pub files: Option<Result<Vec<ChangedFile>, String>>,
    files_requested: bool,
    /// While the file list loads: the files fetched so far and the PR's total.
    pub files_progress: Option<(usize, usize)>,
    /// Indexes into the visible files, not into `files`.
    pub file_state: ListState,
    pub hide_viewed: bool,
    /// Files changed lists folders as a tree, like GitHub, or flat paths.
    pub tree_view: bool,
    /// Tree folders the user folded, by path.
    pub folded_dirs: HashSet<String>,
    /// Files changed hides its list so the diff gets the full width.
    pub file_list_hidden: bool,
    /// The list's top row while the wheel scrolls it apart from the selection, so the
    /// diff does not reload on every wheel step. None: the list follows the selection.
    pub file_list_top: Option<usize>,
    /// The rows the file list showed at the last draw.
    pub file_list_height: usize,
    pub repo: RepoState,
    pub renderer: usize,
    pub diff_scroll: u16,
    /// Long diff lines wrap. Off, they run past the edge and `h`/`l` scroll sideways.
    pub diff_wrap: bool,
    /// The columns the diff is scrolled sideways, when it does not wrap.
    pub diff_shift: u16,
    /// The diff pane's height at the last draw.
    pub diff_height: u16,
    /// The width under the tabs at the last draw; the whole-file view renders to it.
    pub body_width: u16,
    /// What a mouse click can hit, from the last draw.
    pub clicks: Vec<ClickTarget>,
    /// The diff pane's width at the last draw; diffs render to fit it.
    pub diff_width: u16,
    /// The diff pane's left column at the last draw. The mouse wheel scrolls the diff to
    /// the right of it and the list to the left.
    pub diff_left: u16,
    pub diffs: HashMap<DiffKey, DiffOutput>,
    pub overlays: Vec<Overlay>,
    /// The lowercased text that `n` and `N` look for in the diff.
    pub find: Option<String>,
    pub find_hit: Option<FindHit>,
    /// On Files changed, the files that hold the find text and how many lines in each,
    /// in the list's order; None while git looks.
    pub find_files: Option<Result<Vec<(String, usize)>, String>>,
    /// The PR's changed words for the find prompt to complete; None until git returns.
    pub find_words: Option<Vec<String>>,
    find_words_requested: bool,
    /// n or N moved to another file; the find runs there once its diff is drawn. True
    /// goes to its first match, false to its last.
    find_pending: Option<bool>,
}

impl DetailView {
    fn new(pr: PrRef, prefs: &ViewPrefs, renderer_names: &[String]) -> Self {
        let renderer = prefs
            .renderer
            .as_ref()
            .and_then(|name| renderer_names.iter().position(|known| known == name))
            .unwrap_or(0);
        Self {
            pr,
            detail: None,
            activity: None,
            tab: Tab::Overview,
            cursor: 0,
            scroll: 0,
            toggled_folds: HashSet::new(),
            comments: crate::overview::CommentView {
                newest_first: prefs.newest_first,
                no_bots: prefs.no_bots,
                ..Default::default()
            },
            overview: None,
            page: 20,
            commits: ListState::default().with_selected(Some(0)),
            checks: ListState::default().with_selected(Some(0)),
            files: None,
            files_requested: false,
            files_progress: None,
            file_state: ListState::default().with_selected(Some(0)),
            tree_view: prefs.tree_view,
            folded_dirs: HashSet::new(),
            file_list_hidden: prefs.file_list_hidden,
            file_list_top: None,
            file_list_height: 0,
            hide_viewed: prefs.hide_viewed,
            repo: RepoState::Unknown,
            renderer,
            diff_scroll: 0,
            diff_wrap: prefs.diff_wrap,
            diff_shift: 0,
            diff_width: 0,
            diff_left: 0,
            diff_height: 0,
            body_width: 0,
            clicks: Vec::new(),
            diffs: HashMap::new(),
            overlays: Vec::new(),
            find: None,
            find_hit: None,
            find_files: None,
            find_pending: None,
            find_words: None,
            find_words_requested: false,
        }
    }

    /// The diff the pane should show now, or `None` when no diff pane is showing.
    pub fn current_diff_key(&self) -> Option<DiffKey> {
        if self.diff_width == 0 {
            return None;
        }
        let target = match self.tab {
            Tab::Files => DiffTarget::File(
                self.loaded_files()?
                    .get(self.selected_file()?)?
                    .path
                    .clone(),
            ),
            Tab::Commits => {
                let index = self.commits.selected()?;
                DiffTarget::Commit(self.loaded_activity()?.commits.get(index)?.oid.clone())
            }
            Tab::Overview | Tab::Checks => return None,
        };
        Some(DiffKey {
            target,
            renderer: self.renderer,
            width: self.diff_width,
        })
    }

    fn scroll_diff(&mut self, delta: i16) {
        self.diff_scroll = self.diff_scroll.saturating_add_signed(delta);
    }

    /// One wheel tick over a diff: to the next or previous hunk, or a third of the pane
    /// when that hunk is more than a pane away, so a long hunk can still be read.
    /// Scrolls the file list three rows and leaves the selection, and so the diff, alone.
    fn wheel_file_list(&mut self, delta: isize) {
        let last_top = self.file_rows().len().saturating_sub(self.file_list_height);
        let top = self.file_list_top.unwrap_or(self.file_state.offset());
        self.file_list_top = Some(top.saturating_add_signed(delta * 3).min(last_top));
    }

    fn wheel_diff(&mut self, forward: bool) {
        let chunk = (self.diff_height / 3).max(3);
        let current = self.diff_scroll as usize;
        let starts = self.hunk_starts();
        let target = if forward {
            starts.into_iter().find(|start| *start > current)
        } else {
            starts.into_iter().rev().find(|start| *start < current)
        };
        match target {
            Some(target) if target.abs_diff(current) <= self.diff_height as usize => {
                self.diff_scroll = target as u16;
            }
            _ if forward => self.scroll_diff(chunk as i16),
            _ => self.scroll_diff(-(chunk as i16)),
        }
        // The wheel stops at the last screen of the diff instead of scrolling into blank rows.
        if let Some(DiffOutput::Done(Ok(text))) =
            self.current_diff_key().and_then(|key| self.diffs.get(&key))
        {
            let last_screen = text.lines.len().saturating_sub(self.diff_height as usize);
            self.diff_scroll = self.diff_scroll.min(last_screen as u16);
        }
    }

    /// The lines where a hunk starts in the shown diff, in the shapes the default
    /// renderers print. A custom renderer without them scrolls by chunks.
    fn hunk_starts(&self) -> Vec<usize> {
        let Some(DiffOutput::Done(Ok(text))) =
            self.current_diff_key().and_then(|key| self.diffs.get(&key))
        else {
            return Vec::new();
        };
        text.lines
            .iter()
            .enumerate()
            .filter(|(_, line)| {
                let plain: String = line
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect();
                is_hunk_start(&plain)
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// The clone folder and the PR's head commit, once the clone is ready.
    fn head_in_clone(&self) -> Result<(PathBuf, String), String> {
        let RepoState::Ready(repo) = &self.repo else {
            return Err("the local clone is not ready; open Files changed first".to_string());
        };
        let detail = self.loaded().ok_or("the pull request is still loading")?;
        Ok((repo.dir.clone(), detail.head_oid.clone()))
    }

    fn search(&mut self, query: String) -> Result<Action, String> {
        if query.trim().is_empty() {
            return Err("type some text to search for".to_string());
        }
        let (dir, commit) = self.head_in_clone()?;
        let state = ListState::default().with_selected(Some(0));
        self.overlays.push(Overlay::Search {
            query: query.clone(),
            hits: None,
            state,
        });
        Ok(Action::Grep {
            pr: self.pr.clone(),
            dir,
            commit,
            query,
        })
    }

    fn open_file(&mut self, path: String, line: usize) -> Result<Action, String> {
        let (dir, commit) = self.head_in_clone()?;
        // Show a few lines of context above the marked line.
        let scroll = u16::try_from(line.saturating_sub(6)).unwrap_or(u16::MAX);
        self.overlays.push(Overlay::File {
            path: path.clone(),
            lines: None,
            scroll,
        });
        Ok(Action::ShowFile {
            pr: self.pr.clone(),
            dir,
            commit,
            path,
            line,
        })
    }

    /// The selected changed file in full, with its changes, as the current renderer draws
    /// it across the whole screen.
    fn open_whole_diff(&mut self) -> Result<Action, String> {
        let RepoState::Ready(repo) = &self.repo else {
            return Err("the local clone is not ready yet".to_string());
        };
        let mut key = self.current_diff_key().ok_or("select a file first")?;
        let DiffTarget::File(path) = &key.target else {
            return Err("select a file first".to_string());
        };
        key.width = self.body_width.max(self.diff_width);
        let job = diff_job(
            &self.pr,
            repo,
            self.loaded().ok_or("the pull request is still loading")?,
            key.clone(),
        );
        self.overlays.push(Overlay::File {
            path: path.clone(),
            lines: None,
            scroll: 0,
        });
        Ok(Action::ShowWholeDiff(job))
    }

    fn overlay_key(&mut self, key: KeyCode) -> Result<Option<Action>, String> {
        let Some(overlay) = self.overlays.last_mut() else {
            return Ok(None);
        };
        match (overlay, key) {
            (_, KeyCode::Esc | KeyCode::Char('q' | 'h') | KeyCode::Left) => {
                self.overlays.pop();
            }
            (
                Overlay::Search {
                    hits: Some(Ok(hits)),
                    state,
                    ..
                },
                KeyCode::Enter,
            ) => {
                if let Some(hit) = state.selected().and_then(|index| hits.get(index)).cloned() {
                    return self.open_file(hit.path, hit.line).map(Some);
                }
            }
            (
                Overlay::Search {
                    hits: Some(Ok(hits)),
                    state,
                    ..
                },
                code,
            ) => {
                if let (Some(delta), Some(last)) = (step(code), hits.len().checked_sub(1)) {
                    let next = state
                        .selected()
                        .unwrap_or(0)
                        .saturating_add_signed(delta.into());
                    state.select(Some(next.min(last)));
                }
            }
            (
                Overlay::File {
                    lines: Some(Ok(lines)),
                    scroll,
                    ..
                },
                code,
            ) => {
                if let Some(delta) = step(code) {
                    let last =
                        u16::try_from(lines.lines.len().saturating_sub(1)).unwrap_or(u16::MAX);
                    *scroll = scroll.saturating_add_signed(delta).min(last);
                }
            }
            _ => {}
        }
        Ok(None)
    }

    /// Finds the text in the diff on screen. On Files changed it also asks git which
    /// other files hold it, so n and N can go on to them.
    fn start_find(&mut self, query: &str) -> Result<Option<Action>, String> {
        if query.trim().is_empty() {
            return Err("type some text to find".to_string());
        }
        self.find = Some(query.to_lowercase());
        self.find_hit = None;
        self.find_files = None;
        self.find_pending = None;
        let here = self.find_in_diff(true, false);
        if self.tab != Tab::Files {
            return here.map(|()| None);
        }
        let (dir, head) = self.head_in_clone()?;
        let base = self
            .loaded()
            .ok_or("the pull request is still loading")?
            .base_oid
            .clone();
        Ok(Some(Action::FindFiles {
            pr: self.pr.clone(),
            dir,
            base,
            head,
            query: query.to_lowercase(),
        }))
    }

    /// Asks git once for the words the find prompt completes.
    fn words_request(&mut self) -> Option<Action> {
        if self.find_words_requested {
            return None;
        }
        let (dir, head) = self.head_in_clone().ok()?;
        let base = self.loaded()?.base_oid.clone();
        self.find_words_requested = true;
        Some(Action::FindWords {
            pr: self.pr.clone(),
            dir,
            base,
            head,
        })
    }

    /// n and N: the next or previous match. On Files changed, past this file's last match
    /// (or with none here) it goes on to the next file that holds the text.
    fn find_next(&mut self, forward: bool) -> Result<(), String> {
        let in_file = self.tab == Tab::Files && matches!(self.find_files, Some(Ok(_)));
        if !in_file {
            return self.find_in_diff(forward, true);
        }
        let wraps = match self.current_find_hit() {
            Some(hit) if forward => hit.ordinal == hit.total,
            Some(hit) => hit.ordinal == 1,
            None => true,
        };
        if !wraps {
            return self.find_in_diff(forward, true);
        }
        self.next_find_file(forward)
    }

    /// Selects the next (or previous) file that holds the find text, opening folders on the
    /// way, and finds in it once its diff is drawn.
    fn next_find_file(&mut self, forward: bool) -> Result<(), String> {
        let order = self.find_order();
        let files = self
            .loaded_files()
            .ok_or("the file list is still loading")?;
        let query = self.find.clone().unwrap_or_default();
        if order.is_empty() {
            return Err(format!("\"{query}\" is in no shown file"));
        }
        let current = self.selected_file();
        let at = current.and_then(|file| order.iter().position(|index| *index == file));
        let next = match (at, forward) {
            (Some(at), true) => order[(at + 1) % order.len()],
            (Some(at), false) => order[(at + order.len() - 1) % order.len()],
            (None, true) => order[0],
            (None, false) => order[order.len() - 1],
        };
        let path = files[next].path.clone();
        self.folded_dirs
            .retain(|dir| !path.starts_with(&format!("{dir}/")));
        self.file_list_top = None;
        self.select_file_row(Some(next));
        self.reset_diff_position();
        self.find_hit = None;
        self.find_pending = Some(forward);
        self.settle_find();
        Ok(())
    }

    /// Runs a find that waited for its file's diff, once the diff is drawn.
    pub fn settle_find(&mut self) {
        let Some(forward) = self.find_pending else {
            self.follow_redraw();
            return;
        };
        let ready = self
            .current_diff_key()
            .is_some_and(|key| matches!(self.diffs.get(&key), Some(DiffOutput::Done(_))));
        if !ready {
            return;
        }
        self.find_pending = None;
        // A failed or empty diff has no match; the title says so.
        let _ = self.find_in_diff(forward, false);
    }

    /// Scrolls the diff to the next (or previous) line holding the find text, a few lines
    /// below the top. It goes on from the last match in this diff, else from the top line,
    /// and wraps around the end. It searches only the diff on screen.
    fn find_in_diff(&mut self, forward: bool, skip_current: bool) -> Result<(), String> {
        let query = self
            .find
            .clone()
            .ok_or("press / to find text in the diff")?;
        let (key, matches) = self.find_matches(&query)?;
        if matches.is_empty() {
            self.find_hit = None;
            return Err(format!("\"{query}\" is not in this file's diff"));
        }
        let (from, skip) = match &self.find_hit {
            Some(hit) if hit.key == key && skip_current => (hit.line, true),
            _ => (self.diff_scroll as usize, false),
        };
        let position = if forward {
            let after = |line: &usize| *line > from || (!skip && *line == from);
            matches.iter().position(after).unwrap_or(0)
        } else {
            let before = matches.iter().rposition(|line| *line < from);
            before.unwrap_or(matches.len() - 1)
        };
        self.land_on(key, &matches, position);
        Ok(())
    }

    /// The lines of the diff on screen that hold the text, with the diff's key.
    fn find_matches(&self, query: &str) -> Result<(DiffKey, Vec<usize>), String> {
        let key = self.current_diff_key().ok_or("no diff here")?;
        let Some(DiffOutput::Done(Ok(text))) = self.diffs.get(&key) else {
            return Err("the diff is not ready".to_string());
        };
        let pattern = crate::find::Pattern::parse(query);
        let matches = text
            .lines
            .iter()
            .enumerate()
            .filter(|(_, line)| pattern.matches_drawn(line))
            .map(|(index, _)| index)
            .collect();
        Ok((key, matches))
    }

    fn land_on(&mut self, key: DiffKey, matches: &[usize], position: usize) {
        let line = matches[position];
        self.diff_scroll = u16::try_from(line.saturating_sub(FIND_CONTEXT)).unwrap_or(u16::MAX);
        self.find_hit = Some(FindHit {
            key,
            line,
            ordinal: position + 1,
            total: matches.len(),
        });
    }

    /// A resize or another renderer draws the same file again under a new key; the find
    /// stays on the same match there, by its number.
    fn follow_redraw(&mut self) {
        let (Some(query), Some(hit), Some(key)) = (
            self.find.clone(),
            self.find_hit.as_ref(),
            self.current_diff_key(),
        ) else {
            return;
        };
        if hit.key == key || hit.key.target != key.target {
            return;
        }
        let ordinal = hit.ordinal;
        let Ok((key, matches)) = self.find_matches(&query) else {
            return;
        };
        if matches.is_empty() {
            self.find_hit = None;
            return;
        }
        self.land_on(key, &matches, ordinal.min(matches.len()) - 1);
    }

    /// Files changed: the files that hold the find text, in the list's order, folders open.
    fn find_order(&self) -> Vec<usize> {
        let (Some(Ok(found)), Some(files)) = (&self.find_files, self.loaded_files()) else {
            return Vec::new();
        };
        let holds = |index: &usize| found.iter().any(|(path, _)| *path == files[*index].path);
        file_tree::rows(
            files,
            &self.visible_files(),
            self.tree_view,
            &HashSet::new(),
        )
        .into_iter()
        .filter_map(|row| match row {
            FileRow::File { index, .. } => Some(index),
            FileRow::Dir { .. } => None,
        })
        .filter(holds)
        .collect()
    }

    /// The selected file's place among the files that hold the find text, from 1, and
    /// their count. The place is None when this file does not hold it.
    pub fn find_file_place(&self) -> Option<(Option<usize>, usize)> {
        let order = self.find_order();
        if !matches!(self.find_files, Some(Ok(_))) {
            return None;
        }
        let file = self.selected_file();
        let place = order
            .iter()
            .position(|index| Some(*index) == file)
            .map(|at| at + 1);
        Some((place, order.len()))
    }

    /// The find's hit in the diff on screen, if the last find ran in it.
    pub fn current_find_hit(&self) -> Option<&FindHit> {
        let hit = self.find_hit.as_ref()?;
        (Some(&hit.key) == self.current_diff_key().as_ref()).then_some(hit)
    }

    pub fn loaded_activity(&self) -> Option<&PrActivity> {
        self.activity.as_ref()?.as_ref().ok()
    }

    fn loaded(&self) -> Option<&PrDetail> {
        self.detail.as_ref()?.as_ref().ok()
    }

    pub fn loaded_files(&self) -> Option<&Vec<ChangedFile>> {
        self.files.as_ref()?.as_ref().ok()
    }

    /// Indexes of the files the list shows, in GitHub's order.
    pub fn visible_files(&self) -> Vec<usize> {
        let Some(files) = self.loaded_files() else {
            return Vec::new();
        };
        let shown = |file: &ChangedFile| !(self.hide_viewed && file.viewed == ViewedState::Viewed);
        let visible = files.iter().enumerate().filter(|(_, file)| shown(file));
        visible.map(|(index, _)| index).collect()
    }

    /// The Files changed list as shown: folder and file rows in tree view, files in flat.
    pub fn file_rows(&self) -> Vec<FileRow> {
        let Some(files) = self.loaded_files() else {
            return Vec::new();
        };
        file_tree::rows(
            files,
            &self.visible_files(),
            self.tree_view,
            &self.folded_dirs,
        )
    }

    /// The file under the list cursor. None on a folder row.
    pub fn selected_file(&self) -> Option<usize> {
        match self.file_rows().get(self.file_state.selected()?)? {
            FileRow::File { index, .. } => Some(*index),
            FileRow::Dir { .. } => None,
        }
    }

    /// Puts the list cursor on the given file, or on the first file when it is not listed.
    fn select_file_row(&mut self, file: Option<usize>) {
        let rows = self.file_rows();
        let is_file = |row: &FileRow| matches!(row, FileRow::File { .. });
        let target = rows
            .iter()
            .position(|row| matches!(row, FileRow::File { index, .. } if Some(*index) == file))
            .or_else(|| rows.iter().position(is_file));
        self.file_state.select(Some(target.unwrap_or(0)));
    }

    /// Enter on a folder row folds or opens it. Returns false on a file row.
    fn toggle_folder(&mut self) -> bool {
        let rows = self.file_rows();
        let Some(FileRow::Dir { path, .. }) =
            self.file_state.selected().and_then(|row| rows.get(row))
        else {
            return false;
        };
        if !self.folded_dirs.remove(path) {
            self.folded_dirs.insert(path.clone());
        }
        true
    }

    fn show_tab(&mut self, tab: Tab) {
        self.tab = tab;
        self.reset_diff_position();
    }

    /// Flips the selected file's Viewed state at once; the caller syncs it to GitHub.
    /// Marking a file viewed moves on to the next file, as GitHub's web view does.
    fn toggle_viewed(&mut self) -> Option<Action> {
        let pull_request_id = self.loaded()?.id.clone();
        let index = self.selected_file()?;
        let file = self.files.as_mut()?.as_mut().ok()?.get_mut(index)?;
        let viewed = file.viewed != ViewedState::Viewed;
        file.viewed = if viewed {
            ViewedState::Viewed
        } else {
            ViewedState::Unviewed
        };
        let path = file.path.clone();
        if viewed && !self.hide_viewed {
            self.move_in_tab(1);
        }
        self.clamp_file_selection();
        Some(Action::SetViewed {
            pr: self.pr.clone(),
            pull_request_id,
            path,
            viewed,
        })
    }

    fn clamp_file_selection(&mut self) {
        let last = self.file_rows().len().saturating_sub(1);
        let current = self.file_state.selected().unwrap_or(0);
        self.file_state.select(Some(current.min(last)));
    }

    /// The page `o` and `Enter` open: the selected commit or check, else the PR.
    fn link(&self) -> String {
        let selected = |state: &ListState| state.selected().unwrap_or(0);
        let item_url = self.loaded_activity().and_then(|activity| match self.tab {
            Tab::Overview => None,
            Tab::Commits => activity
                .commits
                .get(selected(&self.commits))
                .map(|commit| commit.url.clone()),
            Tab::Checks => activity
                .checks
                .get(selected(&self.checks))
                .and_then(|check| check.url.clone()),
            Tab::Files => Some(format!("{}/files", self.pr.url())),
        });
        item_url.unwrap_or_else(|| self.pr.url())
    }

    fn list_len(&self) -> usize {
        let activity = self.loaded_activity();
        match self.tab {
            Tab::Overview => 0,
            Tab::Commits => activity.map_or(0, |activity| activity.commits.len()),
            Tab::Checks => activity.map_or(0, |activity| activity.checks.len()),
            Tab::Files => self.file_rows().len(),
        }
    }

    /// Opens or closes the `<details>` block under the Overview cursor.
    /// Enter or a click on an Overview row: run the row's action, or open or close its
    /// fold.
    fn press_row(&mut self) {
        let row = self
            .overview
            .as_ref()
            .and_then(|layout| layout.rows.get(self.cursor));
        match row.and_then(|row| row.action) {
            Some(crate::markdown::RowAction::ChecksTab) => self.show_tab(Tab::Checks),
            Some(crate::markdown::RowAction::UnresolvedComments) => {
                let filter = crate::overview::CommentFilter::Unresolved;
                self.set_comments(crate::overview::CommentView {
                    filter,
                    ..self.comments
                });
            }
            None => self.toggle_fold(),
        }
    }

    fn toggle_fold(&mut self) {
        let row = self
            .overview
            .as_ref()
            .and_then(|layout| layout.rows.get(self.cursor));
        let Some(id) = row.and_then(|row| row.fold) else {
            return;
        };
        if !self.toggled_folds.remove(&id) {
            self.toggled_folds.insert(id);
        }
        self.overview = None;
    }

    /// Moves the Overview cursor to the next or previous card or fold.
    fn jump_anchor(&mut self, forward: bool) {
        if let Some(target) = self.anchor_target(forward) {
            self.cursor = target;
            // A jump lands a third of the way down, with the text after it in view.
            self.scroll = self.clamp_scroll(target.saturating_sub(self.page / 3));
        }
    }

    fn click(&mut self, target: ClickTarget, row: u16) -> Result<Option<Action>, String> {
        match target.click {
            Click::Tab(tab) => self.show_tab(tab),
            Click::Page { first } => {
                let last = self
                    .overview
                    .as_ref()
                    .map_or(0, |layout| layout.rows.len().saturating_sub(1));
                self.cursor = (first + (row - target.area.y) as usize).min(last);
                self.press_row();
            }
            Click::Row(index) if !self.overlays.is_empty() => {
                if let Some(Overlay::Search { state, .. }) = self.overlays.last_mut() {
                    state.select(Some(index));
                }
                return self.overlay_key(KeyCode::Enter);
            }
            Click::Row(index) => {
                let state = match self.tab {
                    Tab::Files => &mut self.file_state,
                    Tab::Commits => &mut self.commits,
                    Tab::Checks => &mut self.checks,
                    Tab::Overview => return Ok(None),
                };
                state.select(Some(index));
                self.reset_diff_position();
                if self.tab == Tab::Files {
                    // The clicked row is on screen, so the list stays where the wheel left it.
                    if let Some(top) = self.file_list_top.take() {
                        *self.file_state.offset_mut() = top;
                    }
                    self.toggle_folder();
                }
            }
        }
        Ok(None)
    }

    /// Moves the cursor and the page together, so the text moves on every press. Only at
    /// the very top or bottom, where the page cannot move, does the cursor move alone.
    fn move_overview(&mut self, delta: isize) {
        let total = self.overview.as_ref().map_or(0, |layout| layout.rows.len());
        let cursor = self
            .cursor
            .saturating_add_signed(delta)
            .min(total.saturating_sub(1));
        let moved = cursor as isize - self.cursor as isize;
        self.cursor = cursor;
        self.scroll = self.clamp_scroll(self.scroll.saturating_add_signed(moved));
    }

    fn clamp_scroll(&self, scroll: usize) -> usize {
        let total = self.overview.as_ref().map_or(0, |layout| layout.rows.len());
        scroll.min(total.saturating_sub(self.page))
    }

    /// One wheel tick on the Overview: to the next card or fold, or, when that is more
    /// than a screen away, a third of a screen, so a long card can still be read.
    fn wheel_overview(&mut self, forward: bool) {
        let chunk = (self.page / 3).max(1);
        let distance = self
            .anchor_target(forward)
            .map(|target| target.abs_diff(self.cursor));
        match distance {
            Some(distance) if distance <= self.page => self.jump_anchor(forward),
            _ if forward => self.move_in_tab(chunk as isize),
            _ => self.move_in_tab(-(chunk as isize)),
        }
        self.skip_empty_rows(forward);
    }

    /// A chunk step can land between paragraphs; the cursor goes on to the next row with
    /// text, so it never rests on an empty line.
    fn skip_empty_rows(&mut self, forward: bool) {
        let Some(layout) = &self.overview else {
            return;
        };
        let has_text = |row: &crate::markdown::TextRow| {
            row.line
                .spans
                .iter()
                .any(|span| span.content.chars().any(|char| !" │─╰╯".contains(char)))
        };
        let find = |forward: bool| {
            if forward {
                (self.cursor..layout.rows.len()).find(|index| has_text(&layout.rows[*index]))
            } else {
                (0..=self.cursor)
                    .rev()
                    .find(|index| has_text(&layout.rows[*index]))
            }
        };
        if let Some(target) = find(forward) {
            self.move_overview(target as isize - self.cursor as isize);
        } else if let Some(target) = find(!forward) {
            // At an end of the page there is no text ahead. The cursor steps back to the
            // last text while the page stays put.
            self.cursor = target;
        }
    }

    /// Folds keep their IDs across filters, so what you opened stays open.
    fn set_comments(&mut self, comments: crate::overview::CommentView) {
        self.comments = comments;
        self.overview = None;
    }

    fn anchor_target(&self, forward: bool) -> Option<usize> {
        let layout = self.overview.as_ref()?;
        let mut anchors = layout
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.anchor)
            .map(|(index, _)| index);
        if forward {
            anchors.find(|index| *index > self.cursor)
        } else {
            anchors.rev().find(|index| *index < self.cursor)
        }
    }

    /// Moves to the next or previous file in the list that is not marked Viewed.
    fn jump_unviewed(&mut self, forward: bool) {
        let Some(files) = self.loaded_files() else {
            return;
        };
        let rows = self.file_rows();
        let current = self.file_state.selected().unwrap_or(0);
        let unviewed = |position: &usize| matches!(rows[*position], FileRow::File { index, .. } if files[index].viewed != ViewedState::Viewed);
        let target = if forward {
            (current + 1..rows.len()).find(unviewed)
        } else {
            (0..current).rev().find(unviewed)
        };
        if let Some(target) = target {
            self.file_state.select(Some(target));
            self.reset_diff_position();
        }
    }

    /// How far Space and b move: a screen on the Overview, 20 rows in a list.
    fn page_step(&self) -> isize {
        match self.tab {
            Tab::Overview => self.page.saturating_sub(2).max(1) as isize,
            _ => 20,
        }
    }

    fn move_in_tab(&mut self, delta: isize) {
        let last = self.list_len().saturating_sub(1);
        let state = match self.tab {
            Tab::Overview => return self.move_overview(delta),
            Tab::Commits => &mut self.commits,
            Tab::Checks => &mut self.checks,
            Tab::Files => {
                self.file_list_top = None;
                &mut self.file_state
            }
        };
        let next = state
            .selected()
            .unwrap_or(0)
            .saturating_add_signed(delta)
            .min(last);
        state.select(Some(next));
        self.reset_diff_position();
    }

    /// A new file or commit starts at its top left.
    fn reset_diff_position(&mut self) {
        self.diff_scroll = 0;
        self.diff_shift = 0;
    }

    /// `H` and `L` scroll an unwrapped diff sideways by a quarter of the pane. A wrapped
    /// diff has nothing past the edge, so the render keeps the shift at zero.
    fn shift_diff(&mut self, right: bool) {
        let step = (self.diff_width / 4).max(4);
        self.diff_shift = match right {
            true => self.diff_shift.saturating_add(step),
            false => self.diff_shift.saturating_sub(step),
        };
    }
}

pub enum Screen {
    Inbox,
    Detail(Box<DetailView>),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PromptKind {
    #[default]
    OpenLink,
    Workspace,
    SearchRepo,
    FindInDiff,
    /// Asks before cloning the PR's repo into this folder.
    ConfirmClone {
        pr: PrRef,
        dir: PathBuf,
    },
}

/// A one-line input box over the current screen.
#[derive(Debug, Default)]
pub struct Prompt {
    pub kind: PromptKind,
    pub text: String,
    pub error: Option<String>,
    /// Words that Tab completes the text to, the most likely first.
    pub words: Vec<String>,
}

impl Prompt {
    /// The rest of the first word that starts with the typed text, ignoring case.
    pub fn suggestion(&self) -> Option<String> {
        let typed = self.text.to_lowercase();
        if typed.is_empty() || typed.contains(char::is_whitespace) {
            return None;
        }
        let typed_chars = typed.chars().count();
        self.words
            .iter()
            .find(|word| {
                word.chars().count() > typed_chars && word.to_lowercase().starts_with(&typed)
            })
            .map(|word| word.chars().skip(typed_chars).collect())
    }
}

pub struct App {
    pub groups: Vec<Group>,
    pub list_state: ListState,
    pub screen: Screen,
    pub prompt: Option<Prompt>,
    pub loading: bool,
    pub error: Option<String>,
    pub fetched_at: Option<DateTime<Utc>>,
    /// The full key list shows over the screen; any key closes it.
    pub show_help: bool,
    /// What a mouse click can hit on the inbox, from the last draw.
    pub inbox_clicks: Vec<ClickTarget>,
    /// GitHub's API budget after the latest call.
    pub rate_limit: Option<crate::model::RateLimit>,
    /// The theme menu is open, with this entry of `THEME_NAMES` selected.
    pub theme_menu: Option<usize>,
    /// The theme in use, so the menu opens on it.
    pub theme_name: &'static str,
    /// One flag per group: true while its search is running.
    pub group_loading: Vec<bool>,
    inbox_generation: u64,
    /// Names of the configured diff renderers, in order; `m` cycles through them.
    pub renderer_names: Vec<String>,
    /// The view toggles a newly opened PR starts with; they follow the open PR.
    pub prefs: ViewPrefs,
}

/// Ctrl+L everywhere; Cmd+L only where the terminal reports the Super modifier.
fn is_open_link_shortcut(key: &KeyEvent) -> bool {
    let modifier = key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER);
    modifier && matches!(key.code, KeyCode::Char('l' | 'L'))
}

impl App {
    pub fn new() -> Self {
        Self {
            groups: Vec::new(),
            list_state: ListState::default().with_selected(Some(0)),
            screen: Screen::Inbox,
            prompt: None,
            loading: false,
            error: None,
            fetched_at: None,
            show_help: false,
            inbox_clicks: Vec::new(),
            rate_limit: None,
            theme_menu: None,
            theme_name: crate::theme::THEME_NAMES[0],
            group_loading: Vec::new(),
            inbox_generation: 0,
            renderer_names: Vec::new(),
            prefs: ViewPrefs::default(),
        }
    }

    /// Takes the open PR's view toggles as the ones to keep. Returns true when they
    /// changed, so the caller saves them.
    pub fn sync_prefs(&mut self) -> bool {
        let Screen::Detail(view) = &self.screen else {
            return false;
        };
        let prefs = ViewPrefs {
            newest_first: view.comments.newest_first,
            no_bots: view.comments.no_bots,
            diff_wrap: view.diff_wrap,
            renderer: self.renderer_names.get(view.renderer).cloned(),
            tree_view: view.tree_view,
            hide_viewed: view.hide_viewed,
            file_list_hidden: view.file_list_hidden,
        };
        if prefs == self.prefs {
            return false;
        }
        self.prefs = prefs;
        true
    }

    fn open_view(&mut self) -> Option<&mut DetailView> {
        match &mut self.screen {
            Screen::Detail(view) => Some(view),
            Screen::Inbox => None,
        }
    }

    /// The open PR, when its Files tab shows and its file list was never requested.
    pub fn take_files_request(&mut self) -> Option<PrRef> {
        let view = self.open_view()?;
        if view.tab != Tab::Files || view.files_requested {
            return None;
        }
        view.files_requested = true;
        Some(view.pr.clone())
    }

    /// The open PR, when a diff pane needs a local clone that nobody has looked for yet.
    pub fn take_repo_request(&mut self) -> Option<RepoJob> {
        if self.prompt.is_some() {
            return None;
        }
        let view = self.open_view()?;
        if !matches!(view.tab, Tab::Files | Tab::Commits) || view.repo != RepoState::Unknown {
            return None;
        }
        let detail = view.loaded()?;
        let job = RepoJob {
            pr: view.pr.clone(),
            base_ref: detail.base.clone(),
            base_oid: detail.base_oid.clone(),
            head_oid: detail.head_oid.clone(),
        };
        view.repo = RepoState::Busy("looking for a local clone".to_string());
        Some(job)
    }

    /// The diff the pane shows, when it is neither rendered nor being rendered.
    pub fn take_diff_request(&mut self) -> Option<DiffJob> {
        let view = self.open_view()?;
        let RepoState::Ready(repo) = &view.repo else {
            return None;
        };
        let key = view.current_diff_key()?;
        if view.diffs.contains_key(&key) {
            return None;
        }
        let job = diff_job(&view.pr, repo, view.loaded()?, key.clone());
        view.diffs.insert(key, DiffOutput::Pending);
        Some(job)
    }

    pub fn set_repo(&mut self, pr: &PrRef, result: Result<LocalRepo, String>) {
        if let Some(view) = self.view_for(pr) {
            view.repo = match result {
                Ok(repo) => RepoState::Ready(repo),
                Err(error) => RepoState::Failed(error),
            };
        }
    }

    /// Shows progress or a problem with the clone in the open PR's diff pane.
    pub fn set_repo_state(&mut self, state: RepoState) {
        if let Some(view) = self.open_view() {
            view.repo = state;
        }
    }

    /// Converts the renderer's ANSI colors into styled text once, when it arrives.
    pub fn set_diff(&mut self, pr: &PrRef, key: DiffKey, result: Result<Text<'static>, String>) {
        let Some(view) = self.view_for(pr) else {
            return;
        };
        view.diffs.insert(key, DiffOutput::Done(result));
    }

    /// Fills the search results that are still waiting for this query.
    /// Keeps the files that hold the find text, unless a newer find replaced it.
    pub fn set_find_files(
        &mut self,
        pr: &PrRef,
        query: &str,
        result: Result<Vec<(String, usize)>, String>,
    ) {
        let Some(view) = self.view_for(pr) else {
            return;
        };
        if view.find.as_deref() == Some(query) {
            view.find_files = Some(result);
        }
    }

    /// Keeps the words, and hands them to the find prompt if it is open.
    pub fn set_find_words(&mut self, pr: &PrRef, words: Vec<String>) {
        if let Some(prompt) = &mut self.prompt
            && prompt.kind == PromptKind::FindInDiff
        {
            prompt.words = words.clone();
        }
        if let Some(view) = self.view_for(pr) {
            view.find_words = Some(words);
        }
    }

    /// Runs a find that waited for a diff, now that messages may have brought it.
    pub fn settle_find(&mut self) {
        if let Some(view) = self.open_view() {
            view.settle_find();
        }
    }

    pub fn set_grep(&mut self, pr: &PrRef, query: &str, result: Result<Vec<GrepHit>, String>) {
        let Some(view) = self.view_for(pr) else {
            return;
        };
        for overlay in &mut view.overlays {
            match overlay {
                Overlay::Search {
                    query: shown,
                    hits: hits @ None,
                    state,
                } if shown == query => {
                    // Drawing the empty "searching" list cleared the selection.
                    state.select(Some(0));
                    *hits = Some(result);
                    return;
                }
                _ => {}
            }
        }
    }

    /// Fills the file views that are still waiting for this path.
    pub fn set_file(&mut self, pr: &PrRef, path: &str, lines: Result<Text<'static>, String>) {
        let Some(view) = self.view_for(pr) else {
            return;
        };
        for overlay in &mut view.overlays {
            match overlay {
                Overlay::File {
                    path: shown,
                    lines: slot @ None,
                    ..
                } if shown == path => {
                    *slot = Some(lines);
                    return;
                }
                _ => {}
            }
        }
    }

    pub fn ask(&mut self, kind: PromptKind, text: &str) {
        self.prompt = Some(Prompt {
            kind,
            text: text.to_string(),
            error: None,
            words: Vec::new(),
        });
    }

    pub fn set_prompt_error(&mut self, kind: PromptKind, text: &str, error: String) {
        self.prompt = Some(Prompt {
            kind,
            text: text.to_string(),
            error: Some(error),
            words: Vec::new(),
        });
    }

    pub fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for (group_index, group) in self.groups.iter().enumerate() {
            rows.push(Row::Header(group_index));
            if !group.collapsed {
                rows.extend((0..group.prs.len()).map(|pr_index| Row::Pr(group_index, pr_index)));
            }
        }
        rows
    }

    pub fn selected_row(&self) -> Option<Row> {
        self.rows().get(self.list_state.selected()?).copied()
    }

    fn selected_pr(&self) -> Option<&PrSummary> {
        match self.selected_row()? {
            Row::Pr(group_index, pr_index) => self.groups[group_index].prs.get(pr_index),
            Row::Header(_) => None,
        }
    }

    /// Starts a new fetch of every group. A group keeps its old rows until its new
    /// ones arrive. Returns the fetch's number, which `set_group` checks.
    pub fn start_inbox(&mut self, configs: &[GroupConfig]) -> u64 {
        let groups = configs.iter().map(|config| {
            let old = self.groups.iter().find(|group| group.title == config.title);
            old.cloned().unwrap_or_else(|| Group {
                title: config.title.clone(),
                total: 0,
                prs: Vec::new(),
                collapsed: config.collapsed,
            })
        });
        self.groups = groups.collect();
        self.group_loading = vec![true; configs.len()];
        self.loading = !configs.is_empty();
        self.error = None;
        self.clamp_selection();
        self.inbox_generation += 1;
        self.inbox_generation
    }

    /// Fills one group. A result from an older fetch is dropped. The selection stays
    /// on the same PR when rows above it change.
    pub fn set_group(
        &mut self,
        generation: u64,
        index: usize,
        result: Result<Group, String>,
        fetched_at: DateTime<Utc>,
    ) {
        if generation != self.inbox_generation || index >= self.groups.len() {
            return;
        }
        let selected = self.selected_pr().map(PrSummary::reference);
        self.group_loading[index] = false;
        match result {
            Ok(mut group) => {
                // Keep the fold state the user chose.
                group.collapsed = self.groups[index].collapsed;
                self.groups[index] = group;
            }
            Err(error) => self.error = Some(format!("{}: {error}", self.groups[index].title)),
        }
        if !self.group_loading.contains(&true) {
            self.loading = false;
            self.fetched_at = Some(fetched_at);
        }
        self.reselect(selected);
    }

    fn reselect(&mut self, pr: Option<PrRef>) {
        let rows = self.rows();
        let position = pr.and_then(|pr| {
            rows.iter().position(|row| match row {
                Row::Pr(group, index) => self.groups[*group].prs[*index].reference() == pr,
                Row::Header(_) => false,
            })
        });
        if let Some(position) = position {
            self.list_state.select(Some(position));
        }
        self.clamp_selection();
    }

    pub fn set_inbox_error(&mut self, message: String) {
        self.loading = false;
        self.error = Some(message);
    }

    /// The open PR's view, unless the user has already left that PR.
    /// Drops everything drawn in the old theme's colors: the diffs, the Overview and an
    /// open file view. The next draw builds them again in the new colors.
    pub fn forget_colors(&mut self) {
        if let Screen::Detail(view) = &mut self.screen {
            view.diffs.clear();
            view.overview = None;
            view.overlays
                .retain(|overlay| !matches!(overlay, Overlay::File { .. }));
        }
    }

    fn view_for(&mut self, pr: &PrRef) -> Option<&mut DetailView> {
        match &mut self.screen {
            Screen::Detail(view) if view.pr == *pr => Some(view),
            _ => None,
        }
    }

    pub fn set_detail(&mut self, pr: &PrRef, result: Result<PrDetail, String>) {
        if let Some(view) = self.view_for(pr) {
            view.detail = Some(result);
            view.overview = None;
        }
    }

    pub fn set_activity(&mut self, pr: &PrRef, result: Result<PrActivity, String>) {
        if let Some(view) = self.view_for(pr) {
            view.activity = Some(result);
            view.overview = None;
        }
    }

    pub fn set_files_progress(&mut self, pr: &PrRef, loaded: usize, total: usize) {
        if let Some(view) = self.view_for(pr) {
            view.files_progress = Some((loaded, total));
        }
    }

    pub fn set_files(&mut self, pr: &PrRef, result: Result<Vec<ChangedFile>, String>) {
        if let Some(view) = self.view_for(pr) {
            view.files = Some(result);
            // The tree starts on a folder row; the cursor goes to the first file.
            view.select_file_row(None);
        }
    }

    /// A failed sync puts the checkbox back, so the screen never claims a state GitHub lacks.
    pub fn set_viewed_result(
        &mut self,
        pr: &PrRef,
        path: &str,
        viewed: bool,
        result: Result<(), String>,
    ) {
        let Err(error) = result else {
            return;
        };
        self.error = Some(format!("Viewed not saved for {path}: {error}"));
        let Some(view) = self.view_for(pr) else {
            return;
        };
        let files = view.files.as_mut().and_then(|files| files.as_mut().ok());
        let file = files.and_then(|files| files.iter_mut().find(|file| file.path == path));
        if let Some(file) = file {
            file.viewed = if viewed {
                ViewedState::Unviewed
            } else {
                ViewedState::Viewed
            };
        }
    }

    /// A mouse wheel tick at `column`: positive scrolls down. It moves what the pointer is
    /// over, so the wheel over a diff scrolls the diff, not the list beside it.
    pub fn handle_scroll(&mut self, column: u16, delta: isize) {
        if self.prompt.is_some() || self.show_help {
            return;
        }
        let Screen::Detail(view) = &mut self.screen else {
            self.move_selection(delta);
            return;
        };
        let diff_pane = matches!(view.tab, Tab::Files | Tab::Commits);
        if !view.overlays.is_empty() {
            let key = if delta > 0 {
                KeyCode::Down
            } else {
                KeyCode::Up
            };
            for _ in 0..3 {
                self.error = view.overlay_key(key).err();
            }
        } else if diff_pane && column >= view.diff_left {
            view.wheel_diff(delta > 0);
        } else if view.tab == Tab::Overview {
            view.wheel_overview(delta > 0);
        } else if view.tab == Tab::Files {
            view.wheel_file_list(delta);
        } else {
            view.move_in_tab(delta);
        }
    }

    /// A left click at a screen cell. It does what the keys would do on that item: open a
    /// pull request, switch a tab, select a row, or open or close a fold.
    pub fn handle_click(&mut self, column: u16, row: u16) -> Option<Action> {
        if self.prompt.is_some() {
            return None;
        }
        if self.show_help {
            self.show_help = false;
            return None;
        }
        let hit = |targets: &[ClickTarget]| {
            let position = ratatui::layout::Position::new(column, row);
            targets
                .iter()
                .rev()
                .find(|target| target.area.contains(position))
                .copied()
        };
        let Screen::Detail(view) = &mut self.screen else {
            let target = hit(&self.inbox_clicks)?;
            let Click::Row(index) = target.click else {
                return None;
            };
            self.list_state.select(Some(index));
            return self.open_selected();
        };
        let target = hit(&view.clicks)?;
        let result = view.click(target, row);
        self.report(result)
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        if self.prompt.is_some() {
            return self.prompt_key(key.code);
        }
        if self.show_help {
            self.show_help = false;
            return None;
        }
        if self.theme_menu.is_some() {
            return self.theme_menu_key(key.code);
        }
        if key.code == KeyCode::Char('T') {
            let names = crate::theme::THEME_NAMES;
            self.theme_menu = names
                .iter()
                .position(|name| *name == self.theme_name)
                .or(Some(0));
            return None;
        }
        if key.code == KeyCode::Char('?') {
            self.show_help = true;
            return None;
        }
        if is_open_link_shortcut(&key) {
            self.prompt = Some(Prompt::default());
            return None;
        }
        match self.screen {
            Screen::Inbox => self.inbox_key(key.code),
            Screen::Detail(_) => self.detail_key(key.code),
        }
    }

    fn theme_menu_key(&mut self, key: KeyCode) -> Option<Action> {
        let names = crate::theme::THEME_NAMES;
        let selected = self.theme_menu?;
        match key {
            KeyCode::Char('j') | KeyCode::Down => {
                self.theme_menu = Some((selected + 1) % names.len())
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.theme_menu = Some((selected + names.len() - 1) % names.len());
            }
            KeyCode::Enter => {
                self.theme_menu = None;
                return Some(Action::SetTheme(names[selected]));
            }
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('T') => self.theme_menu = None,
            _ => {}
        }
        None
    }

    /// A paste goes into the open box, or opens the link box when none is open.
    pub fn handle_paste(&mut self, text: &str) {
        let prompt = self.prompt.get_or_insert_with(Prompt::default);
        prompt.text.push_str(text.trim());
        prompt.error = None;
    }

    fn prompt_key(&mut self, key: KeyCode) -> Option<Action> {
        let prompt = self.prompt.as_mut()?;
        match key {
            // Escaping a clone question is the same as answering no.
            KeyCode::Esc if matches!(prompt.kind, PromptKind::ConfirmClone { .. }) => {
                prompt.text.clear();
                return self.submit_prompt();
            }
            KeyCode::Esc if prompt.kind == PromptKind::Workspace => {
                self.prompt = None;
                let message = "no workspace folder, so no diff here. r asks again.";
                self.set_repo_state(RepoState::Failed(message.to_string()));
            }
            KeyCode::Esc => self.prompt = None,
            KeyCode::Backspace => {
                prompt.text.pop();
            }
            KeyCode::Tab => {
                if let Some(rest) = prompt.suggestion() {
                    prompt.text.push_str(&rest);
                }
            }
            KeyCode::Char(character) => prompt.text.push(character),
            KeyCode::Enter => return self.submit_prompt(),
            _ => {}
        }
        None
    }

    fn submit_prompt(&mut self) -> Option<Action> {
        let prompt = self.prompt.take()?;
        match prompt.kind {
            PromptKind::OpenLink => self.submit_link(prompt),
            PromptKind::Workspace => Some(Action::SetWorkspace(prompt.text.trim().to_string())),
            PromptKind::ConfirmClone { pr, dir } => self.submit_clone(pr, dir, &prompt.text),
            PromptKind::SearchRepo => self.in_view(|view| view.search(prompt.text).map(Some)),
            PromptKind::FindInDiff => self.in_view(|view| view.start_find(&prompt.text)),
        }
    }

    /// Shows a failed step's error in the footer.
    fn report(&mut self, result: Result<Option<Action>, String>) -> Option<Action> {
        result.unwrap_or_else(|error| {
            self.error = Some(error);
            None
        })
    }

    /// Runs a step on the open PR, and shows its error in the footer.
    fn in_view(
        &mut self,
        step: impl FnOnce(&mut DetailView) -> Result<Option<Action>, String>,
    ) -> Option<Action> {
        let view = self.open_view()?;
        let result = step(view);
        self.report(result)
    }

    fn submit_clone(&mut self, pr: PrRef, dir: PathBuf, answer: &str) -> Option<Action> {
        if answer.trim().eq_ignore_ascii_case("y") {
            return Some(Action::CloneRepo { pr, dir });
        }
        let declined =
            "no local clone, so no diff here. d still opens the whole diff; r asks again.";
        self.set_repo(&pr, Err(declined.to_string()));
        None
    }

    fn submit_link(&mut self, mut prompt: Prompt) -> Option<Action> {
        let Some(pr) = parse_pr_ref(&prompt.text) else {
            prompt.error = Some("not a pull request link or owner/repo#number".to_string());
            self.prompt = Some(prompt);
            return None;
        };
        Some(self.open_pr(pr))
    }

    fn open_pr(&mut self, pr: PrRef) -> Action {
        self.error = None;
        let view = DetailView::new(pr.clone(), &self.prefs, &self.renderer_names);
        self.screen = Screen::Detail(Box::new(view));
        Action::FetchDetail(pr)
    }

    fn inbox_key(&mut self, key: KeyCode) -> Option<Action> {
        match key {
            KeyCode::Char('q') => return Some(Action::Quit),
            KeyCode::Char('r') => return Some(Action::Refresh),
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Char('g') | KeyCode::Home => self.list_state.select(Some(0)),
            KeyCode::Char('G') | KeyCode::End => self.select_last(),
            KeyCode::Char(' ') => self.toggle_selected_group(),
            KeyCode::Tab => self.jump_group(true),
            KeyCode::BackTab => self.jump_group(false),
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => return self.open_selected(),
            KeyCode::Char('o') => {
                return self
                    .selected_pr()
                    .map(|pr| Action::OpenBrowser(pr.url.clone()));
            }
            KeyCode::Char('d') => {
                return self.selected_pr().map(|pr| Action::RunDiff(pr.reference()));
            }
            _ => {}
        }
        None
    }

    fn detail_key(&mut self, key: KeyCode) -> Option<Action> {
        let renderer_count = self.renderer_names.len().max(1);
        let Screen::Detail(view) = &mut self.screen else {
            return None;
        };
        let diff_pane = matches!(view.tab, Tab::Files | Tab::Commits);
        // An error stays in the footer until the next key.
        self.error = None;
        if !view.overlays.is_empty() {
            let result = view.overlay_key(key);
            return self.report(result);
        }
        let find_result = match key {
            KeyCode::Char('n') if diff_pane => Some(view.find_next(true)),
            KeyCode::Char('N') if diff_pane => Some(view.find_next(false)),
            _ => None,
        };
        if let Some(result) = find_result {
            self.error = result.err();
            return None;
        }
        match key {
            KeyCode::Char('s') => {
                self.prompt = Some(Prompt {
                    kind: PromptKind::SearchRepo,
                    ..Prompt::default()
                });
            }
            KeyCode::Char('/') if diff_pane => {
                self.prompt = Some(Prompt {
                    kind: PromptKind::FindInDiff,
                    words: view.find_words.clone().unwrap_or_default(),
                    ..Prompt::default()
                });
                return view.words_request();
            }
            KeyCode::Enter if view.tab == Tab::Files && view.toggle_folder() => {
                view.clamp_file_selection()
            }
            KeyCode::Char('t') if view.tab == Tab::Files => {
                let file = view.selected_file();
                view.tree_view = !view.tree_view;
                view.select_file_row(file);
            }
            KeyCode::Char('z') if view.tab == Tab::Files => {
                view.file_list_hidden = !view.file_list_hidden
            }
            KeyCode::Enter if view.tab == Tab::Files => {
                let result = view.open_whole_diff().map(Some);
                return self.report(result);
            }
            KeyCode::Char('w') if diff_pane => {
                view.diff_wrap = !view.diff_wrap;
                view.diff_shift = 0;
            }
            KeyCode::Char('H') if diff_pane => view.shift_diff(false),
            KeyCode::Char('L') if diff_pane => view.shift_diff(true),
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('h') | KeyCode::Left => {
                self.screen = Screen::Inbox;
                self.error = None;
            }
            KeyCode::Tab => view.show_tab(view.tab.shifted(1)),
            KeyCode::BackTab => view.show_tab(view.tab.shifted(-1)),
            KeyCode::Char(digit @ '1'..='9') => {
                let index = digit as usize - '1' as usize;
                view.show_tab(Tab::ALL.get(index).copied().unwrap_or(view.tab));
            }
            KeyCode::Char('m') if diff_pane => {
                view.renderer = (view.renderer + 1) % renderer_count;
                view.diff_scroll = 0;
            }
            KeyCode::Char('J') if diff_pane => view.scroll_diff(3),
            KeyCode::Char('K') if diff_pane => view.scroll_diff(-3),
            KeyCode::Char(' ') | KeyCode::PageDown if diff_pane => view.scroll_diff(20),
            KeyCode::Char('b') | KeyCode::PageUp if diff_pane => view.scroll_diff(-20),
            KeyCode::Char('v') if view.tab == Tab::Files => return view.toggle_viewed(),
            KeyCode::Char('V') if view.tab == Tab::Files => {
                view.hide_viewed = !view.hide_viewed;
                view.clamp_file_selection();
            }
            KeyCode::Char('j') | KeyCode::Down if view.tab == Tab::Overview => {
                view.wheel_overview(true)
            }
            KeyCode::Char('k') | KeyCode::Up if view.tab == Tab::Overview => {
                view.wheel_overview(false)
            }
            KeyCode::Char('n') if view.tab == Tab::Overview => {
                let no_bots = !view.comments.no_bots;
                view.set_comments(crate::overview::CommentView {
                    no_bots,
                    ..view.comments
                });
            }
            KeyCode::Char('j') | KeyCode::Down => view.move_in_tab(1),
            KeyCode::Char('k') | KeyCode::Up => view.move_in_tab(-1),
            KeyCode::Char(' ') | KeyCode::PageDown => view.move_in_tab(view.page_step()),
            KeyCode::Char('b') | KeyCode::PageUp => view.move_in_tab(-view.page_step()),
            KeyCode::Char('g') | KeyCode::Home => view.move_in_tab(-1_000_000),
            KeyCode::Char('G') | KeyCode::End => view.move_in_tab(1_000_000),
            KeyCode::Enter if view.tab == Tab::Overview => view.press_row(),
            KeyCode::Char(']') if view.tab == Tab::Overview => view.jump_anchor(true),
            KeyCode::Char('f') if view.tab == Tab::Overview => {
                let filter = view.comments.filter.next();
                view.set_comments(crate::overview::CommentView {
                    filter,
                    ..view.comments
                });
            }
            KeyCode::Char('F') if view.tab == Tab::Overview => {
                let newest_first = !view.comments.newest_first;
                view.set_comments(crate::overview::CommentView {
                    newest_first,
                    ..view.comments
                });
            }
            KeyCode::Char('[') if view.tab == Tab::Overview => view.jump_anchor(false),
            KeyCode::Char(']') if view.tab == Tab::Files => view.jump_unviewed(true),
            KeyCode::Char('[') if view.tab == Tab::Files => view.jump_unviewed(false),
            KeyCode::Char('o') => return Some(Action::OpenBrowser(view.link())),
            KeyCode::Enter if matches!(view.tab, Tab::Commits | Tab::Checks) => {
                return Some(Action::OpenBrowser(view.link()));
            }
            KeyCode::Char('d') => return Some(Action::RunDiff(view.pr.clone())),
            KeyCode::Char('r') => {
                return Some(Self::reload(view, &self.prefs, &self.renderer_names));
            }
            _ => {}
        }
        None
    }

    /// Starts the PR over on the same tab. The file list and the clone are looked up
    /// again when a tab needs them.
    fn reload(view: &mut DetailView, prefs: &ViewPrefs, renderer_names: &[String]) -> Action {
        let tab = view.tab;
        let renderer = view.renderer;
        *view = DetailView::new(view.pr.clone(), prefs, renderer_names);
        view.tab = tab;
        view.renderer = renderer;
        Action::FetchDetail(view.pr.clone())
    }

    /// Moves to the next or previous group header.
    fn jump_group(&mut self, forward: bool) {
        let rows = self.rows();
        let current = self.list_state.selected().unwrap_or(0);
        let headers: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| matches!(row, Row::Header(_)))
            .map(|(index, _)| index)
            .collect();
        // Past the last group it goes round to the first, and back from the first to the last.
        let target = if forward {
            headers
                .iter()
                .find(|index| **index > current)
                .or(headers.first())
        } else {
            headers
                .iter()
                .rev()
                .find(|index| **index < current)
                .or(headers.last())
        };
        if let Some(target) = target {
            self.list_state.select(Some(*target));
        }
    }

    fn open_selected(&mut self) -> Option<Action> {
        if let Some(Row::Header(_)) = self.selected_row() {
            self.toggle_selected_group();
            return None;
        }
        let pr = self.selected_pr()?.reference();
        Some(self.open_pr(pr))
    }

    /// On a PR row, folds its group and moves the selection to the group header.
    fn toggle_selected_group(&mut self) {
        let group_index = match self.selected_row() {
            Some(Row::Header(index) | Row::Pr(index, _)) => index,
            None => return,
        };
        self.groups[group_index].collapsed = !self.groups[group_index].collapsed;
        let header = self
            .rows()
            .iter()
            .position(|row| *row == Row::Header(group_index));
        self.list_state.select(header);
    }

    fn move_selection(&mut self, delta: isize) {
        let last = self.rows().len().saturating_sub(1);
        let current = self.list_state.selected().unwrap_or(0);
        let next = current.saturating_add_signed(delta).min(last);
        self.list_state.select(Some(next));
    }

    fn select_last(&mut self) {
        self.list_state
            .select(Some(self.rows().len().saturating_sub(1)));
    }

    fn clamp_selection(&mut self) {
        let last = self.rows().len().saturating_sub(1);
        let current = self.list_state.selected().unwrap_or(0);
        self.list_state.select(Some(current.min(last)));
    }
}

/// What the renderer needs to draw `key`: the clone, the commits and the paths.
fn diff_job(pr: &PrRef, repo: &LocalRepo, detail: &PrDetail, key: DiffKey) -> DiffJob {
    let (base, head, paths) = match &key.target {
        DiffTarget::File(path) => {
            let old = repo.renames.get(path).cloned();
            let paths = old.into_iter().chain([path.clone()]).collect();
            (detail.base_oid.clone(), detail.head_oid.clone(), paths)
        }
        DiffTarget::Commit(oid) => (format!("{oid}~1"), oid.clone(), Vec::new()),
    };
    DiffJob {
        pr: pr.clone(),
        key,
        dir: repo.dir.clone(),
        base,
        head,
        paths,
    }
}

/// git's "@@ -1,3 +1,4 @@", delta's box top "─────┐", or difftastic's "a.md --- 2/5 --- Text".
fn is_hunk_start(line: &str) -> bool {
    let line = line.trim_end();
    let delta_box =
        line.ends_with('┐') && line.trim_end_matches('┐').chars().all(|char| char == '─');
    let difftastic = line.split(" --- ").nth(1).is_some_and(|part| {
        part.split_once('/')
            .is_some_and(|(at, of)| at.parse::<u32>().is_ok() && of.parse::<u32>().is_ok())
    });
    line.starts_with("@@") || delta_box || difftastic
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ReviewState;
    use crossterm::event::KeyEventKind;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn with_modifier(character: char, modifiers: KeyModifiers) -> KeyEvent {
        let mut event = KeyEvent::new(KeyCode::Char(character), modifiers);
        event.kind = KeyEventKind::Press;
        event
    }

    fn pr(number: u64) -> PrSummary {
        PrSummary {
            repo: "acme/app".to_string(),
            number,
            title: format!("PR {number}"),
            url: format!("https://github.com/acme/app/pull/{number}"),
            author: "octo".to_string(),
            updated_at: Utc::now(),
            is_draft: false,
            review: ReviewState::Awaiting,
            checks: None,
            comments: 0,
        }
    }

    fn group(title: &str, numbers: &[u64], collapsed: bool) -> Group {
        Group {
            title: title.to_string(),
            total: numbers.len() as u64,
            prs: numbers.iter().copied().map(pr).collect(),
            collapsed,
        }
    }

    fn app_with(groups: Vec<Group>) -> App {
        let mut app = App::new();
        app.groups = groups;
        app
    }

    #[test]
    fn collapsed_groups_show_only_their_header() {
        let app = app_with(vec![group("A", &[1, 2], false), group("B", &[3], true)]);
        assert_eq!(
            app.rows(),
            [Row::Header(0), Row::Pr(0, 0), Row::Pr(0, 1), Row::Header(1)]
        );
    }

    #[test]
    fn selection_stops_at_both_ends() {
        let mut app = app_with(vec![group("A", &[1], false)]);
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.list_state.selected(), Some(0));
        app.handle_key(key(KeyCode::Char('j')));
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.list_state.selected(), Some(1));
    }

    #[test]
    fn enter_on_a_header_folds_the_group() {
        let mut app = app_with(vec![group("A", &[1], false)]);
        assert!(app.handle_key(key(KeyCode::Enter)).is_none());
        assert!(app.groups[0].collapsed);
    }

    #[test]
    fn folding_from_a_pr_row_selects_its_header() {
        let mut app = app_with(vec![group("A", &[1], false), group("B", &[2, 3], false)]);
        app.list_state.select(Some(4)); // B's second PR
        app.handle_key(key(KeyCode::Char(' ')));
        assert!(app.groups[1].collapsed);
        assert_eq!(app.selected_row(), Some(Row::Header(1)));
    }

    #[test]
    fn enter_on_a_pr_opens_detail_and_requests_it() {
        let mut app = app_with(vec![group("A", &[7], false)]);
        app.list_state.select(Some(1));
        let action = app.handle_key(key(KeyCode::Enter));
        assert!(matches!(action, Some(Action::FetchDetail(ref pr)) if pr.number == 7));
        let Screen::Detail(view) = &app.screen else {
            panic!("expected detail screen")
        };
        assert_eq!(view.tab, Tab::Overview);
        assert!(matches!(app.screen, Screen::Detail(_)));
        app.handle_key(key(KeyCode::Esc));
        assert!(matches!(app.screen, Screen::Inbox));
    }

    #[test]
    fn a_late_detail_for_another_pr_is_ignored() {
        let mut app = app_with(vec![group("A", &[7], false)]);
        app.list_state.select(Some(1));
        app.handle_key(key(KeyCode::Enter));
        let other = PrRef {
            repo: "acme/app".to_string(),
            number: 8,
        };
        app.set_detail(&other, Err("boom".to_string()));
        let Screen::Detail(view) = &app.screen else {
            panic!("expected detail screen")
        };
        assert!(view.detail.is_none());
    }

    #[test]
    fn refresh_keeps_fold_state_and_clamps_selection() {
        let mut app = app_with(vec![group("A", &[1, 2, 3], true)]);
        app.groups[0].collapsed = false;
        app.list_state.select(Some(3));
        let configs = [GroupConfig {
            title: "A".to_string(),
            query: String::new(),
            collapsed: true,
        }];
        let generation = app.start_inbox(&configs);
        app.set_group(generation, 0, Ok(group("A", &[1], true)), Utc::now());
        assert!(!app.groups[0].collapsed);
        assert_eq!(app.list_state.selected(), Some(1));
    }

    #[test]
    fn groups_fill_in_any_order_and_an_old_fetch_is_dropped() {
        let mut app = App::new();
        let configs: Vec<GroupConfig> = ["A", "B"]
            .iter()
            .map(|title| GroupConfig {
                title: title.to_string(),
                query: String::new(),
                collapsed: false,
            })
            .collect();
        let old = app.start_inbox(&configs);
        let current = app.start_inbox(&configs);
        app.set_group(old, 0, Ok(group("A", &[99], false)), Utc::now());
        assert!(
            app.groups[0].prs.is_empty(),
            "a result from the older fetch was used"
        );
        app.set_group(current, 1, Ok(group("B", &[2], false)), Utc::now());
        assert!(app.loading, "group A is still loading");
        assert_eq!(app.group_loading, [true, false]);
        app.set_group(current, 0, Err("timeout".to_string()), Utc::now());
        assert!(!app.loading);
        assert_eq!(app.error.as_deref(), Some("A: timeout"));
    }

    #[test]
    fn a_group_arriving_above_keeps_the_selected_pr() {
        let mut app = app_with(vec![group("A", &[1], false), group("B", &[5, 6], false)]);
        let configs: Vec<GroupConfig> = ["A", "B"]
            .iter()
            .map(|title| GroupConfig {
                title: title.to_string(),
                query: String::new(),
                collapsed: false,
            })
            .collect();
        app.list_state.select(Some(4));
        assert_eq!(app.selected_pr().unwrap().number, 6);
        let generation = app.start_inbox(&configs);
        app.set_group(generation, 0, Ok(group("A", &[1, 2, 3], false)), Utc::now());
        assert_eq!(app.selected_pr().unwrap().number, 6);
    }

    #[test]
    fn question_mark_shows_the_key_list_and_any_key_closes_it() {
        let mut app = app_with(vec![group("A", &[1], false)]);
        app.handle_key(key(KeyCode::Char('?')));
        assert!(app.show_help);
        assert!(
            app.handle_key(key(KeyCode::Char('q'))).is_none(),
            "q closes the list, it does not quit"
        );
        assert!(!app.show_help);
    }

    #[test]
    fn the_wheel_scrolls_the_pane_under_the_pointer() {
        let files = vec![
            file("a.md", ViewedState::Unviewed),
            file("b.md", ViewedState::Unviewed),
        ];
        let (mut app, _) = files_tab(files);
        if let Screen::Detail(view) = &mut app.screen {
            view.diff_left = 40;
        }
        app.handle_scroll(60, 1);
        assert_eq!(
            view(&app).selected_file(),
            Some(0),
            "the wheel over the diff keeps the file"
        );
        assert_eq!(view(&app).diff_scroll, 3);
        let names: Vec<String> = (0..10).map(|index| format!("f{index}.md")).collect();
        let many = names
            .iter()
            .map(|name| file(name, ViewedState::Unviewed))
            .collect();
        let (mut app, _) = files_tab(many);
        if let Screen::Detail(view) = &mut app.screen {
            view.diff_left = 40;
            view.file_list_height = 4;
        }
        app.handle_scroll(10, 1);
        assert_eq!(
            view(&app).selected_file(),
            Some(0),
            "the wheel over the list keeps the file"
        );
        assert_eq!(view(&app).file_list_top, Some(3));
        app.handle_scroll(10, 1);
        app.handle_scroll(10, 1);
        assert_eq!(
            view(&app).file_list_top,
            Some(6),
            "it stops with the last row at the bottom"
        );
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(
            view(&app).file_list_top,
            None,
            "a key brings the list back to the file"
        );
        assert_eq!(view(&app).selected_file(), Some(1));
    }

    #[test]
    fn shift_h_and_l_scroll_the_diff_sideways_and_h_still_goes_back() {
        let (mut app, _) = files_tab(vec![file("a.md", ViewedState::Unviewed)]);
        if let Screen::Detail(view) = &mut app.screen {
            view.diff_width = 80;
        }
        assert!(view(&app).diff_wrap, "long lines wrap by default");
        app.handle_key(key(KeyCode::Char('w')));
        app.handle_key(key(KeyCode::Char('L')));
        app.handle_key(key(KeyCode::Char('L')));
        assert_eq!(view(&app).diff_shift, 40);
        app.handle_key(key(KeyCode::Char('H')));
        assert_eq!(view(&app).diff_shift, 20);
        assert!(matches!(app.screen, Screen::Detail(_)));
        app.handle_key(key(KeyCode::Char('w')));
        assert_eq!(view(&app).diff_shift, 0);
        app.handle_key(key(KeyCode::Char('h')));
        assert!(matches!(app.screen, Screen::Inbox), "wrapped, h goes back");
    }

    #[test]
    fn the_wheel_on_the_overview_jumps_cards_and_reads_long_ones_by_chunk() {
        use crate::markdown::TextRow;
        let (mut app, _) = files_tab(Vec::new());
        let Screen::Detail(view) = &mut app.screen else {
            unreachable!()
        };
        let mut rows: Vec<TextRow> = (0..120)
            .map(|_| TextRow::plain(ratatui::text::Line::from("text")))
            .collect();
        for anchor in [0, 5, 100] {
            rows[anchor] = TextRow::anchor(ratatui::text::Line::from("card"));
        }
        view.tab = Tab::Overview;
        view.overview = Some(OverviewLayout { width: 60, rows });
        view.page = 30;
        let mut cursors = Vec::new();
        for _ in 0..4 {
            app.handle_scroll(1, 1);
            cursors.push(view_cursor(&app));
        }
        assert_eq!(
            cursors,
            vec![5, 15, 25, 35],
            "a near card is one tick, a long one goes by thirds"
        );
        app.handle_scroll(1, -1);
        assert_eq!(
            view_cursor(&app),
            5,
            "the card top is within a screen, so one tick reaches it"
        );
    }

    fn view_cursor(app: &App) -> usize {
        view(app).cursor
    }

    #[test]
    fn every_overview_move_scrolls_the_page_until_an_end() {
        use crate::markdown::TextRow;
        let (mut app, _) = files_tab(Vec::new());
        let Screen::Detail(detail) = &mut app.screen else {
            unreachable!()
        };
        detail.tab = Tab::Overview;
        detail.overview = Some(OverviewLayout {
            width: 60,
            rows: (0..200)
                .map(|_| TextRow::plain(ratatui::text::Line::from("text")))
                .collect(),
        });
        detail.page = 30;
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(
            (view(&app).cursor, view(&app).scroll),
            (10, 10),
            "with no card ahead, j reads on by a third"
        );
        app.handle_key(key(KeyCode::Char('G')));
        assert_eq!((view(&app).cursor, view(&app).scroll), (199, 170));
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(
            (view(&app).cursor, view(&app).scroll),
            (189, 160),
            "the first k after G scrolls"
        );
        app.handle_key(key(KeyCode::Char('g')));
        assert_eq!((view(&app).cursor, view(&app).scroll), (0, 0));
    }

    #[test]
    fn hunk_starts_are_found_in_each_default_renderer() {
        assert!(is_hunk_start("@@ -254,10 +254,10 @@ Workflow:"));
        assert!(is_hunk_start("─────────────────┐"));
        assert!(is_hunk_start("prompts/a.md --- 2/5 --- Text"));
        assert!(
            !is_hunk_start("────────────────────"),
            "delta's file rule is not a hunk"
        );
        assert!(!is_hunk_start("- a list item --- with dashes"));
        assert!(!is_hunk_start("   and read files via `gh api`."));
    }

    #[test]
    fn the_file_tree_folds_switches_to_flat_and_hides() {
        let files = vec![
            file("src/a.rs", ViewedState::Unviewed),
            file("src/b.rs", ViewedState::Unviewed),
            file("README.md", ViewedState::Unviewed),
        ];
        let (mut app, _) = files_tab(files);
        assert_eq!(
            view(&app).selected_file(),
            Some(0),
            "the cursor starts on a file, not the folder"
        );
        app.handle_key(key(KeyCode::Char('j')));
        app.handle_key(key(KeyCode::Char('t')));
        assert!(!view(&app).tree_view);
        assert_eq!(
            view(&app).selected_file(),
            Some(1),
            "switching views keeps the file"
        );
        app.handle_key(key(KeyCode::Char('t')));
        app.handle_key(key(KeyCode::Char('g')));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(
            view(&app).file_rows().len(),
            2,
            "the folded src/ shows as one row"
        );
        app.handle_key(key(KeyCode::Char('z')));
        assert!(view(&app).file_list_hidden);
    }

    #[test]
    fn forgetting_colors_rebuilds_the_diff_and_the_overview() {
        let files = vec![file("a.md", ViewedState::Unviewed)];
        let (mut app, pr) = files_tab(files);
        ready_repo(&mut app, &pr);
        let job = app
            .take_diff_request()
            .expect("the open file asks for its diff");
        app.set_diff(&pr, job.key, crate::highlight::to_text(b"x\n", None));
        assert!(app.take_diff_request().is_none());
        view_mut(&mut app).overview = Some(OverviewLayout {
            width: 60,
            rows: Vec::new(),
        });
        app.forget_colors();
        assert!(
            view(&app).overview.is_none(),
            "a new theme lays out the Overview again"
        );
        assert!(
            app.take_diff_request().is_some(),
            "a new theme renders the diff again"
        );
    }

    #[test]
    fn the_theme_menu_opens_on_the_current_theme_and_applies_the_pick() {
        let mut app = app_with(vec![group("A", &[7], false)]);
        app.theme_name = "dracula";
        app.handle_key(key(KeyCode::Char('T')));
        assert_eq!(app.theme_menu, Some(4));
        app.handle_key(key(KeyCode::Char('j')));
        let action = app.handle_key(key(KeyCode::Enter));
        assert!(matches!(action, Some(Action::SetTheme("gruvbox_dark"))));
        assert!(app.theme_menu.is_none());
        app.handle_key(key(KeyCode::Char('T')));
        app.handle_key(key(KeyCode::Esc));
        assert!(app.theme_menu.is_none(), "esc closes without a change");
    }

    #[test]
    fn a_click_switches_tabs_and_selects_rows() {
        use ratatui::layout::Rect;
        let files = vec![
            file("a.md", ViewedState::Unviewed),
            file("b.md", ViewedState::Unviewed),
        ];
        let (mut app, _) = files_tab(files);
        let Screen::Detail(detail) = &mut app.screen else {
            unreachable!()
        };
        detail.clicks = vec![
            ClickTarget {
                area: Rect::new(0, 4, 10, 1),
                click: Click::Tab(Tab::Commits),
            },
            ClickTarget {
                area: Rect::new(0, 8, 30, 1),
                click: Click::Row(1),
            },
        ];
        app.handle_click(5, 8);
        assert_eq!(view(&app).selected_file(), Some(1));
        app.handle_click(3, 4);
        assert_eq!(view(&app).tab, Tab::Commits);
        assert!(
            app.handle_click(50, 20).is_none(),
            "a click on nothing does nothing"
        );
    }

    #[test]
    fn tab_jumps_between_group_headers() {
        let mut app = app_with(vec![group("A", &[1, 2], false), group("B", &[3], false)]);
        app.list_state.select(Some(1));
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.selected_row(), Some(Row::Header(1)));
        app.handle_key(key(KeyCode::BackTab));
        assert_eq!(app.selected_row(), Some(Row::Header(0)));
        assert!(!app.groups[0].collapsed, "Tab no longer folds");
        app.handle_key(key(KeyCode::BackTab));
        assert_eq!(
            app.selected_row(),
            Some(Row::Header(1)),
            "back from the first goes to the last"
        );
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(
            app.selected_row(),
            Some(Row::Header(0)),
            "past the last goes to the first"
        );
    }

    #[test]
    fn brackets_jump_to_unviewed_files() {
        let files = vec![
            file("a.md", ViewedState::Unviewed),
            file("b.md", ViewedState::Viewed),
            file("c.md", ViewedState::Viewed),
            file("d.md", ViewedState::Dismissed),
        ];
        let (mut app, _) = files_tab(files);
        app.handle_key(key(KeyCode::Char(']')));
        assert_eq!(view(&app).selected_file(), Some(3));
        app.handle_key(key(KeyCode::Char('[')));
        assert_eq!(view(&app).selected_file(), Some(0));
    }

    #[test]
    fn enter_on_a_file_shows_the_whole_file() {
        let (mut app, pr) = files_tab(vec![file("a.md", ViewedState::Unviewed)]);
        ready_repo(&mut app, &pr);
        let Some(Action::ShowWholeDiff(job)) = app.handle_key(key(KeyCode::Enter)) else {
            panic!("expected the whole file with its changes");
        };
        assert_eq!(job.paths, ["a.md"]);
    }

    #[test]
    fn ctrl_l_and_cmd_l_open_the_link_box() {
        for modifiers in [KeyModifiers::CONTROL, KeyModifiers::SUPER] {
            let mut app = App::new();
            assert!(app.handle_key(with_modifier('l', modifiers)).is_none());
            assert!(app.prompt.is_some(), "{modifiers:?}");
        }
    }

    #[test]
    fn a_pasted_link_opens_its_pr() {
        let mut app = App::new();
        app.handle_paste(" https://github.com/acme/app/pull/2302/changes \n");
        let action = app.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(action, Some(Action::FetchDetail(ref pr)) if pr.repo == "acme/app" && pr.number == 2302)
        );
        assert!(app.prompt.is_none());
    }

    #[test]
    fn a_bad_link_keeps_the_box_open_with_an_error() {
        let mut app = App::new();
        app.handle_key(with_modifier('l', KeyModifiers::CONTROL));
        app.handle_key(key(KeyCode::Char('x')));
        assert!(app.handle_key(key(KeyCode::Enter)).is_none());
        assert!(app.prompt.as_ref().unwrap().error.is_some());
        app.handle_key(key(KeyCode::Esc));
        assert!(app.prompt.is_none());
    }

    #[test]
    fn tab_keys_cycle_and_wrap() {
        let mut app = app_with(vec![group("A", &[7], false)]);
        app.list_state.select(Some(1));
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::BackTab));
        let Screen::Detail(view) = &app.screen else {
            panic!("expected detail screen")
        };
        assert_eq!(view.tab, Tab::Files);
        app.handle_key(key(KeyCode::Char('2')));
        let Screen::Detail(view) = &app.screen else {
            panic!("expected detail screen")
        };
        assert_eq!(view.tab, Tab::Commits);
    }

    #[test]
    fn enter_on_the_overview_does_nothing_and_o_opens_the_pr() {
        let mut app = app_with(vec![group("A", &[7], false)]);
        app.list_state.select(Some(1));
        app.handle_key(key(KeyCode::Enter));
        assert!(app.handle_key(key(KeyCode::Enter)).is_none());
        let action = app.handle_key(key(KeyCode::Char('o')));
        assert!(
            matches!(action, Some(Action::OpenBrowser(ref url)) if url == "https://github.com/acme/app/pull/7")
        );
    }

    fn file(path: &str, viewed: ViewedState) -> ChangedFile {
        ChangedFile {
            path: path.to_string(),
            additions: 1,
            deletions: 0,
            change: "modified".to_string(),
            viewed,
        }
    }

    /// An open PR with its detail loaded and the Files tab showing.
    fn files_tab(files: Vec<ChangedFile>) -> (App, PrRef) {
        let mut app = app_with(vec![group("A", &[7], false)]);
        app.list_state.select(Some(1));
        app.handle_key(key(KeyCode::Enter));
        let pr = view(&app).pr.clone();
        app.set_detail(&pr, Ok(crate::model::tests::sample_detail()));
        app.set_activity(&pr, Ok(crate::model::tests::sample_activity()));
        app.handle_key(key(KeyCode::Char('4')));
        app.set_files(&pr, Ok(files));
        (app, pr)
    }

    fn view(app: &App) -> &DetailView {
        let Screen::Detail(view) = &app.screen else {
            panic!("expected detail screen")
        };
        view
    }

    #[test]
    fn view_toggles_carry_over_to_the_next_pull_request() {
        let (mut app, _) = files_tab(Vec::new());
        app.renderer_names = vec!["word".to_string(), "delta".to_string()];
        app.sync_prefs();
        assert!(!app.sync_prefs(), "nothing changed since");
        for code in ['m', 'w', 't', 'V', 'z', '1', 'n', 'F'] {
            app.handle_key(key(KeyCode::Char(code)));
        }
        assert!(app.sync_prefs());
        assert!(!app.sync_prefs(), "saved once per change");
        app.handle_key(key(KeyCode::Esc));
        app.handle_key(key(KeyCode::Enter));
        let view = view(&app);
        assert_eq!(view.renderer, 1);
        assert!(!view.diff_wrap && !view.tree_view && view.hide_viewed && view.file_list_hidden);
        assert!(view.comments.newest_first && view.comments.no_bots);
    }

    #[test]
    fn files_tab_requests_its_list_once() {
        let mut app = app_with(vec![group("A", &[7], false)]);
        app.list_state.select(Some(1));
        app.handle_key(key(KeyCode::Enter));
        assert!(app.take_files_request().is_none());
        app.handle_key(key(KeyCode::Char('4')));
        assert!(app.take_files_request().is_some());
        app.handle_key(key(KeyCode::Char('1')));
        app.handle_key(key(KeyCode::Char('4')));
        assert!(app.take_files_request().is_none());
    }

    #[test]
    fn reloading_on_the_files_tab_requests_the_list_again() {
        let (mut app, _) = files_tab(vec![file("a.md", ViewedState::Unviewed)]);
        app.handle_key(key(KeyCode::Char('r')));
        assert!(app.take_files_request().is_some());
    }

    fn ready_repo(app: &mut App, pr: &PrRef) {
        let renames = HashMap::from([("b.md".to_string(), "old/b.md".to_string())]);
        app.set_repo(
            pr,
            Ok(LocalRepo {
                dir: PathBuf::from("/clone"),
                renames,
            }),
        );
        let Screen::Detail(view) = &mut app.screen else {
            panic!("expected detail screen")
        };
        view.diff_width = 80;
    }

    #[test]
    fn the_repo_is_requested_once_the_detail_loads() {
        let (mut app, _) = files_tab(vec![file("a.md", ViewedState::Unviewed)]);
        let job = app.take_repo_request().expect("repo job");
        assert_eq!(
            (job.base_oid.as_str(), job.head_oid.as_str()),
            ("base000", "head999")
        );
        assert!(app.take_repo_request().is_none(), "asked twice");
    }

    #[test]
    fn a_renamed_file_diffs_with_both_names() {
        let (mut app, pr) = files_tab(vec![
            file("a.md", ViewedState::Unviewed),
            file("b.md", ViewedState::Unviewed),
        ]);
        ready_repo(&mut app, &pr);
        let first = app.take_diff_request().expect("diff job");
        assert_eq!(first.paths, ["a.md"]);
        assert!(
            app.take_diff_request().is_none(),
            "the pending diff was requested again"
        );
        app.handle_key(key(KeyCode::Char('j')));
        let second = app.take_diff_request().expect("diff job");
        assert_eq!(second.paths, ["old/b.md", "b.md"]);
        assert_eq!(
            (second.base.as_str(), second.head.as_str()),
            ("base000", "head999")
        );
    }

    #[test]
    fn m_switches_renderer_and_needs_a_new_diff() {
        let (mut app, pr) = files_tab(vec![file("a.md", ViewedState::Unviewed)]);
        app.renderer_names = vec!["word".to_string(), "delta".to_string()];
        ready_repo(&mut app, &pr);
        let first = app.take_diff_request().unwrap();
        app.set_diff(
            &pr,
            first.key,
            crate::highlight::to_text(b"\x1b[32m+added\x1b[0m\n", None),
        );
        assert!(app.take_diff_request().is_none());
        app.handle_key(key(KeyCode::Char('m')));
        assert_eq!(app.take_diff_request().unwrap().key.renderer, 1);
        app.handle_key(key(KeyCode::Char('m')));
        assert!(app.take_diff_request().is_none(), "renderer 0 is cached");
    }

    #[test]
    fn a_commit_diffs_against_its_parent() {
        let (mut app, pr) = files_tab(vec![]);
        app.handle_key(key(KeyCode::Char('2')));
        ready_repo(&mut app, &pr);
        let job = app.take_diff_request().expect("diff job");
        assert_eq!(
            (job.base.as_str(), job.head.as_str()),
            ("aaa111~1", "aaa111")
        );
        assert!(job.paths.is_empty());
    }

    #[test]
    fn declining_the_clone_leaves_a_message_and_no_action() {
        let (mut app, pr) = files_tab(vec![]);
        app.ask(
            PromptKind::ConfirmClone {
                pr: pr.clone(),
                dir: PathBuf::from("/w/app"),
            },
            "",
        );
        assert!(app.handle_key(key(KeyCode::Esc)).is_none());
        let Screen::Detail(view) = &app.screen else {
            panic!("expected detail screen")
        };
        assert!(matches!(view.repo, RepoState::Failed(_)));
        app.ask(
            PromptKind::ConfirmClone {
                pr,
                dir: PathBuf::from("/w/app"),
            },
            "",
        );
        app.handle_key(key(KeyCode::Char('y')));
        assert!(matches!(
            app.handle_key(key(KeyCode::Enter)),
            Some(Action::CloneRepo { .. })
        ));
    }

    #[test]
    fn v_marks_viewed_moves_on_and_asks_for_a_sync() {
        let (mut app, _) = files_tab(vec![
            file("a.md", ViewedState::Unviewed),
            file("b.md", ViewedState::Unviewed),
        ]);
        let action = app.handle_key(key(KeyCode::Char('v')));
        assert!(
            matches!(action, Some(Action::SetViewed { ref path, viewed: true, ref pull_request_id, .. })
            if path == "a.md" && pull_request_id == "PR_node")
        );
        assert_eq!(
            view(&app).loaded_files().unwrap()[0].viewed,
            ViewedState::Viewed
        );
        assert_eq!(view(&app).selected_file(), Some(1));
    }

    #[test]
    fn a_failed_sync_puts_the_checkbox_back() {
        let (mut app, pr) = files_tab(vec![file("a.md", ViewedState::Unviewed)]);
        app.handle_key(key(KeyCode::Char('v')));
        app.set_viewed_result(&pr, "a.md", true, Err("forbidden".to_string()));
        assert_eq!(
            view(&app).loaded_files().unwrap()[0].viewed,
            ViewedState::Unviewed
        );
        assert!(app.error.as_deref().unwrap().contains("forbidden"));
    }

    #[test]
    fn hiding_viewed_files_skips_them_in_the_list() {
        let (mut app, _) = files_tab(vec![
            file("a.md", ViewedState::Viewed),
            file("b.md", ViewedState::Unviewed),
        ]);
        app.handle_key(key(KeyCode::Char('V')));
        assert_eq!(view(&app).visible_files(), [1]);
        assert_eq!(view(&app).selected_file(), Some(1));
    }

    fn view_mut(app: &mut App) -> &mut DetailView {
        match &mut app.screen {
            Screen::Detail(view) => view,
            Screen::Inbox => panic!("expected detail screen"),
        }
    }

    fn type_text(app: &mut App, text: &str) {
        for character in text.chars() {
            app.handle_key(key(KeyCode::Char(character)));
        }
    }

    #[test]
    fn an_empty_search_runs_nothing() {
        let (mut app, pr) = files_tab(vec![file("a.md", ViewedState::Unviewed)]);
        ready_repo(&mut app, &pr);
        app.handle_key(key(KeyCode::Char('s')));
        type_text(&mut app, "  ");
        assert!(app.handle_key(key(KeyCode::Enter)).is_none());
        assert!(view(&app).overlays.is_empty());
        assert_eq!(app.error.as_deref(), Some("type some text to search for"));
    }

    #[test]
    fn search_needs_the_clone() {
        let (mut app, _) = files_tab(vec![file("a.md", ViewedState::Unviewed)]);
        app.handle_key(key(KeyCode::Char('s')));
        type_text(&mut app, "Foo");
        assert!(app.handle_key(key(KeyCode::Enter)).is_none());
        assert!(app.error.as_deref().unwrap().contains("clone is not ready"));
    }

    #[test]
    fn search_lists_hits_and_enter_opens_the_file_at_the_line() {
        let (mut app, pr) = files_tab(vec![file("a.md", ViewedState::Unviewed)]);
        ready_repo(&mut app, &pr);
        app.handle_key(key(KeyCode::Char('s')));
        type_text(&mut app, "Foo");
        let Some(Action::Grep { commit, query, .. }) = app.handle_key(key(KeyCode::Enter)) else {
            panic!("expected a grep");
        };
        assert_eq!((commit.as_str(), query.as_str()), ("head999", "Foo"));
        let hit = |path: &str, line| GrepHit {
            path: path.to_string(),
            line,
            text: "Foo".to_string(),
        };
        let Some(Overlay::Search { state, .. }) = view_mut(&mut app).overlays.last_mut() else {
            panic!("expected search results");
        };
        // What ratatui does when it draws the list while it is still empty.
        state.select(None);
        app.set_grep(&pr, "Foo", Ok(vec![hit("x.py", 3), hit("y.py", 40)]));
        let Some(Overlay::Search { state, .. }) = view(&app).overlays.last() else {
            panic!("expected search results");
        };
        assert_eq!(state.selected(), Some(0), "enter must open the first hit");
        app.handle_key(key(KeyCode::Char('j')));
        let Some(Action::ShowFile { path, .. }) = app.handle_key(key(KeyCode::Enter)) else {
            panic!("expected a file");
        };
        assert_eq!(path, "y.py");
        let text = (1..=50)
            .map(|number| format!("line {number}\n"))
            .collect::<String>();
        app.set_file(
            &pr,
            "y.py",
            crate::highlight::to_text(text.as_bytes(), None),
        );
        let Some(Overlay::File {
            scroll,
            lines: Some(Ok(lines)),
            ..
        }) = view(&app).overlays.last()
        else {
            panic!("expected a loaded file");
        };
        assert_eq!(
            (*scroll, lines.lines.len()),
            (34, 50),
            "the hit sits a few lines down"
        );
        app.handle_key(key(KeyCode::Esc));
        assert!(matches!(
            view(&app).overlays.last(),
            Some(Overlay::Search { .. })
        ));
        app.handle_key(key(KeyCode::Esc));
        assert!(view(&app).overlays.is_empty());
    }

    #[test]
    fn find_jumps_to_matching_lines_and_wraps() {
        let (mut app, pr) = files_tab(vec![file("a.md", ViewedState::Unviewed)]);
        ready_repo(&mut app, &pr);
        let job = app.take_diff_request().unwrap();
        app.set_diff(
            &pr,
            job.key,
            crate::highlight::to_text(
                b"one\nNeedle two\nthree\nfour\nfive\nsix\nneedle seven\n",
                None,
            ),
        );
        let place = |app: &App| {
            let view = view(app);
            let hit = view.current_find_hit().expect("a hit");
            (view.diff_scroll, hit.line, hit.ordinal, hit.total)
        };
        app.handle_key(key(KeyCode::Char('/')));
        type_text(&mut app, "needle");
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(place(&app), (0, 1, 1, 2));
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(
            place(&app),
            (3, 6, 2, 2),
            "three lines stay above the match"
        );
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(place(&app), (0, 1, 1, 2), "wraps to the first match");
        app.handle_key(key(KeyCode::Char('N')));
        assert_eq!(place(&app), (3, 6, 2, 2), "wraps back to the last match");
    }

    #[test]
    fn n_goes_on_to_the_next_file_that_holds_the_text() {
        let (mut app, pr) = files_tab(vec![
            file("a.md", ViewedState::Unviewed),
            file("b.md", ViewedState::Unviewed),
        ]);
        ready_repo(&mut app, &pr);
        let job = app.take_diff_request().unwrap();
        app.set_diff(&pr, job.key, crate::highlight::to_text(b"nothing\n", None));
        app.handle_key(key(KeyCode::Char('/')));
        type_text(&mut app, "needle");
        let action = app.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(action, Some(Action::FindFiles { .. })),
            "{action:?}"
        );
        app.set_find_files(&pr, "needle", Ok(vec![("b.md".to_string(), 2)]));
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(view(&app).selected_file(), Some(1), "on to b.md");
        assert!(
            view(&app).current_find_hit().is_none(),
            "waits for its diff"
        );
        let job = app.take_diff_request().unwrap();
        app.set_diff(
            &pr,
            job.key,
            crate::highlight::to_text(b"x\nneedle\nneedle\n", None),
        );
        app.settle_find();
        let hit = view(&app).current_find_hit().expect("found once drawn");
        assert_eq!((hit.line, hit.ordinal, hit.total), (1, 1, 2));
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(
            view(&app).current_find_hit().map(|hit| hit.ordinal),
            Some(2)
        );
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(
            view(&app).selected_file(),
            Some(1),
            "the only file: round to its first"
        );
        assert_eq!(
            view(&app).current_find_hit().map(|hit| hit.ordinal),
            Some(1)
        );
    }

    #[test]
    fn a_resize_keeps_the_find_on_the_same_match() {
        let (mut app, pr) = files_tab(vec![file("a.md", ViewedState::Unviewed)]);
        ready_repo(&mut app, &pr);
        let job = app.take_diff_request().unwrap();
        let text = b"needle\nx\nneedle\nneedle\n";
        app.set_diff(&pr, job.key, crate::highlight::to_text(text, None));
        app.handle_key(key(KeyCode::Char('/')));
        type_text(&mut app, "needle");
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Char('n')));
        if let Screen::Detail(view) = &mut app.screen {
            view.diff_width = 60;
        }
        assert!(
            view(&app).current_find_hit().is_none(),
            "the old width's hit"
        );
        let job = app
            .take_diff_request()
            .expect("drawn again at the new width");
        app.set_diff(&pr, job.key, crate::highlight::to_text(text, None));
        app.settle_find();
        let hit = view(&app)
            .current_find_hit()
            .expect("followed to the new drawing");
        assert_eq!((hit.ordinal, hit.line), (2, 2));
    }

    #[test]
    fn tab_completes_the_find_to_a_word_from_the_diff() {
        let (mut app, pr) = files_tab(vec![file("a.md", ViewedState::Unviewed)]);
        ready_repo(&mut app, &pr);
        app.handle_key(key(KeyCode::Char('/')));
        app.set_find_words(
            &pr,
            vec!["invoice".to_string(), "consolidation".to_string()],
        );
        type_text(&mut app, "CONS");
        assert_eq!(
            app.prompt.as_ref().unwrap().suggestion().as_deref(),
            Some("olidation")
        );
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.prompt.as_ref().unwrap().text, "CONSolidation");
        assert_eq!(
            app.prompt.as_ref().unwrap().suggestion(),
            None,
            "nothing longer"
        );
    }

    #[test]
    fn a_find_marks_every_match_in_a_line_ignoring_case() {
        use ratatui::style::{Color, Style};
        use ratatui::text::{Line, Span};
        let line = Line::from(vec![Span::raw("a Needle and "), Span::raw("NEEDLE")]);
        let mark = Style::new().bg(Color::Yellow);
        let marked = crate::ui::mark_matches(line, "needle", mark);
        let hits: Vec<&str> = marked
            .spans
            .iter()
            .filter(|span| span.style == mark)
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(hits, ["Needle", "NEEDLE"]);
    }
}
