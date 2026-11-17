//! Screen state and key handling. It does no I/O: keys that need I/O return an `Action`.

use ansi_to_tui::IntoText;
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Text;
use ratatui::widgets::ListState;
use std::collections::HashMap;
use std::path::PathBuf;

use crate::repo::{GrepHit, LocalRepo};

use crate::model::{ChangedFile, Group, PrDetail, PrRef, PrSummary, ViewedState, parse_pr_ref};

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
    ShowFile {
        pr: PrRef,
        dir: PathBuf,
        commit: String,
        path: String,
    },
    SetWorkspace(String),
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

/// What the main loop needs to render one diff off the UI thread.
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
        /// The line to mark, counted from 1. Zero marks none.
        line: usize,
        lines: Option<Result<Vec<String>, String>>,
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

fn line_text(line: &ratatui::text::Line) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

pub struct DetailView {
    pub pr: PrRef,
    /// `None` while the request runs.
    pub detail: Option<Result<PrDetail, String>>,
    pub tab: Tab,
    pub scroll: u16,
    pub commits: ListState,
    pub checks: ListState,
    /// `None` until the Files tab is first shown and its request returns.
    pub files: Option<Result<Vec<ChangedFile>, String>>,
    files_requested: bool,
    /// Indexes into the visible files, not into `files`.
    pub file_state: ListState,
    pub hide_viewed: bool,
    pub repo: RepoState,
    pub renderer: usize,
    pub diff_scroll: u16,
    /// The diff pane's width at the last draw; diffs render to fit it.
    pub diff_width: u16,
    pub diffs: HashMap<DiffKey, DiffOutput>,
    pub overlays: Vec<Overlay>,
    /// The lowercased text that `n` and `N` look for in the diff.
    pub find: Option<String>,
}

impl DetailView {
    fn new(pr: PrRef) -> Self {
        Self {
            pr,
            detail: None,
            tab: Tab::Overview,
            scroll: 0,
            commits: ListState::default().with_selected(Some(0)),
            checks: ListState::default().with_selected(Some(0)),
            files: None,
            files_requested: false,
            file_state: ListState::default().with_selected(Some(0)),
            hide_viewed: false,
            repo: RepoState::Unknown,
            renderer: 0,
            diff_scroll: 0,
            diff_width: 0,
            diffs: HashMap::new(),
            overlays: Vec::new(),
            find: None,
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
                DiffTarget::Commit(self.loaded()?.commits.get(index)?.oid.clone())
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

    /// The clone folder and the PR's head commit, once the clone is ready.
    fn head_in_clone(&self) -> Result<(PathBuf, String), String> {
        let RepoState::Ready(repo) = &self.repo else {
            return Err("the local clone is not ready; open Files changed first".to_string());
        };
        let detail = self.loaded().ok_or("the pull request is still loading")?;
        Ok((repo.dir.clone(), detail.head_oid.clone()))
    }

    fn search(&mut self, query: String) -> Result<Action, String> {
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
            line,
            lines: None,
            scroll,
        });
        Ok(Action::ShowFile {
            pr: self.pr.clone(),
            dir,
            commit,
            path,
        })
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
                    let last = u16::try_from(lines.len().saturating_sub(1)).unwrap_or(u16::MAX);
                    *scroll = scroll.saturating_add_signed(delta).min(last);
                }
            }
            _ => {}
        }
        Ok(None)
    }

    fn start_find(&mut self, query: &str) -> Result<(), String> {
        self.find = Some(query.to_lowercase());
        self.find_in_diff(true, false)
    }

    /// Scrolls the diff so the next (or previous) line holding the find text is on top.
    /// The search wraps around the end.
    fn find_in_diff(&mut self, forward: bool, skip_top_line: bool) -> Result<(), String> {
        let query = self
            .find
            .clone()
            .ok_or("press / to find text in the diff")?;
        let key = self.current_diff_key().ok_or("no diff here")?;
        let Some(DiffOutput::Done(Ok(text))) = self.diffs.get(&key) else {
            return Err("the diff is not ready".to_string());
        };
        let matches: Vec<usize> = text
            .lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line_text(line).to_lowercase().contains(&query))
            .map(|(index, _)| index)
            .collect();
        let top = self.diff_scroll as usize;
        let next = if forward {
            let after = |index: &&usize| **index > top || (!skip_top_line && **index == top);
            matches.iter().find(after).or(matches.first())
        } else {
            matches
                .iter()
                .rev()
                .find(|index| **index < top)
                .or(matches.last())
        };
        let line = next.ok_or_else(|| format!("\"{query}\" is not in this diff"))?;
        self.diff_scroll = u16::try_from(*line).unwrap_or(u16::MAX);
        Ok(())
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

    pub fn selected_file(&self) -> Option<usize> {
        self.visible_files()
            .get(self.file_state.selected()?)
            .copied()
    }

    fn show_tab(&mut self, tab: Tab) {
        self.tab = tab;
        self.diff_scroll = 0;
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
        let last = self.visible_files().len().saturating_sub(1);
        let current = self.file_state.selected().unwrap_or(0);
        self.file_state.select(Some(current.min(last)));
    }

    /// The page `o` and `Enter` open: the selected commit or check, else the PR.
    fn link(&self) -> String {
        let selected = |state: &ListState| state.selected().unwrap_or(0);
        let item_url = self.loaded().and_then(|detail| match self.tab {
            Tab::Overview => None,
            Tab::Commits => detail
                .commits
                .get(selected(&self.commits))
                .map(|commit| commit.url.clone()),
            Tab::Checks => detail
                .checks
                .get(selected(&self.checks))
                .and_then(|check| check.url.clone()),
            Tab::Files => Some(format!("{}/files", self.pr.url())),
        });
        item_url.unwrap_or_else(|| self.pr.url())
    }

    fn list_len(&self) -> usize {
        let Some(detail) = self.loaded() else {
            return 0;
        };
        match self.tab {
            Tab::Overview => 0,
            Tab::Commits => detail.commits.len(),
            Tab::Checks => detail.checks.len(),
            Tab::Files => self.visible_files().len(),
        }
    }

    fn move_in_tab(&mut self, delta: isize) {
        let last = self.list_len().saturating_sub(1);
        let state = match self.tab {
            Tab::Overview => {
                self.scroll = self.scroll.saturating_add_signed(delta as i16);
                return;
            }
            Tab::Commits => &mut self.commits,
            Tab::Checks => &mut self.checks,
            Tab::Files => &mut self.file_state,
        };
        let next = state
            .selected()
            .unwrap_or(0)
            .saturating_add_signed(delta)
            .min(last);
        state.select(Some(next));
        self.diff_scroll = 0;
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
}

pub struct App {
    pub groups: Vec<Group>,
    pub list_state: ListState,
    pub screen: Screen,
    pub prompt: Option<Prompt>,
    pub loading: bool,
    pub error: Option<String>,
    pub fetched_at: Option<DateTime<Utc>>,
    /// Names of the configured diff renderers, in order; `m` cycles through them.
    pub renderer_names: Vec<String>,
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
            renderer_names: Vec::new(),
        }
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
        let detail = view.loaded()?;
        let (base, head, paths) = match &key.target {
            DiffTarget::File(path) => {
                let old = repo.renames.get(path).cloned();
                let paths = old.into_iter().chain([path.clone()]).collect();
                (detail.base_oid.clone(), detail.head_oid.clone(), paths)
            }
            DiffTarget::Commit(oid) => (format!("{oid}~1"), oid.clone(), Vec::new()),
        };
        let job = DiffJob {
            pr: view.pr.clone(),
            key: key.clone(),
            dir: repo.dir.clone(),
            base,
            head,
            paths,
        };
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
    pub fn set_diff(&mut self, pr: &PrRef, key: DiffKey, result: Result<Vec<u8>, String>) {
        let Some(view) = self.view_for(pr) else {
            return;
        };
        let text = result.and_then(|bytes| {
            bytes
                .into_text()
                .map_err(|error| format!("bad renderer output: {error}"))
        });
        view.diffs.insert(key, DiffOutput::Done(text));
    }

    /// Fills the search results that are still waiting for this query.
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
    pub fn set_file(&mut self, pr: &PrRef, path: &str, result: Result<String, String>) {
        let Some(view) = self.view_for(pr) else {
            return;
        };
        // The terminal draws a tab as one cell, so expand it like an editor would.
        let lines = result.map(|text| {
            text.lines()
                .map(|line| line.replace('\t', "    "))
                .collect::<Vec<_>>()
        });
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
        });
    }

    pub fn set_prompt_error(&mut self, kind: PromptKind, text: &str, error: String) {
        self.prompt = Some(Prompt {
            kind,
            text: text.to_string(),
            error: Some(error),
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

    /// Keeps the fold state the user chose for groups that still exist.
    pub fn set_groups(&mut self, mut groups: Vec<Group>, fetched_at: DateTime<Utc>) {
        for group in &mut groups {
            if let Some(previous) = self.groups.iter().find(|old| old.title == group.title) {
                group.collapsed = previous.collapsed;
            }
        }
        self.groups = groups;
        self.loading = false;
        self.error = None;
        self.fetched_at = Some(fetched_at);
        self.clamp_selection();
    }

    pub fn set_inbox_error(&mut self, message: String) {
        self.loading = false;
        self.error = Some(message);
    }

    /// The open PR's view, unless the user has already left that PR.
    fn view_for(&mut self, pr: &PrRef) -> Option<&mut DetailView> {
        match &mut self.screen {
            Screen::Detail(view) if view.pr == *pr => Some(view),
            _ => None,
        }
    }

    pub fn set_detail(&mut self, pr: &PrRef, result: Result<PrDetail, String>) {
        if let Some(view) = self.view_for(pr) {
            view.detail = Some(result);
        }
    }

    pub fn set_files(&mut self, pr: &PrRef, result: Result<Vec<ChangedFile>, String>) {
        if let Some(view) = self.view_for(pr) {
            view.files = Some(result);
            view.clamp_file_selection();
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

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        if self.prompt.is_some() {
            return self.prompt_key(key.code);
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
            PromptKind::FindInDiff => {
                self.in_view(|view| view.start_find(&prompt.text).map(|()| None))
            }
        }
    }

    /// Runs a step on the open PR, and shows its error in the footer.
    fn in_view(
        &mut self,
        step: impl FnOnce(&mut DetailView) -> Result<Option<Action>, String>,
    ) -> Option<Action> {
        let view = self.open_view()?;
        step(view).unwrap_or_else(|error| {
            self.error = Some(error);
            None
        })
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
        self.screen = Screen::Detail(Box::new(DetailView::new(pr.clone())));
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
            KeyCode::Char(' ') | KeyCode::Tab => self.toggle_selected_group(),
            KeyCode::Enter => return self.open_selected(),
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
            return view.overlay_key(key).unwrap_or_else(|error| {
                self.error = Some(error);
                None
            });
        }
        let find_result = match key {
            KeyCode::Char('n') if diff_pane => Some(view.find_in_diff(true, true)),
            KeyCode::Char('N') if diff_pane => Some(view.find_in_diff(false, true)),
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
                    ..Prompt::default()
                });
            }
            KeyCode::Char('e') if view.tab == Tab::Files => {
                let path = view
                    .loaded_files()
                    .and_then(|files| Some(files.get(view.selected_file()?)?.path.clone()));
                return view.open_file(path?, 0).map_or_else(
                    |error| {
                        self.error = Some(error);
                        None
                    },
                    Some,
                );
            }
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
            KeyCode::Char('H') if view.tab == Tab::Files => {
                view.hide_viewed = !view.hide_viewed;
                view.clamp_file_selection();
            }
            KeyCode::Char('j') | KeyCode::Down => view.move_in_tab(1),
            KeyCode::Char('k') | KeyCode::Up => view.move_in_tab(-1),
            KeyCode::Char(' ') | KeyCode::PageDown => view.move_in_tab(20),
            KeyCode::Char('b') | KeyCode::PageUp => view.move_in_tab(-20),
            KeyCode::Char('g') | KeyCode::Home => view.move_in_tab(-10_000),
            KeyCode::Char('o') => return Some(Action::OpenBrowser(view.link())),
            KeyCode::Enter if matches!(view.tab, Tab::Commits | Tab::Checks) => {
                return Some(Action::OpenBrowser(view.link()));
            }
            KeyCode::Char('d') => return Some(Action::RunDiff(view.pr.clone())),
            KeyCode::Char('r') => return Some(Self::reload(view)),
            _ => {}
        }
        None
    }

    /// Starts the PR over on the same tab. The file list and the clone are looked up
    /// again when a tab needs them.
    fn reload(view: &mut DetailView) -> Action {
        let tab = view.tab;
        let renderer = view.renderer;
        *view = DetailView::new(view.pr.clone());
        view.tab = tab;
        view.renderer = renderer;
        Action::FetchDetail(view.pr.clone())
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
        app.set_groups(vec![group("A", &[1], true)], Utc::now());
        assert!(!app.groups[0].collapsed);
        assert_eq!(app.list_state.selected(), Some(1));
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
        app.set_diff(&pr, first.key, Ok(b"\x1b[32m+added\x1b[0m\n".to_vec()));
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
        app.handle_key(key(KeyCode::Char('H')));
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
        app.set_file(&pr, "y.py", Ok(text));
        let Some(Overlay::File {
            line,
            scroll,
            lines: Some(Ok(lines)),
            ..
        }) = view(&app).overlays.last()
        else {
            panic!("expected a loaded file");
        };
        assert_eq!((*line, *scroll, lines.len()), (40, 34, 50));
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
            Ok(b"one\nNeedle two\nthree\nneedle four\n".to_vec()),
        );
        app.handle_key(key(KeyCode::Char('/')));
        type_text(&mut app, "needle");
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(view(&app).diff_scroll, 1);
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(view(&app).diff_scroll, 3);
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(view(&app).diff_scroll, 1, "wraps to the first match");
        app.handle_key(key(KeyCode::Char('N')));
        assert_eq!(view(&app).diff_scroll, 3, "wraps back to the last match");
    }
}
