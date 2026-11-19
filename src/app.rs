//! Screen state and key handling. It does no I/O: keys that need I/O return an `Action`.

use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Text;
use ratatui::widgets::ListState;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::editor::{Editor, Outcome};
use crate::file_tree::{self, FileRow};
use crate::github::RepoLabel;
use crate::line_diff::{DiffRow, RowKind};
use crate::markdown::TextRow;
use crate::repo::{GrepHit, LocalRepo, MAX_GREP_HITS};

use crate::config::{EditorConfig, GroupConfig, ViewPrefs};
use crate::model::{
    ChangedFile, Group, HunkLine, Lifecycle, MergeMethod, MergeRequest, PrActivity, PrDetail,
    PrRef, PrSummary, Side, Verdict, ViewedState, parse_pr_ref,
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
    /// Suspend the app and run the editor on a file in the clone.
    OpenEditor(EditorTarget),
    /// The names a line of a file uses, for D to offer.
    LineNames {
        pr: PrRef,
        dir: PathBuf,
        commit: String,
        path: String,
        line: usize,
    },
    /// Where a name is defined at the PR head, the changed files first.
    FindDefinition {
        pr: PrRef,
        dir: PathBuf,
        head: String,
        name: String,
        changed: Vec<String>,
    },
    RunDiff(PrRef),
    /// Fetch an image, or draw a diagram, for the gallery.
    LoadImage(crate::markdown::Visual),
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
    /// Save how many days back the inbox looks, and reload it.
    /// None looks back without a limit.
    SetUpdatedWithin(Option<u32>),
    /// Save the editor o runs, None for $VISUAL or $EDITOR, then open `open` in it: the
    /// file o was opening when it asked for the editor the first time.
    SetEditor {
        editor: Option<EditorConfig>,
        open: Option<EditorTarget>,
    },
    /// Put text on the system clipboard.
    Copy(String),
    /// Look for agents to hand the message to, and list them.
    PickAgent {
        message: String,
        dir: Option<PathBuf>,
    },
    /// Hand the message to the agent picked.
    HandOff {
        target: crate::handoff::Target,
        message: String,
        dir: Option<PathBuf>,
    },
    /// Send a review, or submit the pending one, with a verdict.
    SubmitReview {
        pr: PrRef,
        pull_request_id: String,
        head_oid: String,
        pending_review: Option<String>,
        verdict: Verdict,
        body: String,
    },
    /// Add a line comment to the pending review, starting one when there is none.
    AddComment {
        pr: PrRef,
        pull_request_id: String,
        head_oid: String,
        pending_review: Option<String>,
        target: LineTarget,
        body: String,
    },
    /// Merge the PR, or turn auto-merge on or off.
    Merge {
        pr: PrRef,
        pull_request_id: String,
        head_oid: String,
        request: MergeRequest,
    },
    /// Delete the pending review and its line comments.
    DiscardReview {
        pr: PrRef,
        pending_review: String,
    },
    /// Reply to a review thread. `pending`: you have a pending review, which the reply
    /// joins.
    ReplyToThread {
        pr: PrRef,
        thread: String,
        body: String,
        pending: bool,
    },
    /// Resolve a review thread, or open it again.
    ResolveThread {
        pr: PrRef,
        thread: String,
        resolved: bool,
    },
    /// Read one file's plain diff for select lines.
    LineDiff(DiffJob),
    /// Load the repo's labels for the label menu.
    FetchLabels {
        pr: PrRef,
    },
    /// Add and remove labels on a PR, by label id.
    SetLabels {
        pr: PrRef,
        pull_request_id: String,
        add: Vec<String>,
        remove: Vec<String>,
    },
    CloneRepo {
        pr: PrRef,
        dir: PathBuf,
    },
    /// One file, or every file under a folder, in one request.
    SetViewed {
        pr: PrRef,
        pull_request_id: String,
        paths: Vec<String>,
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
    /// A review thread in select lines, by its place among the file's threads.
    Thread(usize),
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
    File {
        path: String,
        /// The file with the renderer's or bat's colors.
        lines: Option<Result<Text<'static>, String>>,
        scroll: u16,
        /// For a file read at the PR head, the line it opened at; None for a whole diff.
        line: Option<usize>,
    },
}

/// The definitions D found for a name, and the one the file view shows.
#[derive(Debug, Clone)]
pub struct DefWalk {
    pub name: String,
    pub hits: Vec<GrepHit>,
    pub at: usize,
    /// git found the name in more files than revu reads.
    pub capped: bool,
}

/// Where o opens the file in view: its page on GitHub, or the clone's copy in the editor.
#[derive(Debug)]
pub struct OpenMenu {
    pub file: String,
    pub github: String,
    /// What the GitHub page shows, for the menu to say.
    pub github_note: String,
    /// The clone's copy, or why the editor cannot open it.
    pub editor: Result<EditorTarget, String>,
    /// The editor's name, for the menu; App fills it in.
    pub editor_name: String,
    /// 0 for the editor, 1 for GitHub. It starts on the editor when the editor can open the file.
    pub selected: usize,
}

/// A file in the clone's working tree, for the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorTarget {
    pub dir: PathBuf,
    pub path: String,
    pub line: Option<u64>,
    /// Without a line, the editor opens at the file's first change between these commits.
    pub changes: Option<(String, String)>,
    /// The branch or commit the clone has checked out, when it is not the PR's head.
    pub elsewhere: Option<String>,
}

/// A path for a GitHub URL, with the characters that would end or break it escaped.
fn url_path(path: &str) -> String {
    path.chars()
        .map(|character| match character {
            '%' => "%25".to_string(),
            ' ' => "%20".to_string(),
            '#' => "%23".to_string(),
            '?' => "%3F".to_string(),
            other => other.to_string(),
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
struct ViewedSync {
    confirmed: ViewedState,
    pending: u32,
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
    /// Every image the Overview shows, in order, for the gallery.
    pub images: Vec<crate::markdown::Visual>,
}

pub struct DetailView {
    pub pr: PrRef,
    /// The latest load of the detail and activity. A result from an earlier load, which
    /// can finish after a later one, is dropped.
    pub load: u64,
    /// The inbox row the PR was opened from, which draws the header until the detail
    /// arrives. None for a PR opened by link.
    pub summary: Option<PrSummary>,
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
    /// Files with a Viewed write on its way: GitHub's last confirmed state, and how
    /// many writes are still out. A failed write restores the confirmed state once
    /// it is the last one out.
    viewed_sync: HashMap<String, ViewedSync>,
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
    /// The find text's matches in every file at the PR head, which n reaches after the
    /// diff's last match; None while git looks.
    pub find_repo: Option<Result<Vec<GrepHit>, String>>,
    /// git stopped at its limit, so the repo holds more matches than n reaches.
    pub find_repo_capped: bool,
    /// The repo match the file view shows, while n and N walk them.
    pub repo_hit: Option<usize>,
    /// The PR's changed words for the find prompt to complete; None until git returns.
    pub find_words: Option<Vec<String>>,
    find_words_requested: bool,
    /// Select lines on Files changed: a line cursor over the file's plain diff.
    pub line_mode: Option<LineMode>,
    /// t opened every review thread in select lines.
    pub threads_open: bool,
    /// The threads a click flipped from what t set, by ID.
    pub flipped_threads: HashSet<String>,
    /// The thread last clicked: r and u act on it when a line holds several.
    pub chosen_thread: Option<String>,
    /// The select-lines cursor the scroll last brought into view. While it stays there,
    /// J, K, space and b scroll freely, so a thread taller than the pane can be read.
    pub line_followed: Option<usize>,
    /// Each parsed file's definitions, by commit and path; None for a language revu
    /// does not parse.
    pub outlines: HashMap<(String, String), Option<crate::syntax::Outline>>,
    /// Each changed file's hunks by their first changed line, once git returns them.
    pub hunk_firsts: HashMap<String, Vec<crate::line_diff::LineAt>>,
    /// The files whose structure was asked for, so each is read once.
    structure_asked: HashSet<String>,
    /// D's definitions, walked with n and N in the file view.
    pub def_walk: Option<DefWalk>,
    /// The repo's labels, loaded the first time the label menu opens on this PR.
    pub repo_labels: Option<Vec<RepoLabel>>,
    /// n or N moved to another file; the find runs there once its diff is drawn. True
    /// goes to its first match, false to its last.
    find_pending: Option<bool>,
}

impl DetailView {
    /// A saved renderer that is not installed gives way to the first one that is; the
    /// saved choice stays, for when it is installed later.
    fn new(
        pr: PrRef,
        prefs: &ViewPrefs,
        renderer_names: &[String],
        missing: &HashSet<usize>,
    ) -> Self {
        let saved = prefs
            .renderer
            .as_ref()
            .and_then(|name| renderer_names.iter().position(|known| known == name))
            .unwrap_or(0);
        let renderer = match missing.contains(&saved) {
            false => saved,
            true => (0..renderer_names.len())
                .find(|index| !missing.contains(index))
                .unwrap_or(saved),
        };
        Self {
            pr,
            load: 0,
            summary: None,
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
            viewed_sync: HashMap::new(),
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
            find_repo: None,
            find_repo_capped: false,
            repo_hit: None,
            find_pending: None,
            find_words: None,
            find_words_requested: false,
            line_mode: None,
            outlines: HashMap::new(),
            hunk_firsts: HashMap::new(),
            structure_asked: HashSet::new(),
            threads_open: false,
            flipped_threads: HashSet::new(),
            chosen_thread: None,
            line_followed: None,
            def_walk: None,
            repo_labels: None,
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

    /// Whether the drawn hunks are difftastic's, read from the first hunk's header.
    fn drawn_by_difftastic(&self, starts: &[usize]) -> bool {
        let Some(DiffOutput::Done(Ok(text))) =
            self.current_diff_key().and_then(|key| self.diffs.get(&key))
        else {
            return false;
        };
        starts
            .first()
            .and_then(|start| text.lines.get(*start))
            .is_some_and(|line| {
                let plain: String = line
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect();
                is_difftastic_hunk(plain.trim_end())
            })
    }

    /// The clone folder and the PR's head commit, once the clone is ready.
    /// S on Files changed: select lines in the selected file.
    fn start_line_mode(&mut self) -> Result<Action, String> {
        let RepoState::Ready(repo) = &self.repo else {
            return Err(self.clone_not_ready("the local clone is not ready yet"));
        };
        let detail = self.loaded().ok_or("the pull request is still loading")?;
        let key = self.current_diff_key().ok_or("select a file first")?;
        let DiffTarget::File(path) = key.target.clone() else {
            return Err("comments go on the files in Files changed".to_string());
        };
        let job = diff_job(&self.pr, repo, detail, key);
        self.threads_open = false;
        self.flipped_threads.clear();
        self.chosen_thread = None;
        self.line_followed = None;
        self.line_mode = Some(LineMode {
            path,
            rows: None,
            cursor: 0,
            anchor: None,
            scroll: 0,
        });
        Ok(Action::LineDiff(job))
    }

    /// Why the clone cannot be used: the lookup's own error when it failed, git missing
    /// before it ever ran, or else `waiting`.
    fn clone_not_ready(&self, waiting: &str) -> String {
        match &self.repo {
            RepoState::Failed(error) => error.clone(),
            _ if !crate::repo::on_path("git") => format!(
                "git is not installed, so diffs, find and search cannot run. To add it: {}",
                crate::repo::GIT_INSTALL
            ),
            _ => waiting.to_string(),
        }
    }

    /// What o offers while a file is in view: a repo match's file, a whole diff, or the
    /// selected changed file. None elsewhere, where o opens the browser at once.
    fn open_menu(&self) -> Option<OpenMenu> {
        if let Some(Overlay::File {
            path,
            line: Some(line),
            ..
        }) = self.overlays.last()
        {
            let head = &self.loaded()?.head_oid;
            let repo = &self.pr.repo;
            return Some(OpenMenu {
                file: path.clone(),
                github: format!(
                    "https://github.com/{repo}/blob/{head}/{}#L{line}",
                    url_path(path)
                ),
                github_note: format!("the file at the PR head, line {line}"),
                editor: self.editor_target(path, Some(*line as u64)),
                editor_name: String::new(),
                selected: 0,
            });
        }
        if self.tab != Tab::Files {
            return None;
        }
        let path = match self.overlays.last() {
            Some(Overlay::File { path, .. }) => path.clone(),
            _ => {
                let files = self.loaded_files()?;
                files.get(self.selected_file()?)?.path.clone()
            }
        };
        let line = self
            .line_mode
            .as_ref()
            .filter(|mode| mode.path == path)
            .and_then(LineMode::editor_line);
        Some(OpenMenu {
            github: self.files_link(),
            github_note: "the file in the PR's Files changed".to_string(),
            editor: self.editor_target(&path, line),
            editor_name: String::new(),
            file: path,
            selected: 0,
        })
    }

    /// The file in the clone's working tree, which may be on another branch than the PR.
    fn editor_target(&self, path: &str, line: Option<u64>) -> Result<EditorTarget, String> {
        let RepoState::Ready(repo) = &self.repo else {
            return Err(self.clone_not_ready("the local clone is not ready yet"));
        };
        if !repo.dir.join(path).is_file() {
            return Err(
                "not in your clone's checkout; check out the PR branch to edit it".to_string(),
            );
        }
        let detail = self.loaded();
        let changes = match line {
            Some(_) => None,
            None => detail.map(|detail| (detail.base_oid.clone(), detail.head_oid.clone())),
        };
        let elsewhere = detail
            .and_then(|detail| crate::repo::checked_out_elsewhere(&repo.dir, &detail.head_oid));
        Ok(EditorTarget {
            dir: repo.dir.clone(),
            path: path.to_string(),
            line,
            changes,
            elsewhere,
        })
    }

    /// The blocks around select lines' cursor, "BaseApp > on_mount": in the head's
    /// version for an added or kept line, in the base's for a removed one.
    pub fn line_place(&self) -> Option<String> {
        let mode = self.line_mode.as_ref()?;
        let Some(Ok(rows)) = &mode.rows else {
            return None;
        };
        let row = rows.get(mode.cursor)?;
        let detail = self.loaded()?;
        let (commit, path, line) = match (row.new, row.old) {
            (Some(line), _) => (&detail.head_oid, mode.path.clone(), line),
            (None, Some(line)) => (&detail.base_oid, self.base_path(&mode.path), line),
            (None, None) => return None,
        };
        let outline = self.outlines.get(&(commit.clone(), path))?.as_ref()?;
        outline.place(usize::try_from(line).ok()?)
    }

    /// The block the diff pane's top hunk changes, "BaseApp > on_mount". Only when the
    /// drawn hunks match git's one for one; difftastic and whole files show none.
    pub fn hunk_place(&self) -> Option<String> {
        if self.tab != Tab::Files {
            return None;
        }
        let key = self.current_diff_key()?;
        let DiffTarget::File(path) = &key.target else {
            return None;
        };
        let firsts = self.hunk_firsts.get(path)?;
        let starts = self.hunk_starts();
        if starts.len() != firsts.len() || self.drawn_by_difftastic(&starts) {
            return None;
        }
        let top = usize::from(self.diff_scroll);
        let shown = starts.iter().filter(|start| **start <= top).count();
        let first = firsts.get(shown.saturating_sub(1))?;
        let detail = self.loaded()?;
        let (commit, path, line) = match *first {
            crate::line_diff::LineAt::New(line) => (&detail.head_oid, path.clone(), line),
            crate::line_diff::LineAt::Old(line) => (&detail.base_oid, self.base_path(path), line),
        };
        let outline = self.outlines.get(&(commit.clone(), path))?.as_ref()?;
        outline.place(usize::try_from(line).ok()?)
    }

    /// D: asks for the names on select lines' cursor line, in the version of the file
    /// that line belongs to.
    fn line_names_request(&self) -> Result<Action, String> {
        let RepoState::Ready(repo) = &self.repo else {
            return Err(self.clone_not_ready("the local clone is not ready yet"));
        };
        let mode = self.line_mode.as_ref().ok_or("select lines first")?;
        let Some(Ok(rows)) = &mode.rows else {
            return Err("the diff is still loading".to_string());
        };
        let row = rows.get(mode.cursor).ok_or("no line here")?;
        let detail = self.loaded().ok_or("the pull request is still loading")?;
        let (commit, path, line) = match (row.new, row.old) {
            (Some(line), _) => (&detail.head_oid, mode.path.clone(), line),
            (None, Some(line)) => (&detail.base_oid, self.base_path(&mode.path), line),
            (None, None) => return Err("move to a line of code".to_string()),
        };
        Ok(Action::LineNames {
            pr: self.pr.clone(),
            dir: repo.dir.clone(),
            commit: commit.clone(),
            path,
            line: usize::try_from(line).unwrap_or(usize::MAX),
        })
    }

    /// Looks for where `name` is defined at the PR head.
    fn find_definition(&mut self, name: &str) -> Result<Option<Action>, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("type a name to look up".to_string());
        }
        let (dir, head) = self.head_in_clone()?;
        let changed = self
            .loaded_files()
            .map(|files| files.iter().map(|file| file.path.clone()).collect())
            .unwrap_or_default();
        Ok(Some(Action::FindDefinition {
            pr: self.pr.clone(),
            dir,
            head,
            name: name.to_string(),
            changed,
        }))
    }

    /// Opens a definition in the file view, in place of the one shown.
    fn show_definition(&mut self, at: usize) -> Result<Action, String> {
        let walk = self.def_walk.as_mut().ok_or("no definitions to show")?;
        let hit = walk.hits.get(at).ok_or("no such definition")?.clone();
        walk.at = at;
        if matches!(self.overlays.last(), Some(Overlay::File { .. })) {
            self.overlays.pop();
        }
        self.open_file(hit.path, hit.line)
    }

    /// n and N in a definition's file view; past either end they wrap around.
    fn next_definition(&mut self, forward: bool) -> Result<Option<Action>, String> {
        let walk = self.def_walk.as_ref().ok_or("no definitions to walk")?;
        let count = walk.hits.len();
        let at = match forward {
            true => (walk.at + 1) % count,
            false => (walk.at + count - 1) % count,
        };
        self.show_definition(at).map(Some)
    }

    /// The block around the line the file view opened at, "Haystack > path".
    pub fn file_view_place(&self) -> Option<String> {
        let Some(Overlay::File {
            path,
            line: Some(line),
            ..
        }) = self.overlays.last()
        else {
            return None;
        };
        let head = &self.loaded()?.head_oid;
        let outline = self.outlines.get(&(head.clone(), path.clone()))?.as_ref()?;
        outline.place(*line)
    }

    /// How many lines of the changed files hold the find text; None while git looks.
    pub fn diff_match_count(&self) -> Option<usize> {
        let Some(Ok(files)) = &self.find_files else {
            return None;
        };
        Some(files.iter().map(|(_, count)| count).sum())
    }

    /// t in select lines: opens or closes every review thread in the file.
    fn toggle_threads(&mut self) -> Result<(), String> {
        let mode = self.line_mode.as_ref().ok_or("select lines first")?;
        let Some(Ok(activity)) = &self.activity else {
            return Err("the review threads are still loading".to_string());
        };
        if activity.threads_on(&mode.path).next().is_none() {
            return Err("this file has no review threads".to_string());
        }
        self.threads_open = !self.threads_open;
        self.flipped_threads.clear();
        self.chosen_thread = None;
        self.line_followed = None;
        Ok(())
    }

    /// Whether a review thread shows every comment in select lines.
    pub fn thread_open(&self, id: &str) -> bool {
        self.threads_open != self.flipped_threads.contains(id)
    }

    /// A click on a thread in select lines opens or closes that one thread. `index` is
    /// its place among the file's threads.
    fn flip_thread(&mut self, index: usize) {
        let (Some(mode), Some(Ok(activity))) = (&self.line_mode, &self.activity) else {
            return;
        };
        let Some(thread) = activity.threads_on(&mode.path).nth(index) else {
            return;
        };
        let id = thread.id.clone();
        self.chosen_thread = Some(id.clone());
        if !self.flipped_threads.remove(&id) {
            self.flipped_threads.insert(id);
        }
    }

    /// The review thread r and u act on: the one on the cursor's line, an outdated one
    /// with the cursor on the first row, or the one clicked when there are several.
    fn thread_here(&self) -> Result<&crate::model::ThreadLine, String> {
        let mode = self.line_mode.as_ref().ok_or("select lines first")?;
        let Some(Ok(rows)) = &mode.rows else {
            return Err("the diff is still loading".to_string());
        };
        let Some(Ok(activity)) = &self.activity else {
            return Err("the review threads are still loading".to_string());
        };
        let row = rows.get(mode.cursor).ok_or("no line here")?;
        let mut here = self.threads_on_row(&mode.path, row);
        if here.is_empty() && mode.cursor == 0 {
            here = activity
                .threads_on(&mode.path)
                .filter(|thread| {
                    !thread
                        .at
                        .is_some_and(|at| rows.iter().any(|row| row_holds(row, at)))
                })
                .collect();
        }
        if let [thread] = here[..] {
            return Ok(thread);
        }
        if here.is_empty() {
            return Err("no review thread on this line; ] goes to the next".to_string());
        }
        let chosen = self.chosen_thread.as_deref();
        here.into_iter()
            .find(|thread| Some(thread.id.as_str()) == chosen)
            .ok_or_else(|| "several threads here; click the one you mean".to_string())
    }

    /// u in select lines: resolves the thread here, or opens it again.
    fn resolve_thread(&self) -> Result<Option<Action>, String> {
        let thread = self.thread_here()?;
        Ok(Some(Action::ResolveThread {
            pr: self.pr.clone(),
            thread: thread.id.clone(),
            resolved: !thread.resolved,
        }))
    }

    /// ] and [ in select lines: the cursor goes to the next or previous line that holds a
    /// review thread.
    fn jump_to_thread(&mut self, forward: bool) -> Result<(), String> {
        let mode = self.line_mode.as_ref().ok_or("select lines first")?;
        let Some(Ok(rows)) = &mode.rows else {
            return Err("the diff is still loading".to_string());
        };
        let holds = |index: &usize| !self.threads_on_row(&mode.path, &rows[*index]).is_empty();
        let found = match forward {
            true => (mode.cursor + 1..rows.len()).find(holds),
            false => (0..mode.cursor).rev().find(holds),
        };
        let outdated = !forward && found.is_none() && self.has_outdated_threads();
        let direction = if forward { "below" } else { "above" };
        let index = match outdated {
            true => 0,
            false => found.ok_or(format!("no review thread {direction}"))?,
        };
        if let Some(mode) = &mut self.line_mode {
            mode.cursor = index;
            // The outdated threads sit above the first row: the top of the pane shows
            // them, even when they are taller than the pane.
            if outdated {
                mode.scroll = 0;
                self.line_followed = Some(0);
            }
        }
        Ok(())
    }

    /// Whether select lines lists threads above the diff, as their lines are gone from it.
    fn has_outdated_threads(&self) -> bool {
        let (Some(mode), Some(Ok(activity))) = (&self.line_mode, &self.activity) else {
            return false;
        };
        let Some(Ok(rows)) = &mode.rows else {
            return false;
        };
        activity.threads_on(&mode.path).any(|thread| {
            !thread
                .at
                .is_some_and(|at| rows.iter().any(|row| row_holds(row, at)))
        })
    }

    /// The review threads on a row of a file's diff.
    pub fn threads_on_row<'a>(
        &'a self,
        path: &'a str,
        row: &DiffRow,
    ) -> Vec<&'a crate::model::ThreadLine> {
        let Some(Ok(activity)) = &self.activity else {
            return Vec::new();
        };
        activity
            .threads_on(path)
            .filter(|thread| thread.at.is_some_and(|at| row_holds(row, at)))
            .collect()
    }

    /// The file's name at the base: its old name when the PR renamed it.
    fn base_path(&self, path: &str) -> String {
        match &self.repo {
            RepoState::Ready(repo) => repo.renames.get(path).cloned(),
            _ => None,
        }
        .unwrap_or_else(|| path.to_string())
    }

    fn head_in_clone(&self) -> Result<(PathBuf, String), String> {
        let RepoState::Ready(repo) = &self.repo else {
            return Err(
                self.clone_not_ready("the local clone is not ready; open Files changed first")
            );
        };
        let detail = self.loaded().ok_or("the pull request is still loading")?;
        Ok((repo.dir.clone(), detail.head_oid.clone()))
    }

    fn open_file(&mut self, path: String, line: usize) -> Result<Action, String> {
        let (dir, commit) = self.head_in_clone()?;
        // Show a few lines of context above the marked line.
        let scroll = u16::try_from(line.saturating_sub(6)).unwrap_or(u16::MAX);
        self.overlays.push(Overlay::File {
            path: path.clone(),
            lines: None,
            scroll,
            line: Some(line),
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
            return Err(self.clone_not_ready("the local clone is not ready yet"));
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
            line: None,
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
                self.repo_hit = None;
                self.def_walk = None;
            }
            (Overlay::File { .. }, KeyCode::Char('n')) if self.def_walk.is_some() => {
                return self.next_definition(true);
            }
            (Overlay::File { .. }, KeyCode::Char('N')) if self.def_walk.is_some() => {
                return self.next_definition(false);
            }
            // n and N go on through the repo's matches while one is on screen.
            (Overlay::File { .. }, KeyCode::Char('n')) if self.repo_hit.is_some() => {
                return self.find_next(true);
            }
            (Overlay::File { .. }, KeyCode::Char('N')) if self.repo_hit.is_some() => {
                return self.find_next(false);
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
        self.find = Some(query.to_string());
        self.find_hit = None;
        self.find_files = None;
        self.find_repo = None;
        self.leave_repo_hits();
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
            query: query.to_string(),
        }))
    }

    /// Esc: ends the find, its marks and its counts.
    fn clear_find(&mut self) {
        self.find = None;
        self.find_hit = None;
        self.find_files = None;
        self.find_repo = None;
        self.find_repo_capped = false;
        self.find_pending = None;
        self.leave_repo_hits();
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
    /// (or with none here) it goes on to the next file that holds the text. Past the last
    /// file it goes on to the matches in the rest of the repo at the PR head, then back.
    fn find_next(&mut self, forward: bool) -> Result<Option<Action>, String> {
        if let Some(hit) = self.repo_hit {
            return self.next_repo_hit(hit, forward);
        }
        let in_file = self.tab == Tab::Files && matches!(self.find_files, Some(Ok(_)));
        if !in_file {
            return self.find_in_diff(forward, true).map(|()| None);
        }
        let wraps = match self.current_find_hit() {
            Some(hit) if forward => hit.ordinal == hit.total,
            Some(hit) => hit.ordinal == 1,
            None => true,
        };
        if !wraps {
            return self.find_in_diff(forward, true).map(|()| None);
        }
        let hits = self.repo_hit_count();
        if hits > 0 && self.at_last_find_file(forward) {
            let first = if forward { 0 } else { hits - 1 };
            return self.show_repo_hit(first).map(Some);
        }
        self.next_find_file(forward).map(|()| None)
    }

    /// The number of repo matches n can walk; none while git looks or after it failed.
    fn repo_hit_count(&self) -> usize {
        match &self.find_repo {
            Some(Ok(hits)) => hits.len(),
            _ => 0,
        }
    }

    /// The selected file is the last (or, going back, the first) of the files that hold
    /// the find text, or none of them does.
    fn at_last_find_file(&self, forward: bool) -> bool {
        let order = self.find_order();
        let current = self.selected_file();
        let edge = if forward { order.last() } else { order.first() };
        order.is_empty() || (current.is_some() && edge == current.as_ref())
    }

    /// Moves along the repo's matches. Past either end it goes back to the diff: n to its
    /// first matching file, N to its last.
    fn next_repo_hit(&mut self, hit: usize, forward: bool) -> Result<Option<Action>, String> {
        let next = if forward {
            hit.checked_add(1)
                .filter(|next| *next < self.repo_hit_count())
        } else {
            hit.checked_sub(1)
        };
        if let Some(next) = next {
            return self.show_repo_hit(next).map(Some);
        }
        self.leave_repo_hits();
        if self.find_order().is_empty() {
            let query = self.find.clone().unwrap_or_default();
            return Err(format!(
                "\"{query}\" is in no changed file; n goes on through the repo"
            ));
        }
        self.select_file_row(None);
        self.next_find_file(forward).map(|()| None)
    }

    /// Opens the file of a repo match at the PR head, at its line.
    fn show_repo_hit(&mut self, index: usize) -> Result<Action, String> {
        let Some(Ok(hits)) = &self.find_repo else {
            return Err("the repo search has not finished".to_string());
        };
        let hit = hits.get(index).ok_or("no such repo match")?.clone();
        self.leave_repo_hits();
        let action = self.open_file(hit.path, hit.line)?;
        self.repo_hit = Some(index);
        Ok(action)
    }

    /// Closes the file view of a repo match, if one is open.
    fn leave_repo_hits(&mut self) {
        if self.repo_hit.take().is_some() {
            self.overlays.pop();
        }
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
    /// v on a file flips it; on a folder, marks every file under it viewed, or unmarks
    /// them all when every one is viewed already.
    fn toggle_viewed(&mut self) -> Option<Action> {
        let row = self.file_rows().get(self.file_state.selected()?)?.clone();
        match row {
            FileRow::File { index, .. } => self.toggle_file_viewed(index),
            FileRow::Dir { path, .. } => self.toggle_folder_viewed(&path),
        }
    }

    fn toggle_folder_viewed(&mut self, folder: &str) -> Option<Action> {
        let pull_request_id = self.loaded()?.id.clone();
        let prefix = format!("{folder}/");
        let files = self.files.as_mut()?.as_mut().ok()?;
        let mut under: Vec<_> = files
            .iter_mut()
            .filter(|file| file.path.starts_with(&prefix))
            .collect();
        let viewed = under.iter().any(|file| file.viewed != ViewedState::Viewed);
        let target = if viewed {
            ViewedState::Viewed
        } else {
            ViewedState::Unviewed
        };
        let mut paths = Vec::new();
        for file in under.iter_mut().filter(|file| file.viewed != target) {
            note_viewed_write(&mut self.viewed_sync, &file.path, file.viewed);
            file.viewed = target;
            paths.push(file.path.clone());
        }
        // A folder that hides its viewed files may vanish; the cursor stays in the list.
        self.clamp_file_selection();
        (!paths.is_empty()).then(|| Action::SetViewed {
            pr: self.pr.clone(),
            pull_request_id,
            paths,
            viewed,
        })
    }

    fn toggle_file_viewed(&mut self, index: usize) -> Option<Action> {
        let pull_request_id = self.loaded()?.id.clone();
        let file = self.files.as_mut()?.as_mut().ok()?.get_mut(index)?;
        let viewed = file.viewed != ViewedState::Viewed;
        note_viewed_write(&mut self.viewed_sync, &file.path, file.viewed);
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
            paths: vec![path],
            viewed,
        })
    }

    /// A Viewed write came back. A failed one puts each file back to GitHub's last
    /// confirmed state, unless a later write for it is still out.
    fn settle_viewed(&mut self, paths: &[String], viewed: bool, saved: bool) {
        let mut restore = Vec::new();
        for path in paths {
            let Some(sync) = self.viewed_sync.get_mut(path) else {
                continue;
            };
            sync.pending = sync.pending.saturating_sub(1);
            if saved {
                sync.confirmed = match viewed {
                    true => ViewedState::Viewed,
                    false => ViewedState::Unviewed,
                };
            }
            if sync.pending > 0 {
                continue;
            }
            if !saved {
                restore.push((path.clone(), sync.confirmed));
            }
            self.viewed_sync.remove(path);
        }
        let Some(Ok(files)) = self.files.as_mut() else {
            return;
        };
        for (path, state) in restore {
            if let Some(file) = files.iter_mut().find(|file| file.path == path) {
                file.viewed = state;
            }
        }
    }

    /// The selected file's path, or the selected folder's with a trailing slash.
    fn selected_row_path(&self) -> Option<String> {
        match self.file_rows().get(self.file_state.selected()?)? {
            FileRow::File { index, .. } => Some(self.loaded_files()?.get(*index)?.path.clone()),
            FileRow::Dir { path, .. } => Some(format!("{path}/")),
        }
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
            Tab::Files => Some(self.files_link()),
        });
        item_url.unwrap_or_else(|| self.pr.url())
    }

    /// The PR's Files changed page, at the selected file, and at the cursor's line in
    /// select lines. GitHub names each file's anchor by the SHA-256 of its path, and a
    /// line by L (old side) or R (new side) and its number.
    fn files_link(&self) -> String {
        let page = format!("{}/files", self.pr.url());
        let file = self
            .loaded_files()
            .zip(self.selected_file())
            .and_then(|(files, index)| files.get(index));
        let Some(file) = file else {
            return page;
        };
        let anchor = format!("{page}#diff-{}", crate::sha256::hex(file.path.as_bytes()));
        let line = self
            .line_mode
            .as_ref()
            .filter(|mode| mode.path == file.path)
            .and_then(|mode| match &mode.rows {
                Some(Ok(rows)) => rows.get(mode.cursor)?.target(),
                _ => None,
            });
        match line {
            Some((Side::Left, number)) => format!("{anchor}L{number}"),
            Some((Side::Right, number)) => format!("{anchor}R{number}"),
            None => anchor,
        }
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
            Click::Thread(index) => self.flip_thread(index),
            // A file view over the tab takes no row clicks.
            Click::Row(_) if !self.overlays.is_empty() => return Ok(None),
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

    /// One wheel tick or j/k on the Overview: to the next card or fold when its top is on
    /// screen, since everything before it has then been seen; otherwise a third of a
    /// screen, so a long card is read through, not jumped over.
    fn wheel_overview(&mut self, forward: bool) {
        let chunk = (self.page / 3).max(1);
        let on_screen = |row: usize| (self.scroll..self.scroll + self.page).contains(&row);
        let before = self.scroll;
        match self.anchor_target(forward) {
            Some(target) if on_screen(target) => {
                self.jump_anchor(forward);
                // The page only follows the key's direction, never back over read text.
                self.scroll = if forward {
                    self.scroll.max(before)
                } else {
                    self.scroll.min(before)
                };
            }
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
    /// D in select lines: the name to look up, with the line's names to pick from.
    Definition {
        names: Vec<String>,
    },
    Workspace,
    FindInDiff,
    UpdatedWithin,
    /// A custom editor command; `open` as in `Action::SetEditor`.
    Editor {
        open: Option<EditorTarget>,
    },
    /// Asks before deleting your pending review and its line comments.
    ConfirmDiscard {
        pr: PrRef,
        pending_review: String,
    },
    /// Asks before a merge write, which cannot be undone.
    ConfirmMerge {
        pr: PrRef,
        pull_request_id: String,
        head_oid: String,
        request: MergeRequest,
        base: String,
        bypass: bool,
    },
    /// Asks before cloning the PR's repo into this folder.
    ConfirmClone {
        pr: PrRef,
        dir: PathBuf,
    },
}

impl Action {
    /// For a write to GitHub: what the footer says while it runs, and once it succeeds.
    pub fn save_note(&self) -> Option<SaveNote> {
        let (doing, done) = match self {
            Action::SubmitReview { .. } => ("sending your review to GitHub...", "review sent"),
            Action::AddComment { .. } => (
                "adding the comment to your pending review...",
                "comment added to your pending review; R submits it",
            ),
            Action::SetLabels { .. } => ("saving labels to GitHub...", "labels saved"),
            Action::Merge { request, .. } => match request {
                MergeRequest::Now(_) => ("merging on GitHub...", "merged"),
                MergeRequest::Auto(_) => (
                    "turning auto-merge on...",
                    "auto-merge on; GitHub merges it once the requirements pass",
                ),
                MergeRequest::DisableAuto => ("turning auto-merge off...", "auto-merge off"),
            },
            Action::DiscardReview { .. } => (
                "discarding your pending review...",
                "pending review discarded",
            ),
            Action::ReplyToThread { pending: true, .. } => (
                "adding the reply to your pending review...",
                "reply added to your pending review; R submits it",
            ),
            Action::ReplyToThread { .. } => ("posting the reply on GitHub...", "reply posted"),
            Action::ResolveThread { resolved: true, .. } => {
                ("resolving the thread...", "thread resolved")
            }
            Action::ResolveThread { .. } => ("reopening the thread...", "thread reopened"),
            _ => return None,
        };
        let reload_on_error = matches!(self, Action::SetLabels { .. });
        Some(SaveNote {
            doing,
            done,
            reload_on_error,
        })
    }
}

/// The footer's words for a write in progress and for its success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SaveNote {
    pub doing: &'static str,
    pub done: &'static str,
    /// A failure may still have changed GitHub, so the PR loads again to show it.
    pub reload_on_error: bool,
}

/// The label checklist: the repo's labels, ticked for the ones the PR will have.
#[derive(Debug)]
pub struct LabelMenu {
    pub pr: PrRef,
    pub pull_request_id: String,
    /// `None` while the repo's labels load.
    pub labels: Option<Result<Vec<RepoLabel>, String>>,
    /// The PR's labels when the menu opened.
    pub current: HashSet<String>,
    /// The labels the PR will have after enter.
    pub chosen: HashSet<String>,
    /// Typed text; only labels holding it show.
    pub filter: String,
    /// The cursor, among the shown labels.
    pub selected: usize,
}

impl LabelMenu {
    /// The labels holding the filter, ignoring case.
    pub fn shown(&self) -> Vec<&RepoLabel> {
        let Some(Ok(labels)) = &self.labels else {
            return Vec::new();
        };
        let filter = self.filter.to_lowercase();
        let holds = |label: &&RepoLabel| label.name.to_lowercase().contains(&filter);
        labels.iter().filter(holds).collect()
    }

    fn toggle(&mut self) {
        let Some(name) = self
            .shown()
            .get(self.selected)
            .map(|label| label.name.clone())
        else {
            return;
        };
        if !self.chosen.remove(&name) {
            self.chosen.insert(name);
        }
    }

    /// The adds and removes, by label id; nothing when the ticks match the PR.
    fn change(self) -> Option<Action> {
        let Some(Ok(labels)) = &self.labels else {
            return None;
        };
        let ids = |names: Vec<&String>| -> Vec<String> {
            let id_of = |name: &&String| labels.iter().find(|label| &&label.name == name);
            names
                .iter()
                .filter_map(id_of)
                .map(|label| label.id.clone())
                .collect()
        };
        let add = ids(self.chosen.difference(&self.current).collect());
        let remove = ids(self.current.difference(&self.chosen).collect());
        if add.is_empty() && remove.is_empty() {
            return None;
        }
        Some(Action::SetLabels {
            pr: self.pr,
            pull_request_id: self.pull_request_id,
            add,
            remove,
        })
    }
}

/// The next renderer whose command is installed, or the next one when none is known to
/// be.
fn next_renderer(current: usize, count: usize, missing: &HashSet<usize>) -> usize {
    (1..=count)
        .map(|step| (current + step) % count)
        .find(|index| !missing.contains(index))
        .unwrap_or((current + 1) % count)
}

/// Why a review can only be a Comment: GitHub refuses your Approve and Request changes
/// on your own PR, and a verdict means nothing once the PR is merged or closed.
fn comment_only_reason(detail: &PrDetail) -> Option<&'static str> {
    match detail.lifecycle {
        Lifecycle::Merged { .. } => Some("the PR is merged, so only Comment applies"),
        Lifecycle::Closed { .. } => Some("the PR is closed, so only Comment applies"),
        _ if detail.viewer_is_author => Some("GitHub takes only Comment on your own PR"),
        _ => None,
    }
}

/// Counts a Viewed write for the file; the first one out keeps the state GitHub had.
fn note_viewed_write(sync: &mut HashMap<String, ViewedSync>, path: &str, before: ViewedState) {
    let entry = sync.entry(path.to_string()).or_insert(ViewedSync {
        confirmed: before,
        pending: 0,
    });
    entry.pending += 1;
}

/// A row of the merge menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeItem {
    Method(MergeMethod),
    /// Ticks merging now past branch protection, for an admin.
    Bypass,
    DisableAuto,
}

/// The merge menu: GitHub's merge button and its dropdown.
#[derive(Debug)]
pub struct MergeMenu {
    pub pr: PrRef,
    pub pull_request_id: String,
    pub head_oid: String,
    pub base: String,
    pub commit_count: u64,
    pub items: Vec<MergeItem>,
    pub default_method: Option<MergeMethod>,
    /// The PR can merge now; otherwise a method turns auto-merge on.
    pub ready: bool,
    pub can_auto_merge: bool,
    /// Bypass requirements is ticked.
    pub bypass: bool,
    pub selected: usize,
    /// Why Enter did nothing, such as a blocked merge with no way past it.
    pub note: Option<&'static str>,
}

impl MergeMenu {
    /// The menu for the PR, or why GitHub would not merge it from here.
    pub fn new(pr: &PrRef, detail: &PrDetail) -> Result<MergeMenu, String> {
        let items = merge_items(detail)?;
        let merge = &detail.merge;
        let selected = items
            .iter()
            .position(|item| {
                merge
                    .default_method
                    .is_some_and(|method| item == &MergeItem::Method(method))
            })
            .unwrap_or(0);
        Ok(MergeMenu {
            pr: pr.clone(),
            pull_request_id: detail.id.clone(),
            head_oid: detail.head_oid.clone(),
            base: detail.base.clone(),
            commit_count: detail.commit_count,
            items,
            default_method: merge.default_method,
            ready: merges_now(detail),
            can_auto_merge: merge.can_enable_auto_merge,
            bypass: false,
            selected,
            note: None,
        })
    }

    /// What Enter on the selected row asks to send, if anything.
    fn request(&self) -> Result<(MergeRequest, bool), &'static str> {
        let method = match self.items[self.selected] {
            MergeItem::Method(method) => method,
            MergeItem::DisableAuto => return Ok((MergeRequest::DisableAuto, false)),
            MergeItem::Bypass => return Err("enter ticks bypass"),
        };
        if self.bypass {
            return Ok((MergeRequest::Now(method), true));
        }
        if self.ready {
            return Ok((MergeRequest::Now(method), false));
        }
        if self.can_auto_merge {
            return Ok((MergeRequest::Auto(method), false));
        }
        Err("GitHub blocks the merge; tick Bypass requirements to merge anyway")
    }
}

/// Whether GitHub would merge the PR now, as its green merge button says.
fn merges_now(detail: &PrDetail) -> bool {
    matches!(
        detail.merge_state.as_str(),
        "CLEAN" | "HAS_HOOKS" | "UNSTABLE"
    )
}

/// The merge menu's rows, as GitHub's merge box offers them, or why there are none.
fn merge_items(detail: &PrDetail) -> Result<Vec<MergeItem>, String> {
    let merge = &detail.merge;
    match detail.lifecycle {
        Lifecycle::Merged { .. } => return Err("the PR is merged already".to_string()),
        Lifecycle::Closed { .. } => return Err("the PR is closed".to_string()),
        Lifecycle::Draft => {
            return Err("the PR is a draft; mark it ready for review first".to_string());
        }
        Lifecycle::Open => {}
    }
    if merge.auto_merge.is_some() {
        if merge.can_disable_auto_merge {
            return Ok(vec![MergeItem::DisableAuto]);
        }
        return Err("auto-merge is on; GitHub merges it once the requirements pass".to_string());
    }
    if !merge.can_write && !merge.can_bypass {
        return Err("you cannot merge in this repo".to_string());
    }
    // GitHub checks again after every push to either branch, and its merge button
    // waits until it knows.
    if matches!(detail.merge_state.as_str(), "UNKNOWN" | "") {
        return Err(
            "GitHub is still checking whether it can merge; r fetches it again".to_string(),
        );
    }
    if detail.merge_state == "DIRTY" {
        return Err("the branch has conflicts; resolve them before it can merge".to_string());
    }
    if merge.methods.is_empty() {
        return Err("GitHub reports no merge method for this repo".to_string());
    }
    let ready = merges_now(detail);
    if !ready && !merge.can_enable_auto_merge && !merge.can_bypass {
        return Err("GitHub blocks the merge, and auto-merge is off for this repo".to_string());
    }
    let mut items: Vec<MergeItem> = merge
        .methods
        .iter()
        .copied()
        .map(MergeItem::Method)
        .collect();
    if !ready && merge.can_bypass {
        items.push(MergeItem::Bypass);
    }
    Ok(items)
}

/// Select lines' state: the file's numbered rows, the cursor, and a range's other end.
#[derive(Debug)]
pub struct LineMode {
    pub path: String,
    /// `None` while git runs.
    pub rows: Option<Result<Vec<DiffRow>, String>>,
    pub cursor: usize,
    /// V marked this row; the range runs from here to the cursor.
    pub anchor: Option<usize>,
    /// The first screen row drawn, counting wrapped code and thread rows.
    pub scroll: usize,
}

impl LineMode {
    /// The new file's line at the cursor. A removed line or a hunk header has none, so
    /// the next line that has one, else the one before.
    fn editor_line(&self) -> Option<u64> {
        let Some(Ok(rows)) = &self.rows else {
            return None;
        };
        let after = rows.get(self.cursor..)?.iter().find_map(|row| row.new);
        after.or_else(|| {
            rows.get(..self.cursor)?
                .iter()
                .rev()
                .find_map(|row| row.new)
        })
    }

    /// Moves the cursor by `delta` rows, within the diff.
    pub fn move_cursor(&mut self, delta: isize) {
        let last = match &self.rows {
            Some(Ok(rows)) => rows.len().saturating_sub(1),
            _ => 0,
        };
        self.cursor = self.cursor.saturating_add_signed(delta).min(last);
    }

    /// The rows the cursor and the range cover, first to last.
    pub fn marked(&self) -> (usize, usize) {
        let anchor = self.anchor.unwrap_or(self.cursor);
        (anchor.min(self.cursor), anchor.max(self.cursor))
    }

    /// Where a comment on the marked rows goes. GitHub takes a range on one side only.
    fn target(&self) -> Result<LineTarget, String> {
        let Some(Ok(rows)) = &self.rows else {
            return Err("the diff is still loading".to_string());
        };
        let (first, last) = self.marked();
        // A binary file's diff has no rows at all.
        let Some(selected) = rows.get(first..=last) else {
            return Err("this file has no lines to comment on".to_string());
        };
        let targets: Vec<(Side, u64)> = selected.iter().filter_map(DiffRow::target).collect();
        let (Some(&(side, start)), Some(&(end_side, end))) = (targets.first(), targets.last())
        else {
            return Err("a hunk header takes no comment; move to a line".to_string());
        };
        if targets.iter().any(|(row_side, _)| *row_side != side) || end_side != side {
            return Err(
                "a range must stay on one side: only removed lines, or only new and unchanged ones"
                    .to_string(),
            );
        }
        Ok(LineTarget {
            path: self.path.clone(),
            side,
            line: end,
            start_line: (start != end).then_some(start),
        })
    }
}

/// Where a line comment goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineTarget {
    pub path: String,
    pub side: Side,
    pub line: u64,
    /// The first line of a range; None for one line.
    pub start_line: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposeKind {
    /// `comment_only`: why only Comment applies, on your own PR or a finished one.
    Review {
        verdict: Verdict,
        comment_only: Option<&'static str>,
        drafts: usize,
    },
    LineComment(LineTarget),
    /// A reply to a review thread: its ID, and who started it and where, for the title.
    Reply {
        thread: String,
        author: String,
        place: String,
    },
    /// A request to a coding agent, with the context revu attaches.
    Handoff(crate::handoff::Context),
}

/// The text box for a review or a line comment, and what it sends.
#[derive(Debug)]
pub struct Composer {
    pub pr: PrRef,
    pub pull_request_id: String,
    /// The head commit the diff showed. A review revu creates is pinned to it, so it
    /// covers what you saw, not commits pushed since.
    pub head_oid: String,
    pub pending_review: Option<String>,
    pub kind: ComposeKind,
    pub editor: Editor,
    pub error: Option<String>,
    /// ctrl+s was pressed; y sends.
    pub confirming: bool,
    /// The request is on its way; keys wait for it.
    pub sending: bool,
    /// esc was pressed on text; a second esc throws it away.
    pub cancel_asked: bool,
    /// The diff's words, which Tab completes the word at the cursor to. Handoff only.
    pub words: Vec<String>,
}

impl Composer {
    pub fn new(pr: PrRef, detail: &PrDetail, activity: &PrActivity, kind: ComposeKind) -> Self {
        Composer {
            pr,
            pull_request_id: detail.id.clone(),
            head_oid: detail.head_oid.clone(),
            pending_review: activity.pending_review.clone(),
            kind,
            editor: Editor::default(),
            error: None,
            confirming: false,
            sending: false,
            cancel_asked: false,
            words: Vec::new(),
        }
    }

    /// The box for a request to an agent. It writes nothing to GitHub, so it needs
    /// no review state.
    pub fn handoff(pr: PrRef, detail: &PrDetail, context: crate::handoff::Context) -> Self {
        Composer {
            pr,
            pull_request_id: detail.id.clone(),
            head_oid: detail.head_oid.clone(),
            pending_review: None,
            kind: ComposeKind::Handoff(context),
            editor: Editor::default(),
            error: None,
            confirming: false,
            sending: false,
            cancel_asked: false,
            words: Vec::new(),
        }
    }

    /// The rest of the word that ends at the cursor, from the diff's words. None when
    /// the cursor is inside a word or after a space, or the box is not a handoff.
    pub fn suggestion(&self) -> Option<String> {
        if !matches!(self.kind, ComposeKind::Handoff(_)) || self.editor.anchor.is_some() {
            return None;
        }
        let (before, after) = self.editor.text.split_at(self.editor.cursor);
        if after.starts_with(|next: char| !next.is_whitespace()) {
            return None;
        }
        let typed = before.rsplit(char::is_whitespace).next().unwrap_or("");
        complete(&self.words, typed)
    }

    /// Tab: replaces the typed start with the whole word, spelled as in the diff.
    fn accept_suggestion(&mut self) -> bool {
        if self.suggestion().is_none() {
            return false;
        }
        let before = &self.editor.text[..self.editor.cursor];
        let typed = before.rsplit(char::is_whitespace).next().unwrap_or("");
        let Some((word, _)) = matching_word(&self.words, typed) else {
            return false;
        };
        let word = word.to_string();
        self.editor.anchor = Some(self.editor.cursor - typed.len());
        self.editor.insert(&word);
        true
    }

    /// Tab: Comment, Approve, Request changes, in turn, unless only Comment applies.
    fn next_verdict(&mut self) {
        if let ComposeKind::Review {
            verdict,
            comment_only: None,
            ..
        } = &mut self.kind
        {
            *verdict = match verdict {
                Verdict::Comment => Verdict::Approve,
                Verdict::Approve => Verdict::RequestChanges,
                Verdict::RequestChanges => Verdict::Comment,
            };
        }
    }

    /// What GitHub would refuse, caught before sending.
    fn check(&self) -> Result<(), String> {
        let empty = self.editor.text.trim().is_empty();
        match &self.kind {
            ComposeKind::Review {
                verdict: Verdict::RequestChanges,
                ..
            } if empty => Err("Request changes needs a comment saying what to change".to_string()),
            ComposeKind::Review {
                verdict: Verdict::Comment,
                drafts: 0,
                ..
            } if empty => Err("write a comment, or add line comments first".to_string()),
            ComposeKind::LineComment(_) if empty => Err("write the comment first".to_string()),
            ComposeKind::Reply { .. } if empty => Err("write the reply first".to_string()),
            ComposeKind::Handoff(_) if empty => {
                Err("write what you want the agent to do".to_string())
            }
            _ => Ok(()),
        }
    }

    /// The question ctrl+s asks before sending.
    pub fn confirm_text(&self) -> String {
        match &self.kind {
            ComposeKind::Review {
                verdict, drafts, ..
            } => {
                let drafts = match drafts {
                    0 => String::new(),
                    1 => " with 1 line comment".to_string(),
                    count => format!(" with {count} line comments"),
                };
                format!(
                    "send {}{drafts} to GitHub? y sends, any other key goes back",
                    verdict.label()
                )
            }
            ComposeKind::LineComment(_) => {
                "add this to your pending review on GitHub? y adds it, any other key goes back"
                    .to_string()
            }
            ComposeKind::Reply { .. } if self.pending_review.is_some() => {
                "add this reply to your pending review on GitHub? y adds it, any other key goes back"
                    .to_string()
            }
            ComposeKind::Reply { .. } => {
                "post this reply on GitHub? y posts it, any other key goes back".to_string()
            }
            // ctrl+s opens the agent list, which is the confirm step.
            ComposeKind::Handoff(_) => String::new(),
        }
    }

    fn action(&self) -> Action {
        let body = self.editor.text.trim_end().to_string();
        let (pr, pull_request_id, pending_review) = (
            self.pr.clone(),
            self.pull_request_id.clone(),
            self.pending_review.clone(),
        );
        let head_oid = self.head_oid.clone();
        match &self.kind {
            ComposeKind::Review { verdict, .. } => Action::SubmitReview {
                pr,
                pull_request_id,
                head_oid,
                pending_review,
                verdict: *verdict,
                body,
            },
            ComposeKind::LineComment(target) => Action::AddComment {
                pr,
                pull_request_id,
                head_oid,
                pending_review,
                target: target.clone(),
                body,
            },
            ComposeKind::Reply { thread, .. } => Action::ReplyToThread {
                pr,
                thread: thread.clone(),
                body,
                pending: pending_review.is_some(),
            },
            ComposeKind::Handoff(context) => Action::PickAgent {
                message: crate::handoff::message(context, &body),
                dir: context.dir.clone(),
            },
        }
    }
}

/// The agents a handoff can go to, running sessions first, and the message it sends.
#[derive(Debug)]
pub struct AgentMenu {
    pub targets: Vec<crate::handoff::Target>,
    pub selected: usize,
    pub message: String,
    pub dir: Option<PathBuf>,
}

/// A row of the settings panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    Theme,
    Workspace,
    UpdatedWithin,
    Editor,
}

pub const SETTINGS: [Setting; 4] = [
    Setting::Theme,
    Setting::Workspace,
    Setting::UpdatedWithin,
    Setting::Editor,
];

/// A row of the editor list in settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorChoice {
    /// No editor key: $VISUAL, then $EDITOR, then vi.
    Shell,
    Known {
        label: &'static str,
        program: &'static str,
    },
    /// A command the user types.
    Custom,
}

/// The editor list: the shell's editor, the known editors on PATH, then a custom command.
#[derive(Debug)]
pub struct EditorMenu {
    /// The shell's editor, which the first row names.
    pub shell: String,
    /// The file to open once the user picks, when o asked for the editor.
    pub open: Option<EditorTarget>,
    pub choices: Vec<EditorChoice>,
    /// The choice the config has now, marked in the list.
    pub current: usize,
    pub selected: usize,
}

impl EditorMenu {
    pub fn new(
        shell_program: &str,
        config: Option<&EditorConfig>,
        installed: impl Fn(&str) -> bool,
    ) -> Self {
        let known = crate::editor_command::KNOWN_EDITORS
            .iter()
            .filter(|(_, program)| *program != shell_program && installed(program))
            .map(|&(label, program)| EditorChoice::Known { label, program });
        let choices: Vec<EditorChoice> = std::iter::once(EditorChoice::Shell)
            .chain(known)
            .chain(std::iter::once(EditorChoice::Custom))
            .collect();
        let wanted = match config {
            None => EditorChoice::Shell,
            Some(config) => choices
                .iter()
                .copied()
                .find(|choice| match choice {
                    EditorChoice::Known { program, .. } => {
                        crate::editor_command::config_for(program) == *config
                    }
                    _ => false,
                })
                .unwrap_or(EditorChoice::Custom),
        };
        let current = choices
            .iter()
            .position(|choice| *choice == wanted)
            .unwrap_or(0);
        Self {
            shell: shell_program.to_string(),
            open: None,
            choices,
            current,
            selected: current,
        }
    }
}

impl Setting {
    pub fn label(self) -> &'static str {
        match self {
            Setting::Theme => "Theme",
            Setting::Workspace => "Workspace folders",
            Setting::UpdatedWithin => "Updated within",
            Setting::Editor => "Editor",
        }
    }
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
        if self.text.contains(char::is_whitespace) {
            return None;
        }
        complete(&self.words, &self.text)
    }
}

/// The rest of the first word longer than `typed` that starts with it, ignoring case.
fn complete(words: &[String], typed: &str) -> Option<String> {
    matching_word(words, typed).map(|(_, rest)| rest.to_string())
}

/// The first word longer than `typed` that starts with it, ignoring case, and the part
/// of it after the typed text. The split is found on the word itself, as lowercasing
/// can change how many characters a letter takes.
fn matching_word<'a>(words: &'a [String], typed: &str) -> Option<(&'a str, &'a str)> {
    if typed.is_empty() {
        return None;
    }
    let typed = typed.to_lowercase();
    words.iter().find_map(|word| {
        let split = word
            .char_indices()
            .map(|(index, _)| index)
            .skip(1)
            .find(|&index| word[..index].to_lowercase() == typed)?;
        Some((word.as_str(), &word[split..]))
    })
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
    /// The help's first visible line; j/k scroll it.
    pub help_scroll: u16,
    /// What a mouse click can hit on the inbox, from the last draw.
    pub inbox_clicks: Vec<ClickTarget>,
    /// GitHub's API budget after the latest call.
    pub rate_limit: Option<crate::model::RateLimit>,
    /// The theme menu is open, with this entry of `THEME_NAMES` selected.
    pub theme_menu: Option<usize>,
    /// The text box for a review or a line comment, while it is open.
    pub composer: Option<Composer>,
    /// The label checklist, while it is open.
    pub label_menu: Option<LabelMenu>,
    /// The merge menu, while it is open.
    pub merge_menu: Option<MergeMenu>,
    /// o's choice of GitHub or the editor, while it is open.
    pub open_menu: Option<OpenMenu>,
    /// An action a reply from a worker started, for the main loop to run.
    pub queued: Option<Action>,
    /// The number of the latest detail load, across every PR opened.
    next_load: u64,
    /// The clone lookup's answer, for the main loop to act on.
    pub clone_lookup: Option<(RepoJob, Result<crate::repo::CloneLookup, String>)>,
    /// Renderers whose command is not installed; m skips them.
    pub missing_renderers: HashSet<usize>,
    /// A warning for the diff footer, such as a renderer that is not installed.
    pub notice: Option<String>,
    /// The notice was drawn; the next key clears it.
    pub notice_seen: bool,
    /// gh is missing or logged out; the whole screen says how to fix it.
    pub gh_problem: Option<crate::github::GhProblem>,
    /// When gh was last found broken, so r shows that it checked again.
    pub gh_checked_at: Option<DateTime<Utc>>,
    /// Each group's error from the latest fetch.
    group_errors: Vec<Option<String>>,
    /// A write to GitHub is running; the footer says what it saves.
    pub saving: Option<SaveNote>,
    /// The last write succeeded; the footer says so until the next key.
    pub saved: Option<String>,
    /// A write succeeded, so this PR loads again to show it.
    pub reload_after_write: Option<PrRef>,
    /// The settings panel's selected row, while it is open.
    pub settings_menu: Option<usize>,
    /// The config values the settings panel shows.
    pub workspace_dirs: Vec<String>,
    pub updated_within_days: Option<u32>,
    /// The theme in use, so the menu opens on it.
    pub theme_name: &'static str,
    /// One flag per group: true while its search is running.
    pub group_loading: Vec<bool>,
    inbox_generation: u64,
    /// Names of the configured diff renderers, in order; `m` cycles through them.
    pub renderer_names: Vec<String>,
    /// The editor o offers, for the menu to name it.
    pub editor_name: String,
    /// The config's editor key, for the settings panel.
    pub editor_config: Option<EditorConfig>,
    /// The editor list from settings, while it is open.
    pub editor_menu: Option<EditorMenu>,
    /// The agents a handoff can go to, while the list is open.
    pub agent_menu: Option<AgentMenu>,
    /// The view toggles a newly opened PR starts with; they follow the open PR.
    pub prefs: ViewPrefs,
    /// The image gallery, while it is open.
    pub gallery: Option<crate::gallery::Gallery>,
    /// How this terminal draws images, found when the first image loads.
    pub picker: Option<ratatui_image::picker::Picker>,
    /// Each PR's head and comment count when you last left it.
    pub seen: crate::seen::Seen,
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
            help_scroll: 0,
            inbox_clicks: Vec::new(),
            rate_limit: None,
            theme_menu: None,
            composer: None,
            label_menu: None,
            merge_menu: None,
            open_menu: None,
            queued: None,
            next_load: 0,
            clone_lookup: None,
            missing_renderers: HashSet::new(),
            notice: None,
            notice_seen: false,
            gh_problem: None,
            gh_checked_at: None,
            group_errors: Vec::new(),
            saving: None,
            saved: None,
            reload_after_write: None,
            settings_menu: None,
            workspace_dirs: Vec::new(),
            updated_within_days: None,
            theme_name: crate::theme::THEME_NAMES[0],
            group_loading: Vec::new(),
            inbox_generation: 0,
            renderer_names: Vec::new(),
            editor_name: crate::editor_command::editor_name(
                None,
                &crate::editor_command::shell_editor(),
            ),
            editor_config: None,
            editor_menu: None,
            agent_menu: None,
            prefs: ViewPrefs::default(),
            gallery: None,
            picker: None,
            seen: crate::seen::Seen::default(),
        }
    }

    /// The renderer to save. While the saved one is not installed, the view shows a
    /// stand-in, and the saved choice stays for when it is installed.
    fn kept_renderer(&self, shown: usize) -> Option<String> {
        let saved_is_missing = self.prefs.renderer.as_ref().is_some_and(|name| {
            let index = self.renderer_names.iter().position(|known| known == name);
            index.is_some_and(|index| self.missing_renderers.contains(&index))
        });
        match saved_is_missing {
            true => self.prefs.renderer.clone(),
            false => self.renderer_names.get(shown).cloned(),
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
            renderer: self.kept_renderer(view.renderer),
            tree_view: view.tree_view,
            hide_viewed: view.hide_viewed,
            file_list_hidden: view.file_list_hidden,
            editor_picked: self.prefs.editor_picked,
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
        if let Err(error) = &result
            && error.contains(crate::repo::NOT_INSTALLED)
        {
            self.missing_renderers.insert(key.renderer);
        }
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
        if let Some(composer) = &mut self.composer
            && composer.pr == *pr
            && matches!(composer.kind, ComposeKind::Handoff(_))
        {
            composer.words = words.clone();
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

    /// The repo's matches for a find, unless a newer find replaced it. Matches in the
    /// changed files are dropped: n already reaches them in the diff.
    pub fn set_find_repo(&mut self, pr: &PrRef, query: &str, result: Result<Vec<GrepHit>, String>) {
        let Some(view) = self.view_for(pr) else {
            return;
        };
        if view.find.as_deref() != Some(query) {
            return;
        }
        let changed: HashSet<String> = view
            .loaded_files()
            .map(|files| files.iter().map(|file| file.path.clone()).collect())
            .unwrap_or_default();
        view.find_repo_capped = matches!(&result, Ok(hits) if hits.len() == MAX_GREP_HITS);
        let result = result.map(|hits| {
            hits.into_iter()
                .filter(|hit| !changed.contains(&hit.path))
                .collect()
        });
        view.find_repo = Some(result);
    }

    /// Fills the file views that are still waiting for this path.
    pub fn set_file(&mut self, pr: &PrRef, path: &str, lines: Result<Text<'static>, String>) {
        let Some(view) = self.view_for(pr) else {
            return;
        };
        let width = usize::from(view.body_width);
        for overlay in &mut view.overlays {
            match overlay {
                Overlay::File {
                    path: shown,
                    lines: slot @ None,
                    scroll,
                    line,
                } if shown == path => {
                    if let (Ok(text), Some(line)) = (&lines, line) {
                        *scroll = context_scroll(text, line.saturating_sub(1), width);
                    }
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
        self.group_errors = vec![None; configs.len()];
        self.loading = !configs.is_empty();
        self.error = None;
        self.clamp_selection();
        self.inbox_generation += 1;
        self.inbox_generation
    }

    /// The errors, without repeats, when every group failed and none has rows from
    /// before; an inbox of empty groups would read as nothing to review.
    pub fn inbox_failure(&self) -> Option<Vec<String>> {
        let all_failed = !self.loading
            && !self.group_errors.is_empty()
            && self.group_errors.iter().all(Option::is_some)
            && self.groups.iter().all(|group| group.prs.is_empty());
        if !all_failed {
            return None;
        }
        let mut errors: Vec<String> = Vec::new();
        for error in self.group_errors.iter().flatten() {
            if !errors.contains(error) {
                errors.push(error.clone());
            }
        }
        Some(errors)
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
            Err(error) => {
                self.error = Some(format!("{}: {error}", self.groups[index].title));
                self.group_errors[index] = Some(error);
            }
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

    /// Whether this PR is the one on screen.
    pub fn shows(&mut self, pr: &PrRef) -> bool {
        self.view_for(pr).is_some()
    }

    fn view_for(&mut self, pr: &PrRef) -> Option<&mut DetailView> {
        match &mut self.screen {
            Screen::Detail(view) if view.pr == *pr => Some(view),
            _ => None,
        }
    }

    /// Starts a load of the open PR's detail and activity, and returns its number.
    pub fn start_load(&mut self, pr: &PrRef) -> Option<u64> {
        self.next_load += 1;
        let load = self.next_load;
        let view = self.view_for(pr)?;
        view.load = load;
        Some(load)
    }

    /// Whether a result belongs to the latest load of the open PR.
    fn is_latest_load(&mut self, pr: &PrRef, load: u64) -> bool {
        self.view_for(pr).is_some_and(|view| view.load == load)
    }

    pub fn set_loaded_detail(&mut self, pr: &PrRef, load: u64, result: Result<PrDetail, String>) {
        if self.is_latest_load(pr, load) {
            self.set_detail(pr, result);
        }
    }

    pub fn set_loaded_activity(
        &mut self,
        pr: &PrRef,
        load: u64,
        result: Result<PrActivity, String>,
    ) {
        if self.is_latest_load(pr, load) {
            self.set_activity(pr, result);
        }
    }

    pub fn set_loaded_hunks(
        &mut self,
        pr: &PrRef,
        load: u64,
        hunks: HashMap<String, Vec<HunkLine>>,
    ) {
        if self.is_latest_load(pr, load) {
            self.set_hunks(pr, hunks);
        }
    }

    pub fn set_detail(&mut self, pr: &PrRef, result: Result<PrDetail, String>) {
        let (prefs, renderer_names) = (self.prefs.clone(), self.renderer_names.clone());
        let Some(view) = self.view_for(pr) else {
            return;
        };
        let commits = |detail: &PrDetail| (detail.base_oid.clone(), detail.head_oid.clone());
        let before = match &view.detail {
            Some(Ok(detail)) => Some(commits(detail)),
            _ => None,
        };
        let after = result.as_ref().ok().map(commits);
        // New commits make the files, the clone's refs and every drawn diff stale, as
        // r would; the activity of this load stays.
        if before.is_some() && after.is_some() && before != after {
            let activity = view.activity.take();
            let load = view.load;
            Self::reload(view, &prefs, &renderer_names);
            view.activity = activity;
            view.load = load;
        }
        view.detail = Some(result);
        view.overview = None;
    }

    pub fn set_activity(&mut self, pr: &PrRef, result: Result<PrActivity, String>) {
        if let Some(view) = self.view_for(pr) {
            view.activity = Some(result);
            view.overview = None;
        }
    }

    /// The code under each thread, which arrives after the comments.
    pub fn set_hunks(&mut self, pr: &PrRef, hunks: HashMap<String, Vec<HunkLine>>) {
        if let Some(view) = self.view_for(pr)
            && let Some(Ok(activity)) = &mut view.activity
        {
            activity.set_code(&hunks);
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
        paths: &[String],
        viewed: bool,
        result: Result<(), String>,
    ) {
        if let Some(view) = self.view_for(pr) {
            view.settle_viewed(paths, viewed, result.is_ok());
        }
        let Err(error) = result else {
            return;
        };
        let what = match paths {
            [path] => path.clone(),
            _ => format!("{} files", paths.len()),
        };
        self.error = Some(format!("Viewed not saved for {what}: {error}"));
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
        } else if let Some(mode) = view.line_mode.as_mut().filter(|_| column >= view.diff_left) {
            // The mode keeps its rows on the cursor, so the wheel moves the cursor.
            mode.move_cursor(delta * 3);
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
        if self.prompt.is_some() || self.agent_menu.is_some() {
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
        let on_image = view.tab == Tab::Overview
            && matches!(target.click, Click::Page { .. })
            && view
                .overview
                .as_ref()
                .and_then(|layout| layout.rows.get(view.cursor))
                .is_some_and(|row| row.image.is_some());
        if on_image {
            return self.open_gallery();
        }
        self.report(result)
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        self.saved = None;
        if self.notice_seen {
            self.notice = None;
        }
        if self.gh_problem.is_some() {
            return match key.code {
                KeyCode::Char('q') => Some(Action::Quit),
                KeyCode::Char('r') => {
                    self.gh_problem = None;
                    Some(Action::Refresh)
                }
                _ => None,
            };
        }
        // The agent list opens over the handoff box, so it takes keys first.
        if self.agent_menu.is_some() {
            return self.agent_menu_key(key.code);
        }
        if self.composer.is_some() {
            return self.composer_key(key);
        }
        if self.gallery.is_some() {
            return self.gallery_key(key.code);
        }
        if self.label_menu.is_some() {
            return self.label_menu_key(key.code);
        }
        if self.merge_menu.is_some() {
            self.merge_menu_key(key.code);
            return None;
        }
        if self.open_menu.is_some() {
            return self.open_menu_key(key.code);
        }
        if self.prompt.is_some() {
            return self.prompt_key(key.code);
        }
        if self.show_help {
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => self.help_scroll += 1,
                KeyCode::Char('k') | KeyCode::Up => {
                    self.help_scroll = self.help_scroll.saturating_sub(1)
                }
                KeyCode::Char(' ') | KeyCode::PageDown => self.help_scroll += 10,
                KeyCode::Char('b') | KeyCode::PageUp => {
                    self.help_scroll = self.help_scroll.saturating_sub(10)
                }
                _ => self.show_help = false,
            }
            return None;
        }
        if self.theme_menu.is_some() {
            return self.theme_menu_key(key.code);
        }
        if self.editor_menu.is_some() {
            return self.editor_menu_key(key.code);
        }
        if self.settings_menu.is_some() {
            return self.settings_key(key.code);
        }
        if key.code == KeyCode::Char(',') {
            self.settings_menu = Some(0);
            return None;
        }
        if key.code == KeyCode::Char('?') {
            self.show_help = true;
            self.help_scroll = 0;
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

    /// The settings panel: j/k move, Enter edits the row, esc closes.
    fn settings_key(&mut self, key: KeyCode) -> Option<Action> {
        let selected = self.settings_menu?;
        let rows = SETTINGS.len();
        match key {
            KeyCode::Char('j') | KeyCode::Down => self.settings_menu = Some((selected + 1) % rows),
            KeyCode::Char('k') | KeyCode::Up => {
                self.settings_menu = Some((selected + rows - 1) % rows)
            }
            KeyCode::Enter => {
                self.settings_menu = None;
                return self.edit_setting(SETTINGS[selected]);
            }
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char(',') => self.settings_menu = None,
            _ => {}
        }
        None
    }

    /// Opens the theme menu, or a box filled with the current value; a yes or no flips.
    fn edit_setting(&mut self, setting: Setting) -> Option<Action> {
        match setting {
            Setting::Theme => {
                let names = crate::theme::THEME_NAMES;
                self.theme_menu = names
                    .iter()
                    .position(|name| *name == self.theme_name)
                    .or(Some(0));
            }
            Setting::Workspace => {
                let current = self.workspace_dirs.join(", ");
                self.ask(PromptKind::Workspace, &current);
            }
            Setting::UpdatedWithin => {
                let current = self.updated_within_days.map(|days| days.to_string());
                self.ask(PromptKind::UpdatedWithin, &current.unwrap_or_default());
            }
            Setting::Editor => self.editor_menu = Some(self.editor_list()),
        }
        None
    }

    /// The editor list on this machine, on the editor the config has now.
    fn editor_list(&self) -> EditorMenu {
        let shell =
            crate::editor_command::editor_name(None, &crate::editor_command::shell_editor());
        EditorMenu::new(&shell, self.editor_config.as_ref(), crate::repo::on_path)
    }

    /// The editor list: j/k move, Enter saves the pick or asks for a custom command.
    fn editor_menu_key(&mut self, key: KeyCode) -> Option<Action> {
        let menu = self.editor_menu.as_mut()?;
        let count = menu.choices.len();
        match key {
            KeyCode::Char('j') | KeyCode::Down => menu.selected = (menu.selected + 1) % count,
            KeyCode::Char('k') | KeyCode::Up => menu.selected = (menu.selected + count - 1) % count,
            KeyCode::Esc | KeyCode::Char('q') => self.editor_menu = None,
            KeyCode::Enter => {
                let choice = menu.choices[menu.selected];
                let open = menu.open.take();
                self.editor_menu = None;
                return match choice {
                    EditorChoice::Shell => Some(Action::SetEditor { editor: None, open }),
                    EditorChoice::Known { program, .. } => Some(Action::SetEditor {
                        editor: Some(crate::editor_command::config_for(program)),
                        open,
                    }),
                    EditorChoice::Custom => {
                        let current = self
                            .editor_config
                            .as_ref()
                            .filter(|editor| crate::editor_command::known_label(editor).is_none())
                            .map(|editor| editor.command.clone());
                        self.ask(PromptKind::Editor { open }, &current.unwrap_or_default());
                        None
                    }
                };
            }
            _ => {}
        }
        None
    }

    /// The settings panel's value for a row.
    pub fn setting_value(&self, setting: Setting) -> String {
        match setting {
            Setting::Theme => self.theme_name.to_string(),
            Setting::Workspace if self.workspace_dirs.is_empty() => "not set".to_string(),
            Setting::Workspace => self.workspace_dirs.join(", "),
            Setting::UpdatedWithin => match self.updated_within_days {
                Some(days) => format!("{days} days"),
                None => "no limit".to_string(),
            },
            Setting::Editor => match &self.editor_config {
                None => format!("{} (from your shell)", self.editor_name),
                Some(editor) => crate::editor_command::known_label(editor)
                    .map_or_else(|| editor.command.clone(), str::to_string),
            },
        }
    }

    /// An empty command goes back to the shell's editor. A new command waits unless revu
    /// knows it opens a window; `wait` in config.yaml changes that.
    fn submit_editor(&mut self, text: &str, open: Option<EditorTarget>) -> Option<Action> {
        let command = text.trim();
        if command.is_empty() {
            return Some(Action::SetEditor { editor: None, open });
        }
        if !command.contains("{file}") {
            let error = "the command needs {file}, such as zed {file}:{line}".to_string();
            self.set_prompt_error(PromptKind::Editor { open }, text, error);
            return None;
        }
        let editor = EditorConfig {
            command: command.to_string(),
            wait: !crate::editor_command::opens_a_window(command),
        };
        Some(Action::SetEditor {
            editor: Some(editor),
            open,
        })
    }

    fn submit_updated_within(&mut self, text: &str) -> Option<Action> {
        if text.trim().is_empty() {
            return Some(Action::SetUpdatedWithin(None));
        }
        match text.trim().parse::<u32>() {
            Ok(days) if days > 0 => Some(Action::SetUpdatedWithin(Some(days))),
            _ => {
                let error =
                    "type a number of days, such as 30, or nothing for no limit".to_string();
                self.set_prompt_error(PromptKind::UpdatedWithin, text, error);
                None
            }
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
            KeyCode::Esc | KeyCode::Char('q') => self.theme_menu = None,
            _ => {}
        }
        None
    }

    /// A paste goes into the open box, or opens the link box when none is open.
    pub fn handle_paste(&mut self, text: &str) {
        // The list holds the message built before it opened; a paste would not reach it.
        if self.agent_menu.is_some() {
            return;
        }
        if let Some(composer) = self.composer.as_mut() {
            composer.editor.insert(text);
            return;
        }
        if let Some(menu) = self.label_menu.as_mut() {
            menu.filter.push_str(text.trim());
            menu.selected = 0;
            return;
        }
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
            // On first run, no folder means no diff; from the settings it changes nothing.
            KeyCode::Esc
                if prompt.kind == PromptKind::Workspace && self.workspace_dirs.is_empty() =>
            {
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
            PromptKind::FindInDiff => self.in_view(|view| view.start_find(&prompt.text)),
            PromptKind::Definition { .. } => {
                self.in_view(|view| view.find_definition(&prompt.text))
            }
            PromptKind::UpdatedWithin => self.submit_updated_within(&prompt.text),
            PromptKind::Editor { open } => self.submit_editor(&prompt.text, open),
            PromptKind::ConfirmDiscard { pr, pending_review } => prompt
                .text
                .trim()
                .eq_ignore_ascii_case("y")
                .then_some(Action::DiscardReview { pr, pending_review }),
            PromptKind::ConfirmMerge {
                pr,
                pull_request_id,
                head_oid,
                request,
                ..
            } => prompt
                .text
                .trim()
                .eq_ignore_ascii_case("y")
                .then_some(Action::Merge {
                    pr,
                    pull_request_id,
                    head_oid,
                    request,
                }),
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

    /// The review and comment box: text keys go to the editor, Tab picks the verdict,
    /// ctrl+s asks to confirm, and y sends.
    fn composer_key(&mut self, key: KeyEvent) -> Option<Action> {
        let composer = self.composer.as_mut()?;
        if composer.sending {
            return None;
        }
        if composer.confirming {
            composer.confirming = false;
            if matches!(key.code, KeyCode::Char('y') | KeyCode::Enter) {
                composer.sending = true;
                return Some(composer.action());
            }
            return None;
        }
        if key.code == KeyCode::Tab {
            if !composer.accept_suggestion() {
                composer.next_verdict();
            }
            return None;
        }
        let outcome = composer.editor.key(key);
        let cancel_asked = std::mem::take(&mut composer.cancel_asked);
        composer.error = None;
        match outcome {
            Outcome::Handled => None,
            Outcome::Copy(text) => Some(Action::Copy(text)),
            Outcome::Submit => {
                match composer.check() {
                    Ok(()) if matches!(composer.kind, ComposeKind::Handoff(_)) => {
                        return Some(composer.action());
                    }
                    Ok(()) => composer.confirming = true,
                    Err(error) => composer.error = Some(error),
                }
                None
            }
            Outcome::Cancel if cancel_asked || composer.editor.text.trim().is_empty() => {
                self.composer = None;
                None
            }
            Outcome::Cancel => {
                composer.cancel_asked = true;
                None
            }
        }
    }

    /// Select lines: j/k move, V starts or ends a range, enter writes the comment, and
    /// esc or c goes back to the diff.
    fn line_mode_key(&mut self, key: KeyCode) -> Option<Action> {
        let Screen::Detail(view) = &mut self.screen else {
            return None;
        };
        let mode = view.line_mode.as_mut()?;
        match key {
            KeyCode::Char('j') | KeyCode::Down => mode.move_cursor(1),
            KeyCode::Char('k') | KeyCode::Up => mode.move_cursor(-1),
            KeyCode::Char('g') => mode.cursor = 0,
            KeyCode::Char('G') => mode.move_cursor(isize::MAX),
            KeyCode::Char('V') => {
                mode.anchor = match mode.anchor {
                    Some(_) => None,
                    None => Some(mode.cursor),
                }
            }
            KeyCode::Esc | KeyCode::Char('S') | KeyCode::Char('q') => view.line_mode = None,
            KeyCode::Char('w') => {
                view.diff_wrap = !view.diff_wrap;
                view.diff_shift = 0;
            }
            KeyCode::Char('y') => {
                let Some(Ok(rows)) = &mode.rows else {
                    return None;
                };
                let (first, last) = mode.marked();
                let picked = rows.get(first..=last)?;
                let lines = picked.iter().filter(|row| row.target().is_some()).count();
                let unit = if lines == 1 { "line" } else { "lines" };
                let place = crate::line_diff::place(&mode.path, picked);
                self.saved = Some(format!("copied {place} ({lines} {unit})"));
                return Some(Action::Copy(crate::line_diff::snippet(&mode.path, picked)));
            }
            KeyCode::Enter => {
                let result = self.open_line_comment();
                self.error = result.err();
            }
            KeyCode::Char('R') => {
                let result = self.open_review();
                self.error = result.err();
            }
            KeyCode::Char('a') => {
                let result = self.open_handoff();
                return self.report(result);
            }
            KeyCode::Char('D') => {
                let result = view.line_names_request().map(Some);
                return self.report(result);
            }
            KeyCode::Char('t') => {
                let result = view.toggle_threads();
                self.error = result.err();
            }
            KeyCode::Char('r') => {
                let result = self.open_reply();
                self.error = result.err();
            }
            KeyCode::Char('u') => {
                let result = view.resolve_thread();
                return self.report(result);
            }
            KeyCode::Char('J') => mode.scroll = mode.scroll.saturating_add(3),
            KeyCode::Char('K') => mode.scroll = mode.scroll.saturating_sub(3),
            KeyCode::Char(' ') | KeyCode::PageDown => mode.scroll = mode.scroll.saturating_add(20),
            KeyCode::Char('b') | KeyCode::PageUp => mode.scroll = mode.scroll.saturating_sub(20),
            KeyCode::Char(']') | KeyCode::Char('[') => {
                let result = view.jump_to_thread(key == KeyCode::Char(']'));
                self.error = result.err();
            }
            _ => {}
        }
        None
    }

    /// r in select lines: a box for a reply to the thread on the cursor's line.
    fn open_reply(&mut self) -> Result<(), String> {
        let Screen::Detail(view) = &self.screen else {
            return Ok(());
        };
        let thread = view.thread_here()?;
        let (Some(Ok(detail)), Some(Ok(activity))) = (&view.detail, &view.activity) else {
            return Err("wait for the PR to load before replying".to_string());
        };
        let author = thread
            .comments
            .first()
            .map_or_else(String::new, |comment| comment.author.login.clone());
        let kind = ComposeKind::Reply {
            thread: thread.id.clone(),
            author,
            place: format!("{} {}", thread.path, thread.lines),
        };
        self.composer = Some(Composer::new(view.pr.clone(), detail, activity, kind));
        Ok(())
    }

    /// a: a box for a request to a coding agent. In select lines it attaches the marked
    /// lines and the thread on the cursor's line; in Files changed, the selected file.
    fn open_handoff(&mut self) -> Result<Option<Action>, String> {
        let Screen::Detail(view) = &mut self.screen else {
            return Ok(None);
        };
        // The find prompt's words, loaded once per PR; Tab completes from them.
        let words_request = match view.find_words {
            Some(_) => None,
            None => view.words_request(),
        };
        let view = &*view;
        let detail = view
            .loaded()
            .ok_or("wait for the PR to load before handing it off")?;
        let mut spot = crate::handoff::Spot::default();
        match &view.line_mode {
            Some(mode) => {
                if let Some(Ok(rows)) = &mode.rows {
                    let (first, last) = mode.marked();
                    if let Some(picked) = rows.get(first..=last) {
                        spot.lines = Some((
                            crate::line_diff::place(&mode.path, picked),
                            crate::line_diff::snippet(&mode.path, picked),
                        ));
                    }
                }
                spot.thread = view.thread_here().ok();
            }
            None if view.tab == Tab::Files => spot.file = view.selected_row_path(),
            None => {}
        }
        let checkout = match &view.repo {
            RepoState::Ready(repo) => Some(repo),
            _ => None,
        };
        let elsewhere = checkout
            .and_then(|repo| crate::repo::checked_out_elsewhere(&repo.dir, &detail.head_oid));
        let checkout = checkout.map(|repo| crate::handoff::Checkout {
            dir: &repo.dir,
            elsewhere: elsewhere.as_deref(),
        });
        let context = crate::handoff::context(&view.pr, detail, checkout, spot);
        let mut composer = Composer::handoff(view.pr.clone(), detail, context);
        composer.words = view.find_words.clone().unwrap_or_default();
        self.composer = Some(composer);
        Ok(words_request)
    }

    /// The agent list: j/k move, Enter hands the message off, esc goes back to the box.
    fn agent_menu_key(&mut self, key: KeyCode) -> Option<Action> {
        let menu = self.agent_menu.as_mut()?;
        let count = menu.targets.len().max(1);
        match key {
            KeyCode::Char('j') | KeyCode::Down => menu.selected = (menu.selected + 1) % count,
            KeyCode::Char('k') | KeyCode::Up => menu.selected = (menu.selected + count - 1) % count,
            KeyCode::Esc | KeyCode::Char('q') => self.agent_menu = None,
            KeyCode::Enter => {
                let menu = self.agent_menu.take()?;
                let target = menu.targets.get(menu.selected)?.clone();
                return Some(Action::HandOff {
                    target,
                    message: menu.message,
                    dir: menu.dir,
                });
            }
            _ => {}
        }
        None
    }

    /// The handoff reached its agent, or failed and went to the clipboard instead.
    pub fn handed_off(&mut self, result: Result<String, String>) {
        self.composer = None;
        match result {
            Ok(note) => self.saved = Some(note),
            Err(error) => self.error = Some(error),
        }
    }

    /// Enter in select lines: a box for the line, or the range V marked.
    fn open_line_comment(&mut self) -> Result<(), String> {
        let Screen::Detail(view) = &self.screen else {
            return Ok(());
        };
        let mode = view.line_mode.as_ref().ok_or("not selecting lines")?;
        let target = mode.target()?;
        let (Some(Ok(detail)), Some(Ok(activity))) = (&view.detail, &view.activity) else {
            return Err("wait for the PR to load before commenting".to_string());
        };
        let kind = ComposeKind::LineComment(target);
        self.composer = Some(Composer::new(view.pr.clone(), detail, activity, kind));
        Ok(())
    }

    pub fn set_outline(
        &mut self,
        pr: &PrRef,
        commit: String,
        path: String,
        outline: Option<crate::syntax::Outline>,
    ) {
        if let Some(view) = self.view_for(pr) {
            view.outlines.insert((commit, path), outline);
        }
    }

    /// The names on the line D asked about: one is looked up at once, several are
    /// offered in a prompt, the calls first.
    pub fn set_line_names(&mut self, pr: &PrRef, names: Vec<String>) {
        let Some(view) = self.view_for(pr) else {
            return;
        };
        match names.as_slice() {
            [] => self.error = Some("no names on this line to look up".to_string()),
            [name] => {
                let result = view.find_definition(name);
                self.queued = self.report(result);
            }
            [first, ..] => {
                self.prompt = Some(Prompt {
                    kind: PromptKind::Definition {
                        names: names.clone(),
                    },
                    text: first.clone(),
                    error: None,
                    words: names,
                });
            }
        }
    }

    /// D's answer: the first definition opens, and n and N walk the others.
    pub fn set_definitions(
        &mut self,
        pr: &PrRef,
        name: String,
        result: Result<(Vec<GrepHit>, bool), String>,
    ) {
        let Some(view) = self.view_for(pr) else {
            return;
        };
        let (hits, capped) = match result {
            Ok(found) => found,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        if hits.is_empty() {
            let searched = match capped {
                true => "in the files revu read",
                false => "at the PR head",
            };
            self.error = Some(format!(
                "no definition of {name} {searched}; it may come from a library"
            ));
            return;
        }
        view.def_walk = Some(DefWalk {
            name,
            hits,
            at: 0,
            capped,
        });
        let result = view.show_definition(0).map(Some);
        self.queued = self.report(result);
    }

    /// Whether this diff job's file still needs its structure read; asking marks it.
    /// Commits and whole-commit diffs have none.
    /// Whether a file at a commit is parsed already, so a worker need not parse it again.
    pub fn knows_outline(&self, pr: &PrRef, commit: &str, path: &str) -> bool {
        let Screen::Detail(view) = &self.screen else {
            return false;
        };
        view.pr == *pr
            && view
                .outlines
                .contains_key(&(commit.to_string(), path.to_string()))
    }

    pub fn wants_structure(&mut self, job: &DiffJob) -> bool {
        let Some(view) = self.view_for(&job.pr) else {
            return false;
        };
        let DiffTarget::File(path) = &job.key.target else {
            return false;
        };
        view.tab == Tab::Files && view.structure_asked.insert(path.clone())
    }

    pub fn set_hunk_firsts(
        &mut self,
        pr: &PrRef,
        path: String,
        firsts: Vec<crate::line_diff::LineAt>,
    ) {
        if let Some(view) = self.view_for(pr) {
            view.hunk_firsts.insert(path, firsts);
        }
    }

    pub fn set_line_rows(&mut self, pr: &PrRef, path: &str, result: Result<String, String>) {
        let Some(view) = self.view_for(pr) else {
            return;
        };
        let Some(mode) = view.line_mode.as_mut().filter(|mode| mode.path == path) else {
            return;
        };
        let rows = result.map(|diff| crate::line_diff::parse(&diff));
        // Start on the first changed line, where a comment usually goes.
        let changed = |row: &DiffRow| matches!(row.kind, RowKind::Added | RowKind::Removed);
        mode.cursor = rows
            .as_ref()
            .ok()
            .and_then(|rows| rows.iter().position(changed))
            .unwrap_or(0);
        mode.rows = Some(rows);
    }

    /// e: the label checklist, ticked with the PR's labels; the repo's list loads next.
    fn open_label_menu(&mut self) -> Option<Action> {
        let Screen::Detail(view) = &self.screen else {
            return None;
        };
        let Some(Ok(detail)) = &view.detail else {
            self.error = Some("wait for the PR to load before labeling it".to_string());
            return None;
        };
        let current: HashSet<String> = detail
            .labels
            .iter()
            .map(|label| label.name.clone())
            .collect();
        let cached = view.repo_labels.clone();
        let fetch = cached.is_none().then(|| Action::FetchLabels {
            pr: view.pr.clone(),
        });
        self.label_menu = Some(LabelMenu {
            pr: view.pr.clone(),
            pull_request_id: detail.id.clone(),
            labels: cached.map(Ok),
            chosen: current.clone(),
            current,
            filter: String::new(),
            selected: 0,
        });
        fetch
    }

    pub fn set_repo_labels(&mut self, pr: &PrRef, result: Result<Vec<RepoLabel>, String>) {
        if let (Ok(labels), Some(view)) = (&result, self.view_for(pr)) {
            view.repo_labels = Some(labels.clone());
        }
        if let Some(menu) = self.label_menu.as_mut().filter(|menu| &menu.pr == pr) {
            menu.labels = Some(result);
        }
    }

    /// Typing filters, up and down move, space ticks, enter applies, esc closes.
    fn label_menu_key(&mut self, key: KeyCode) -> Option<Action> {
        let menu = self.label_menu.as_mut()?;
        let shown = menu.shown().len();
        match key {
            KeyCode::Esc => self.label_menu = None,
            KeyCode::Down => menu.selected = (menu.selected + 1).min(shown.saturating_sub(1)),
            KeyCode::Up => menu.selected = menu.selected.saturating_sub(1),
            KeyCode::Char(' ') => menu.toggle(),
            KeyCode::Backspace => {
                menu.filter.pop();
                menu.selected = 0;
            }
            KeyCode::Char(character) => {
                menu.filter.push(character);
                menu.selected = 0;
            }
            KeyCode::Enter => {
                let menu = self.label_menu.take()?;
                return menu.change();
            }
            _ => {}
        }
        None
    }

    /// R: a review of the open PR, carrying the pending line comments.
    fn open_review(&mut self) -> Result<(), String> {
        let Screen::Detail(view) = &self.screen else {
            return Ok(());
        };
        let (Some(Ok(detail)), Some(Ok(activity))) = (&view.detail, &view.activity) else {
            return Err("wait for the PR to load before reviewing it".to_string());
        };
        let kind = ComposeKind::Review {
            verdict: Verdict::Comment,
            comment_only: comment_only_reason(detail),
            drafts: activity.drafts.len(),
        };
        self.composer = Some(Composer::new(view.pr.clone(), detail, activity, kind));
        Ok(())
    }

    /// M: opens the merge menu, or says why the PR cannot merge from here.
    fn open_merge_menu(&mut self) -> Result<(), String> {
        let Screen::Detail(view) = &self.screen else {
            return Ok(());
        };
        let Some(Ok(detail)) = &view.detail else {
            return Err("the pull request is still loading".to_string());
        };
        self.merge_menu = Some(MergeMenu::new(&view.pr, detail)?);
        Ok(())
    }

    fn merge_menu_key(&mut self, key: KeyCode) {
        let Some(menu) = self.merge_menu.as_mut() else {
            return;
        };
        let count = menu.items.len();
        match key {
            KeyCode::Char('j') | KeyCode::Down => menu.selected = (menu.selected + 1) % count,
            KeyCode::Char('k') | KeyCode::Up => menu.selected = (menu.selected + count - 1) % count,
            KeyCode::Esc | KeyCode::Char('q') => self.merge_menu = None,
            KeyCode::Enter | KeyCode::Char(' ')
                if menu.items[menu.selected] == MergeItem::Bypass =>
            {
                menu.bypass = !menu.bypass;
                menu.note = None;
            }
            KeyCode::Enter => match menu.request() {
                Ok((request, bypass)) => {
                    let kind = PromptKind::ConfirmMerge {
                        pr: menu.pr.clone(),
                        pull_request_id: menu.pull_request_id.clone(),
                        head_oid: menu.head_oid.clone(),
                        request,
                        base: menu.base.clone(),
                        bypass,
                    };
                    self.merge_menu = None;
                    self.ask(kind, "");
                }
                Err(note) => menu.note = Some(note),
            },
            _ => {}
        }
    }

    fn open_menu_key(&mut self, key: KeyCode) -> Option<Action> {
        let menu = self.open_menu.as_mut()?;
        match key {
            KeyCode::Char('j' | 'k') | KeyCode::Down | KeyCode::Up | KeyCode::Tab => {
                menu.selected = 1 - menu.selected;
            }
            KeyCode::Esc | KeyCode::Char('q') => self.open_menu = None,
            // The menu shows why the editor cannot open the file; it stays open.
            KeyCode::Enter if menu.selected == 0 && menu.editor.is_err() => {}
            KeyCode::Enter => {
                let menu = self.open_menu.take()?;
                return match (menu.selected, menu.editor) {
                    (0, Ok(target))
                        if self.editor_config.is_none() && !self.prefs.editor_picked =>
                    {
                        self.editor_menu = Some(EditorMenu {
                            open: Some(target),
                            ..self.editor_list()
                        });
                        None
                    }
                    (0, Ok(target)) => Some(Action::OpenEditor(target)),
                    _ => Some(Action::OpenBrowser(menu.github)),
                };
            }
            _ => {}
        }
        None
    }

    /// X: asks before deleting the pending review.
    fn ask_discard(&mut self) -> Result<(), String> {
        let Screen::Detail(view) = &self.screen else {
            return Ok(());
        };
        let pending = match &view.activity {
            Some(Ok(activity)) => activity.pending_review.clone(),
            _ => None,
        };
        let Some(pending_review) = pending else {
            return Err("you have no pending review on this PR".to_string());
        };
        let kind = PromptKind::ConfirmDiscard {
            pr: view.pr.clone(),
            pending_review,
        };
        self.ask(kind, "");
        Ok(())
    }

    /// A write finished: a sent box closes, a failed one shows why and keeps the text.
    pub fn set_written(&mut self, pr: &PrRef, result: Result<(), String>) {
        let note = self.saving.take();
        let reload_on_error = note.as_ref().is_some_and(|note| note.reload_on_error);
        if result.is_ok() || reload_on_error {
            self.reload_after_write = Some(pr.clone());
        }
        if result.is_ok() {
            self.saved = note.map(|note| note.done.to_string());
        }
        let Some(composer) = self.composer.as_mut().filter(|composer| &composer.pr == pr) else {
            self.error = result.err();
            return;
        };
        composer.sending = false;
        match result {
            Ok(()) => self.composer = None,
            Err(error) => composer.error = Some(error),
        }
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
        Some(self.open_pr(pr, None))
    }

    fn open_pr(&mut self, pr: PrRef, summary: Option<PrSummary>) -> Action {
        self.error = None;
        let mut view = DetailView::new(
            pr.clone(),
            &self.prefs,
            &self.renderer_names,
            &self.missing_renderers,
        );
        if let Some(summary) = &summary {
            self.seen.record(summary);
        }
        view.summary = summary;
        self.screen = Screen::Detail(Box::new(view));
        Action::FetchDetail(pr)
    }

    /// Leaving a PR marks it seen as it is now, your own comments included, and puts
    /// its fresh row in the inbox so the stale one does not read as new.
    fn back_to_inbox(&mut self) {
        let fresh = match &self.screen {
            Screen::Detail(view) => view.loaded().map(|detail| detail.summary.clone()),
            Screen::Inbox => None,
        };
        if let Some(fresh) = fresh {
            self.seen.record(&fresh);
            let rows = self
                .groups
                .iter_mut()
                .flat_map(|group| group.prs.iter_mut());
            for row in rows.filter(|row| row.reference() == fresh.reference()) {
                *row = fresh.clone();
            }
        }
        self.screen = Screen::Inbox;
        self.error = None;
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
        if key == KeyCode::Char('o') {
            return match view.open_menu() {
                Some(menu) => {
                    let editor_name = self.editor_name.clone();
                    let selected = if menu.editor.is_ok() { 0 } else { 1 };
                    self.open_menu = Some(OpenMenu {
                        editor_name,
                        selected,
                        ..menu
                    });
                    None
                }
                None => Some(Action::OpenBrowser(view.link())),
            };
        }
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
            return self.report(result);
        }
        if view.line_mode.is_some() {
            return self.line_mode_key(key);
        }
        let on_image = view
            .overview
            .as_ref()
            .and_then(|layout| layout.rows.get(view.cursor))
            .is_some_and(|row| row.image.is_some());
        let gallery_key = match key {
            KeyCode::Char('i') => true,
            KeyCode::Enter => on_image,
            _ => false,
        };
        if view.tab == Tab::Overview && gallery_key {
            return self.open_gallery();
        }
        match key {
            KeyCode::Char('S') if view.tab == Tab::Files => {
                let result = view.start_line_mode();
                return self.report(result.map(Some));
            }
            KeyCode::Char('R') => {
                let result = self.open_review();
                self.error = result.err();
            }
            KeyCode::Char('a') => {
                let result = self.open_handoff();
                return self.report(result);
            }
            KeyCode::Char('X') => {
                let result = self.ask_discard();
                self.error = result.err();
            }
            KeyCode::Char('M') => {
                let result = self.open_merge_menu();
                self.error = result.err();
            }
            KeyCode::Char('e') if view.tab == Tab::Overview => return self.open_label_menu(),
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
            KeyCode::Esc => view.clear_find(),
            KeyCode::Char('q') | KeyCode::Char('h') | KeyCode::Left => self.back_to_inbox(),
            KeyCode::Tab => view.show_tab(view.tab.shifted(1)),
            KeyCode::BackTab => view.show_tab(view.tab.shifted(-1)),
            KeyCode::Char(digit @ '1'..='9') => {
                let index = digit as usize - '1' as usize;
                view.show_tab(Tab::ALL.get(index).copied().unwrap_or(view.tab));
            }
            KeyCode::Char('m') if diff_pane => {
                let next = next_renderer(view.renderer, renderer_count, &self.missing_renderers);
                if next == view.renderer {
                    self.notice = Some("no other renderer is installed".to_string());
                    self.notice_seen = false;
                    return None;
                }
                view.renderer = next;
                view.diff_scroll = 0;
            }
            KeyCode::Char('J') if diff_pane => view.scroll_diff(3),
            KeyCode::Char('K') if diff_pane => view.scroll_diff(-3),
            KeyCode::Char(' ') | KeyCode::PageDown if diff_pane => view.scroll_diff(20),
            KeyCode::Char('b') | KeyCode::PageUp if diff_pane => view.scroll_diff(-20),
            KeyCode::Char('v') if view.tab == Tab::Files => return view.toggle_viewed(),
            KeyCode::Char('y') if view.tab == Tab::Files => {
                let path = view.selected_row_path()?;
                self.saved = Some(format!("copied {path}"));
                return Some(Action::Copy(path));
            }
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

    /// Opens the gallery at the image on the cursor row, else at the first image on
    /// screen, else at the PR's first image.
    fn open_gallery(&mut self) -> Option<Action> {
        let Screen::Detail(view) = &self.screen else {
            return None;
        };
        let layout = view.overview.as_ref()?;
        let rows = &layout.rows;
        let start = rows
            .get(view.cursor)
            .and_then(|row| row.image.as_ref())
            .or_else(|| {
                rows.iter()
                    .skip(view.scroll)
                    .take(view.page)
                    .find_map(|row| row.image.as_ref())
            });
        let gallery = crate::gallery::Gallery::open(layout.images.clone(), start);
        if gallery.is_none() {
            self.error = Some("this pull request shows no image or diagram".to_string());
        }
        self.gallery = gallery;
        self.gallery_load()
    }

    fn gallery_load(&mut self) -> Option<Action> {
        self.gallery.as_mut()?.start_load().map(Action::LoadImage)
    }

    fn gallery_key(&mut self, key: KeyCode) -> Option<Action> {
        let gallery = self.gallery.as_mut()?;
        match key {
            KeyCode::Esc | KeyCode::Char('q') => self.gallery = None,
            KeyCode::Char('h') | KeyCode::Left => {
                gallery.step(false);
                return self.gallery_load();
            }
            KeyCode::Char('l') | KeyCode::Right => {
                gallery.step(true);
                return self.gallery_load();
            }
            KeyCode::Char('o') => {
                if let crate::markdown::Visual::Image(url) = gallery.current() {
                    return Some(Action::OpenBrowser(url.clone()));
                }
            }
            _ => {}
        }
        None
    }

    /// A loaded image or diagram arrived. It is dropped when its gallery has closed.
    pub fn set_image(
        &mut self,
        item: crate::markdown::Visual,
        result: Result<crate::gallery::Shown, String>,
    ) {
        let Some(gallery) = self.gallery.as_mut() else {
            return;
        };
        if !gallery.loaded.contains_key(&item) {
            return;
        }
        let loaded = match result {
            Ok(shown) => crate::gallery::Loaded::Ready(shown),
            Err(error) => crate::gallery::Loaded::Failed(error),
        };
        gallery.loaded.insert(item, loaded);
    }

    /// Starts the PR over on the same tab. The file list and the clone are looked up
    /// again when a tab needs them.
    fn reload(view: &mut DetailView, prefs: &ViewPrefs, renderer_names: &[String]) -> Action {
        let tab = view.tab;
        let renderer = view.renderer;
        let summary = view.summary.take();
        *view = DetailView::new(view.pr.clone(), prefs, renderer_names, &HashSet::new());
        view.tab = tab;
        view.renderer = renderer;
        view.summary = summary;
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
        let summary = self.selected_pr()?.clone();
        Some(self.open_pr(summary.reference(), Some(summary)))
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

/// Whether a review thread on `at` belongs under this row. An unchanged line holds a
/// thread on either side.
pub fn row_holds(row: &DiffRow, at: (crate::model::Side, u64)) -> bool {
    let left_of_context = row.kind == RowKind::Context
        && row
            .old
            .is_some_and(|old| at == (crate::model::Side::Left, old));
    row.target() == Some(at) || left_of_context
}

/// The file view's top line, so a few wrapped rows of context show above the line it
/// opened at. A long line above takes several rows, so it leaves room for fewer.
fn context_scroll(text: &Text, line: usize, width: usize) -> u16 {
    const CONTEXT_ROWS: usize = 5;
    // No width yet means the pane was never drawn; count each line as one row.
    let rows = |index: usize| {
        let length: usize = text.lines.get(index).map_or(1, ratatui::text::Line::width);
        match width {
            0 => 1,
            width => length.div_ceil(width).max(1),
        }
    };
    let mut top = line.min(text.lines.len().saturating_sub(1));
    let mut above = 0;
    while top > 0 && above + rows(top - 1) <= CONTEXT_ROWS {
        top -= 1;
        above += rows(top);
    }
    u16::try_from(top).unwrap_or(u16::MAX)
}

/// git's "@@ -1,3 +1,4 @@", delta's box top "─────┐", or difftastic's "a.md --- 2/5 --- Text".
fn is_hunk_start(line: &str) -> bool {
    let line = line.trim_end();
    let delta_box =
        line.ends_with('┐') && line.trim_end_matches('┐').chars().all(|char| char == '─');
    line.starts_with("@@") || delta_box || is_difftastic_hunk(line)
}

/// difftastic's "a.md --- 2/5 --- Text". It groups changes its own way, so its hunks
/// need not be git's even when there are as many.
fn is_difftastic_hunk(line: &str) -> bool {
    line.split(" --- ").nth(1).is_some_and(|part| {
        part.split_once('/')
            .is_some_and(|(at, of)| at.parse::<u32>().is_ok() && of.parse::<u32>().is_ok())
    })
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
            additions: 0,
            deletions: 0,
            changed_files: 0,
            head_oid: String::new(),
            turn: None,
            conflicts: false,
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
    fn a_pr_opened_from_the_inbox_keeps_its_row_for_the_header() {
        let mut app = app_with(vec![group("A", &[7], false)]);
        app.list_state.select(Some(1));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(
            view(&app).summary.as_ref().map(|summary| summary.number),
            Some(7)
        );
        app.handle_key(key(KeyCode::Char('r')));
        assert!(view(&app).summary.is_some(), "a reload keeps it");
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
        app.handle_key(key(KeyCode::Char('h')));
        assert!(matches!(app.screen, Screen::Inbox));
    }

    #[test]
    fn leaving_a_pr_marks_it_seen_with_its_fresh_row() {
        let mut detail = crate::model::tests::sample_detail();
        let mut app = app_with(vec![group("A", &[detail.summary.number], false)]);
        let reference = detail.summary.reference();
        app.groups[0].prs[0] = PrSummary {
            comments: 1,
            head_oid: "old".to_string(),
            ..detail.summary.clone()
        };
        app.list_state.select(Some(1));
        app.handle_key(key(KeyCode::Enter));
        // You comment and the PR loads again.
        detail.summary.comments = 2;
        detail.summary.head_oid = "old".to_string();
        app.set_detail(&reference, Ok(detail));
        app.handle_key(key(KeyCode::Char('h')));

        let row = &app.groups[0].prs[0];
        assert_eq!(row.comments, 2, "the inbox shows the fresh row");
        assert!(!app.seen.is_new(row), "your own comment is not news");
        let pushed = PrSummary {
            head_oid: "new".to_string(),
            ..row.clone()
        };
        assert!(app.seen.is_new(&pushed));
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
        view.overview = Some(OverviewLayout {
            width: 60,
            rows,
            images: Vec::new(),
        });
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
            25,
            "the card top is off screen, so going back reads by thirds too"
        );
    }

    fn overview_with_anchors(app: &mut App, total: usize, anchors: &[usize]) {
        use crate::markdown::TextRow;
        let Screen::Detail(view) = &mut app.screen else {
            unreachable!()
        };
        let mut rows: Vec<TextRow> = (0..total)
            .map(|_| TextRow::plain(ratatui::text::Line::from("text")))
            .collect();
        for anchor in anchors {
            rows[*anchor] = TextRow::anchor(ratatui::text::Line::from("card"));
        }
        view.tab = Tab::Overview;
        view.overview = Some(OverviewLayout {
            width: 60,
            rows,
            images: Vec::new(),
        });
        view.page = 30;
    }

    #[test]
    fn j_never_skips_the_rest_of_a_comment_with_the_cursor_low_on_screen() {
        let (mut app, _) = files_tab(Vec::new());
        overview_with_anchors(&mut app, 200, &[0, 50, 150]);
        let Screen::Detail(detail) = &mut app.screen else {
            unreachable!()
        };
        // As after a click near the bottom: the next card is 25 rows down, off screen.
        (detail.cursor, detail.scroll) = (25, 0);
        app.handle_key(key(KeyCode::Char('j')));
        assert!(
            view(&app).scroll <= 29,
            "rows 30 to 49 must show before the next card; the page starts at {}",
            view(&app).scroll
        );
    }

    #[test]
    fn j_through_the_overview_shows_every_row_and_keeps_some_in_common() {
        let (mut app, _) = files_tab(Vec::new());
        overview_with_anchors(&mut app, 300, &[0, 4, 20, 45, 52, 90, 125, 128, 200, 260]);
        let mut seen = vec![false; 300];
        let mut scroll = 0;
        for _ in 0..80 {
            app.handle_key(key(KeyCode::Char('j')));
            let next = view(&app).scroll;
            assert!(next >= scroll, "j never moves the page back");
            assert!(
                next < scroll + 30,
                "screens at {scroll} and {next} share no row"
            );
            scroll = next;
            seen[scroll..scroll + 30]
                .iter_mut()
                .for_each(|row| *row = true);
        }
        assert!(
            seen.iter().all(|row| *row),
            "every row showed at some point"
        );
    }

    #[test]
    fn hunks_fill_in_the_open_pr_and_redraw_its_overview() {
        let (mut app, pr) = files_tab(Vec::new());
        app.set_activity(
            &pr,
            Ok(crate::model::parse_activity(&crate::model::tests::activity_fixture()).unwrap()),
        );
        let code = |app: &App| match &view(app).activity {
            Some(Ok(activity)) => activity
                .timeline
                .iter()
                .flat_map(|comment| &comment.threads)
                .map(|thread| thread.code.len())
                .sum::<usize>(),
            _ => 0,
        };
        assert_eq!(code(&app), 0);
        let mut hunks = std::collections::HashMap::new();
        let line = crate::model::HunkLine {
            number: 12,
            mark: '+',
            text: "x".to_string(),
        };
        hunks.insert("T2".to_string(), vec![line]);
        app.set_hunks(&pr, hunks);
        assert_eq!(code(&app), 1);
        assert!(
            view(&app).overview.is_none(),
            "the overview is laid out again"
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
            images: Vec::new(),
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
            images: Vec::new(),
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
    fn settings_edit_the_workspace_and_check_the_days() {
        let mut app = App::new();
        app.workspace_dirs = vec!["~/Work".to_string(), "~/Side".to_string()];
        app.handle_key(key(KeyCode::Char(',')));
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.setting_value(Setting::Workspace), "~/Work, ~/Side");
        app.handle_key(key(KeyCode::Enter));
        assert!(app.settings_menu.is_none());
        let prompt = app.prompt.as_ref().unwrap();
        assert_eq!(
            (&prompt.kind, prompt.text.as_str()),
            (&PromptKind::Workspace, "~/Work, ~/Side")
        );
        app.handle_key(key(KeyCode::Esc));
        assert!(app.prompt.is_none(), "esc keeps the folder");
        app.handle_key(key(KeyCode::Char(',')));
        app.handle_key(key(KeyCode::Char('j')));
        app.handle_key(key(KeyCode::Char('j')));
        app.handle_key(key(KeyCode::Enter));
        app.prompt.as_mut().unwrap().text = "soon".to_string();
        assert!(app.handle_key(key(KeyCode::Enter)).is_none());
        assert!(app.prompt.as_ref().unwrap().error.is_some());
        app.prompt.as_mut().unwrap().text = "14".to_string();
        let action = app.handle_key(key(KeyCode::Enter));
        assert!(matches!(action, Some(Action::SetUpdatedWithin(Some(14)))));
        app.handle_key(key(KeyCode::Char(',')));
        app.handle_key(key(KeyCode::Char('j')));
        app.handle_key(key(KeyCode::Char('j')));
        app.handle_key(key(KeyCode::Enter));
        app.prompt.as_mut().unwrap().text = " ".to_string();
        let action = app.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(action, Some(Action::SetUpdatedWithin(None))),
            "empty is no limit"
        );
    }

    /// , then Enter on a settings row.
    fn edit_setting_row(app: &mut App, setting: Setting) -> Option<Action> {
        app.handle_key(key(KeyCode::Char(',')));
        let row = SETTINGS.iter().position(|row| *row == setting).unwrap();
        for _ in 0..row {
            app.handle_key(key(KeyCode::Char('j')));
        }
        app.handle_key(key(KeyCode::Enter))
    }

    #[test]
    fn the_editor_list_offers_the_shell_editor_the_installed_ones_and_a_custom_command() {
        let installed = |program: &str| ["zed", "hx", "nvim"].contains(&program);
        let menu = EditorMenu::new("hx", None, installed);
        let labels: Vec<String> = menu
            .choices
            .iter()
            .map(|choice| match choice {
                EditorChoice::Shell => "shell".to_string(),
                EditorChoice::Known { label, .. } => label.to_string(),
                EditorChoice::Custom => "custom".to_string(),
            })
            .collect();
        assert_eq!(
            labels,
            ["shell", "Zed", "Neovim", "custom"],
            "hx is the shell's"
        );
        assert_eq!(menu.current, 0);
        let zed = crate::editor_command::config_for("zed");
        let menu = EditorMenu::new("hx", Some(&zed), installed);
        assert_eq!((menu.current, menu.shell.as_str()), (1, "hx"));
        let other = EditorConfig {
            command: "zed -n {file}".to_string(),
            wait: false,
        };
        assert_eq!(EditorMenu::new("hx", Some(&other), installed).current, 3);
    }

    #[test]
    fn settings_save_the_picked_editor_or_ask_for_a_command() {
        let mut app = App::new();
        app.editor_name = "hx".to_string();
        assert_eq!(app.setting_value(Setting::Editor), "hx (from your shell)");
        edit_setting_row(&mut app, Setting::Editor);
        let menu = app.editor_menu.as_mut().expect("the editor list");
        menu.choices = vec![
            EditorChoice::Shell,
            EditorChoice::Known {
                label: "Zed",
                program: "zed",
            },
            EditorChoice::Custom,
        ];
        menu.selected = 0;
        app.handle_key(key(KeyCode::Char('j')));
        let Some(Action::SetEditor {
            editor: Some(editor),
            ..
        }) = app.handle_key(key(KeyCode::Enter))
        else {
            panic!("expected Zed saved");
        };
        assert_eq!(editor, crate::editor_command::config_for("zed"));
        app.editor_config = Some(editor);
        assert_eq!(app.setting_value(Setting::Editor), "Zed");

        edit_setting_row(&mut app, Setting::Editor);
        app.editor_menu.as_mut().unwrap().selected = 0;
        let action = app.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(action, Some(Action::SetEditor { editor: None, .. })),
            "{action:?}"
        );

        edit_setting_row(&mut app, Setting::Editor);
        let menu = app.editor_menu.as_mut().unwrap();
        menu.selected = menu.choices.len() - 1;
        assert!(app.handle_key(key(KeyCode::Enter)).is_none());
        let prompt = app.prompt.as_mut().expect("the custom command box");
        assert_eq!(prompt.text, "", "a known editor is not a custom command");
        prompt.text = "zed".to_string();
        assert!(app.handle_key(key(KeyCode::Enter)).is_none());
        assert!(app.prompt.as_ref().unwrap().error.is_some(), "no {{file}}");
        app.prompt.as_mut().unwrap().text = "myedit {file}".to_string();
        let Some(Action::SetEditor {
            editor: Some(custom),
            ..
        }) = app.handle_key(key(KeyCode::Enter))
        else {
            panic!("expected the custom command saved");
        };
        assert!(custom.wait, "an unknown editor runs in the terminal");
    }

    #[test]
    fn the_theme_menu_opens_from_settings_on_the_current_theme_and_applies_the_pick() {
        let mut app = app_with(vec![group("A", &[7], false)]);
        app.theme_name = "dracula";
        app.handle_key(key(KeyCode::Char(',')));
        app.handle_key(key(KeyCode::Enter));
        let dracula = crate::theme::THEME_NAMES
            .iter()
            .position(|name| *name == "dracula");
        assert_eq!(app.theme_menu, dracula);
        app.handle_key(key(KeyCode::Char('j')));
        let action = app.handle_key(key(KeyCode::Enter));
        assert!(matches!(action, Some(Action::SetTheme("gruvbox_dark"))));
        assert!(app.theme_menu.is_none());
        app.handle_key(key(KeyCode::Char(',')));
        app.handle_key(key(KeyCode::Enter));
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

    fn ctrl(character: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL)
    }

    #[test]
    fn a_handoff_asks_for_text_then_lists_agents_then_sends_the_pick() {
        use crate::handoff::{Context, Target};
        let mut app = App::new();
        let detail = crate::model::tests::sample_detail();
        let context = Context {
            summary: vec!["acme/app#1 Title".to_string()],
            text: "I'm reviewing a pull request in revu.\n".to_string(),
            dir: Some(PathBuf::from("/src/app")),
        };
        let pr = PrRef {
            repo: "acme/app".to_string(),
            number: 1,
        };
        app.composer = Some(Composer::handoff(pr, &detail, context));

        assert!(app.handle_key(ctrl('s')).is_none());
        let error = app
            .composer
            .as_ref()
            .and_then(|composer| composer.error.clone());
        assert_eq!(
            error.as_deref(),
            Some("write what you want the agent to do")
        );

        app.handle_key(key(KeyCode::Char('h')));
        app.handle_key(key(KeyCode::Char('i')));
        let Some(Action::PickAgent { message, dir }) = app.handle_key(ctrl('s')) else {
            panic!("ctrl+s should ask for the agent list");
        };
        assert!(message.ends_with("My request:\nhi\n"));
        assert_eq!(dir, Some(PathBuf::from("/src/app")));

        app.agent_menu = Some(AgentMenu {
            targets: vec![Target::NewClaude, Target::Clipboard],
            selected: 0,
            message: message.clone(),
            dir,
        });
        app.handle_key(key(KeyCode::Esc));
        assert!(app.agent_menu.is_none(), "esc closes the list");
        assert!(app.composer.is_some(), "and keeps the text");

        app.agent_menu = Some(AgentMenu {
            targets: vec![Target::NewClaude, Target::Clipboard],
            selected: 0,
            message,
            dir: None,
        });
        app.handle_key(key(KeyCode::Char('j')));
        let Some(Action::HandOff { target, .. }) = app.handle_key(key(KeyCode::Enter)) else {
            panic!("Enter should hand off");
        };
        assert_eq!(target, Target::Clipboard);
        app.handed_off(Ok("copied".to_string()));
        assert!(app.composer.is_none());
        assert_eq!(app.saved.as_deref(), Some("copied"));
    }

    #[test]
    fn tab_in_a_handoff_completes_the_word_before_the_cursor() {
        let detail = crate::model::tests::sample_detail();
        let context = crate::handoff::Context {
            summary: Vec::new(),
            text: String::new(),
            dir: None,
        };
        let pr = PrRef {
            repo: "acme/app".to_string(),
            number: 1,
        };
        let mut app = App::new();
        let mut composer = Composer::handoff(pr, &detail, context);
        composer.words = ["render_composer", "Renderer", "src/ui.rs"]
            .map(String::from)
            .to_vec();
        app.composer = Some(composer);
        let suggestion = |app: &App| app.composer.as_ref().and_then(Composer::suggestion);

        app.handle_paste("why does rend");
        assert_eq!(suggestion(&app).as_deref(), Some("er_composer"));
        app.handle_key(key(KeyCode::Tab));
        let text = |app: &App| {
            app.composer
                .as_ref()
                .map(|composer| composer.editor.text.clone())
        };
        assert_eq!(text(&app).as_deref(), Some("why does render_composer"));

        app.handle_paste(" ");
        assert_eq!(suggestion(&app), None, "nothing after a space");
        app.handle_paste("SRC/u");
        assert_eq!(suggestion(&app).as_deref(), Some("i.rs"), "case is ignored");
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(
            text(&app).as_deref(),
            Some("why does render_composer src/ui.rs"),
            "Tab spells the word as the diff does"
        );
        app.handle_paste(" r");

        app.handle_key(key(KeyCode::Left));
        assert_eq!(suggestion(&app), None, "nothing inside a word");

        let words = vec!["İstanbul".to_string()];
        assert_eq!(complete(&words, "İ").as_deref(), Some("stanbul"));
        assert_eq!(complete(&words, "i̇s").as_deref(), Some("tanbul"));
    }

    #[test]
    fn a_saved_renderer_that_is_not_installed_gives_way_to_one_that_is() {
        let names: Vec<String> = ["word", "delta", "difftastic"].map(String::from).to_vec();
        let prefs = ViewPrefs {
            renderer: Some("difftastic".to_string()),
            ..ViewPrefs::default()
        };
        let pr = PrRef {
            repo: "acme/app".to_string(),
            number: 1,
        };
        let view = DetailView::new(pr.clone(), &prefs, &names, &HashSet::from([2]));
        assert_eq!(view.renderer, 0);
        let view = DetailView::new(pr, &prefs, &names, &HashSet::new());
        assert_eq!(
            view.renderer, 2,
            "the saved choice returns once it is installed"
        );
    }

    #[test]
    fn a_stand_in_renderer_is_not_saved_over_the_missing_choice() {
        let mut app = app_with(vec![group("A", &[7], false)]);
        app.renderer_names = ["word", "difftastic"].map(String::from).to_vec();
        app.missing_renderers = HashSet::from([1]);
        app.prefs.renderer = Some("difftastic".to_string());
        assert_eq!(app.kept_renderer(0).as_deref(), Some("difftastic"));
        app.missing_renderers.clear();
        assert_eq!(app.kept_renderer(0).as_deref(), Some("word"));
    }

    #[test]
    fn an_inbox_where_every_group_failed_shows_the_error_not_empty_groups() {
        let configs: Vec<GroupConfig> = ["A", "B"]
            .map(|title| GroupConfig {
                title: title.to_string(),
                query: String::new(),
                collapsed: false,
            })
            .to_vec();
        let mut app = App::new();
        let generation = app.start_inbox(&configs);
        let limit = "API rate limit exceeded".to_string();
        app.set_group(generation, 0, Err(limit.clone()), Utc::now());
        assert!(app.inbox_failure().is_none(), "B is still loading");
        app.set_group(generation, 1, Err(limit.clone()), Utc::now());
        assert_eq!(
            app.inbox_failure(),
            Some(vec![limit.clone()]),
            "one copy of a shared error"
        );

        let generation = app.start_inbox(&configs);
        app.set_group(generation, 0, Ok(group("A", &[1], false)), Utc::now());
        app.set_group(generation, 1, Err(limit), Utc::now());
        assert!(app.inbox_failure().is_none(), "one group loaded");
    }

    #[test]
    fn the_gh_screen_takes_only_retry_and_quit() {
        let mut app = app_with(vec![group("A", &[7], false)]);
        app.gh_problem = Some(crate::github::GhProblem::Missing);
        assert!(app.handle_key(key(KeyCode::Enter)).is_none());
        assert!(matches!(app.screen, Screen::Inbox));
        assert!(matches!(
            app.handle_key(key(KeyCode::Char('q'))),
            Some(Action::Quit)
        ));
        assert!(matches!(
            app.handle_key(key(KeyCode::Char('r'))),
            Some(Action::Refresh)
        ));
        assert!(app.gh_problem.is_none(), "r clears it until the next check");
    }

    #[test]
    fn m_skips_a_renderer_that_is_not_installed() {
        let missing = HashSet::from([1]);
        assert_eq!(next_renderer(0, 3, &missing), 2);
        assert_eq!(next_renderer(2, 3, &missing), 0);
        assert_eq!(
            next_renderer(0, 1, &HashSet::from([0])),
            0,
            "a lone renderer stays"
        );
    }

    #[test]
    fn the_label_menu_filters_ticks_and_sends_only_the_changes() {
        let label = |id: &str, name: &str| RepoLabel {
            id: id.to_string(),
            name: name.to_string(),
            color: "ededed".to_string(),
        };
        let (mut app, pr) = files_tab(vec![file("a.rs", ViewedState::Unviewed)]);
        assert!(
            app.handle_key(key(KeyCode::Char('e'))).is_none(),
            "e works on the Overview only"
        );
        app.handle_key(key(KeyCode::Char('1')));
        let action = app.handle_key(key(KeyCode::Char('e')));
        assert!(matches!(action, Some(Action::FetchLabels { .. })));
        let current = app.label_menu.as_ref().unwrap().current.clone();
        let kept = current.iter().next().cloned().unwrap_or_default();
        let labels = vec![label("L1", &kept), label("L2", "docs"), label("L3", "bug")];
        app.set_repo_labels(&pr, Ok(labels));
        type_text(&mut app, "do");
        assert_eq!(
            app.label_menu.as_ref().unwrap().shown().len(),
            1,
            "only docs holds do"
        );
        app.handle_key(key(KeyCode::Char(' ')));
        let action = app.handle_key(key(KeyCode::Enter));
        let Some(Action::SetLabels { add, remove, .. }) = action else {
            panic!("enter applies the change");
        };
        assert_eq!(
            (add, remove),
            (vec!["L2".to_string()], Vec::<String>::new())
        );
        assert!(app.label_menu.is_none());
        let again = app.handle_key(key(KeyCode::Char('e')));
        assert!(
            again.is_none(),
            "the second open uses the labels already loaded"
        );
        assert!(matches!(
            app.label_menu.as_ref().unwrap().labels,
            Some(Ok(_))
        ));
    }

    #[test]
    fn y_copies_a_file_path_or_a_folder_path() {
        let (mut app, _) = files_tab(vec![file("src/a.rs", ViewedState::Unviewed)]);
        view_mut(&mut app).tree_view = false;
        view_mut(&mut app).file_state.select(Some(0));
        let action = app.handle_key(key(KeyCode::Char('y')));
        assert!(
            matches!(action, Some(Action::Copy(ref text)) if text == "src/a.rs"),
            "{action:?}"
        );
        assert_eq!(app.saved.as_deref(), Some("copied src/a.rs"));
        let (mut app, _) = on_src_folder(vec![file("src/a.rs", ViewedState::Unviewed)]);
        let action = app.handle_key(key(KeyCode::Char('y')));
        assert!(
            matches!(action, Some(Action::Copy(ref text)) if text == "src/"),
            "{action:?}"
        );
    }

    /// o, then enter on GitHub, the menu's second choice.
    fn open_on_github(app: &mut App) -> Option<Action> {
        assert!(app.handle_key(key(KeyCode::Char('o'))).is_none());
        let menu = app
            .open_menu
            .as_ref()
            .expect("o on a file asks where to open it");
        if menu.selected == 0 {
            app.handle_key(key(KeyCode::Char('j')));
        }
        app.handle_key(key(KeyCode::Enter))
    }

    /// A clone folder holding `paths`, so the editor has a file to open. The user has
    /// picked an editor before, so o opens it without asking.
    fn clone_with(app: &mut App, pr: &PrRef, paths: &[&str]) -> PathBuf {
        app.prefs.editor_picked = true;
        let dir = std::env::temp_dir().join(format!(
            "revu-open-{}-{}",
            std::process::id(),
            paths.join("-").replace('/', "_")
        ));
        for path in paths {
            let file = dir.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, "x\n").unwrap();
        }
        app.set_repo(
            pr,
            Ok(LocalRepo {
                dir: dir.clone(),
                renames: HashMap::new(),
            }),
        );
        dir
    }

    #[test]
    fn o_on_a_changed_file_opens_the_editor_at_its_first_change() {
        let (mut app, pr) = files_tab(vec![file("a.md", ViewedState::Unviewed)]);
        let dir = clone_with(&mut app, &pr, &["a.md"]);
        app.handle_key(key(KeyCode::Char('o')));
        let Some(Action::OpenEditor(target)) = app.handle_key(key(KeyCode::Enter)) else {
            panic!("expected the editor");
        };
        assert_eq!(
            (target.dir, target.path.as_str(), target.line),
            (dir, "a.md", None)
        );
        let (_, head) = target
            .changes
            .expect("the first change, between base and head");
        assert_eq!(head, "head999");
        assert!(app.open_menu.is_none());
    }

    #[test]
    fn the_first_editor_open_asks_which_editor_then_opens_the_file_in_it() {
        let (mut app, pr) = files_tab(vec![file("a.md", ViewedState::Unviewed)]);
        clone_with(&mut app, &pr, &["a.md"]);
        app.prefs.editor_picked = false;
        app.handle_key(key(KeyCode::Char('o')));
        assert!(app.handle_key(key(KeyCode::Enter)).is_none());
        let menu = app.editor_menu.as_mut().expect("the editor list");
        assert_eq!(
            menu.open.as_ref().map(|target| target.path.as_str()),
            Some("a.md")
        );
        menu.choices = vec![
            EditorChoice::Shell,
            EditorChoice::Known {
                label: "Zed",
                program: "zed",
            },
            EditorChoice::Custom,
        ];
        app.handle_key(key(KeyCode::Char('j')));
        let Some(Action::SetEditor {
            editor: Some(editor),
            open: Some(target),
        }) = app.handle_key(key(KeyCode::Enter))
        else {
            panic!("expected Zed saved and the file opened");
        };
        assert_eq!(
            (editor.command.as_str(), target.path.as_str()),
            ("zed {dir} {file}:{line}", "a.md")
        );

        // Once picked, o opens the editor at once.
        app.prefs.editor_picked = true;
        app.handle_key(key(KeyCode::Char('o')));
        let action = app.handle_key(key(KeyCode::Enter));
        assert!(matches!(action, Some(Action::OpenEditor(_))), "{action:?}");
    }

    #[test]
    fn the_editor_says_why_when_the_file_is_not_checked_out() {
        let (mut app, pr) = files_tab(vec![file("a.md", ViewedState::Unviewed)]);
        clone_with(&mut app, &pr, &["other.md"]);
        app.handle_key(key(KeyCode::Char('o')));
        app.handle_key(key(KeyCode::Char('j')));
        assert!(app.handle_key(key(KeyCode::Enter)).is_none());
        let menu = app
            .open_menu
            .as_ref()
            .expect("the menu stays, with the reason");
        assert!(
            menu.editor
                .as_ref()
                .unwrap_err()
                .contains("check out the PR branch")
        );
        app.handle_key(key(KeyCode::Esc));
        assert!(app.open_menu.is_none());
    }

    #[test]
    fn select_lines_opens_the_editor_at_the_cursor_or_the_next_new_line() {
        let (mut app, pr) = files_tab(vec![file("a.md", ViewedState::Unviewed)]);
        clone_with(&mut app, &pr, &["a.md"]);
        view_mut(&mut app).line_mode = Some(LineMode {
            path: "a.md".to_string(),
            rows: Some(Ok(crate::line_diff::parse(
                "@@ -1,3 +1,3 @@\n keep\n-old\n+new\n last\n",
            ))),
            // The removed line: it has no new line, so the added line after it.
            cursor: 2,
            anchor: None,
            scroll: 0,
        });
        app.handle_key(key(KeyCode::Char('o')));
        let Some(Action::OpenEditor(target)) = app.handle_key(key(KeyCode::Enter)) else {
            panic!("expected the editor");
        };
        assert_eq!((target.line, target.changes), (Some(2), None));
    }

    #[test]
    fn a_repo_match_knows_its_block_and_the_diffs_match_count() {
        let (mut app, pr) = files_tab(vec![file("a.md", ViewedState::Unviewed)]);
        clone_with(&mut app, &pr, &["a.md", "b.py"]);
        app.handle_key(key(KeyCode::Char('/')));
        type_text(&mut app, "needle");
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(view(&app).diff_match_count(), None, "git still looks");
        app.set_find_files(&pr, "needle", Ok(vec![("a.md".to_string(), 5)]));
        assert_eq!(view(&app).diff_match_count(), Some(5));
        let hit = GrepHit {
            path: "b.py".to_string(),
            line: 3,
            text: "needle".to_string(),
        };
        app.set_find_repo(&pr, "needle", Ok(vec![hit]));
        for _ in 0..6 {
            if view(&app).repo_hit.is_some() {
                break;
            }
            app.handle_key(key(KeyCode::Char('n')));
        }
        assert_eq!(view(&app).repo_hit, Some(0));
        assert_eq!(view(&app).file_view_place(), None, "not parsed yet");
        let code = "class A:\n    def f(self):\n        needle()\n";
        let outline = crate::syntax::outline("b.py", code.as_bytes());
        app.set_outline(&pr, "head999".to_string(), "b.py".to_string(), outline);
        assert_eq!(view(&app).file_view_place().as_deref(), Some("A > f"));
    }

    #[test]
    fn esc_clears_the_find_and_never_leaves_the_pr() {
        let (mut app, pr) = files_tab(vec![file("a.md", ViewedState::Unviewed)]);
        clone_with(&mut app, &pr, &["a.md"]);
        app.handle_key(key(KeyCode::Char('/')));
        type_text(&mut app, "needle");
        app.handle_key(key(KeyCode::Enter));
        app.set_find_files(&pr, "needle", Ok(vec![("a.md".to_string(), 2)]));
        app.handle_key(key(KeyCode::Esc));
        let view = view(&app);
        assert!(view.find.is_none() && view.find_files.is_none() && view.find_repo.is_none());
        assert!(matches!(app.screen, Screen::Detail(_)), "still on the PR");
        app.handle_key(key(KeyCode::Esc));
        assert!(
            matches!(app.screen, Screen::Detail(_)),
            "with no find, esc does nothing"
        );
        app.handle_key(key(KeyCode::Char('h')));
        assert!(matches!(app.screen, Screen::Inbox), "h goes back");
    }

    #[test]
    fn the_file_view_opens_with_a_few_wrapped_rows_above_its_line() {
        let text = Text::from(
            ["a", "b", "c", "d", "e", "f", "g", "h"]
                .iter()
                .map(|line| ratatui::text::Line::from(*line))
                .collect::<Vec<_>>(),
        );
        assert_eq!(context_scroll(&text, 7, 80), 2, "five short lines above");
        assert_eq!(context_scroll(&text, 2, 80), 0, "the top of the file");
        let long = Text::from(vec![
            ratatui::text::Line::from("x".repeat(600)),
            ratatui::text::Line::from("y"),
            ratatui::text::Line::from("needle"),
        ]);
        assert_eq!(
            context_scroll(&long, 2, 100),
            1,
            "a 6-row line above does not fit"
        );
        assert_eq!(context_scroll(&long, 2, 200), 0, "at 3 rows it does");
        assert_eq!(context_scroll(&long, 2, 0), 0, "no width yet: no wrap");
    }

    #[test]
    fn o_on_a_repo_match_opens_the_file_at_the_pr_head_and_line() {
        let (mut app, pr) = files_tab(vec![file("a.md", ViewedState::Unviewed)]);
        clone_with(&mut app, &pr, &["a.md", "docs/x y.py"]);
        app.handle_key(key(KeyCode::Char('/')));
        type_text(&mut app, "needle");
        app.handle_key(key(KeyCode::Enter));
        app.set_find_files(&pr, "needle", Ok(Vec::new()));
        let hit = GrepHit {
            path: "docs/x y.py".to_string(),
            line: 3,
            text: "needle".to_string(),
        };
        app.set_find_repo(&pr, "needle", Ok(vec![hit]));
        app.handle_key(key(KeyCode::Char('n')));
        let blob = format!(
            "https://github.com/{}/blob/head999/docs/x%20y.py#L3",
            pr.repo
        );
        let action = open_on_github(&mut app);
        assert!(
            matches!(action, Some(Action::OpenBrowser(ref url)) if *url == blob),
            "{action:?}"
        );
        assert_eq!(view(&app).overlays.len(), 1, "the file stays open");
        app.handle_key(key(KeyCode::Char('o')));
        let Some(Action::OpenEditor(target)) = app.handle_key(key(KeyCode::Enter)) else {
            panic!("expected the editor");
        };
        assert_eq!(
            (target.path.as_str(), target.line),
            ("docs/x y.py", Some(3))
        );
    }

    #[test]
    fn o_away_from_a_file_opens_the_browser_at_once() {
        let (mut app, pr) = files_tab(vec![file("a.md", ViewedState::Unviewed)]);
        app.handle_key(key(KeyCode::Char('1')));
        let action = app.handle_key(key(KeyCode::Char('o')));
        assert!(
            matches!(action, Some(Action::OpenBrowser(ref url)) if *url == pr.url()),
            "{action:?}"
        );
        assert!(app.open_menu.is_none());
    }

    #[test]
    fn o_opens_the_selected_file_or_line_in_githubs_diff() {
        let anchor = format!(
            "{}/files#diff-{}",
            PrRef {
                repo: "acme/app".to_string(),
                number: 7
            }
            .url(),
            crate::sha256::hex(b"a.rs")
        );
        let (mut app, _) = files_tab(vec![file("a.rs", ViewedState::Unviewed)]);
        view_mut(&mut app).tree_view = false;
        view_mut(&mut app).file_state.select(Some(0));
        let action = open_on_github(&mut app);
        assert!(
            matches!(action, Some(Action::OpenBrowser(ref url)) if *url == anchor),
            "{action:?}"
        );
        view_mut(&mut app).line_mode = Some(LineMode {
            path: "a.rs".to_string(),
            rows: Some(Ok(crate::line_diff::parse(
                "@@ -1,2 +1,2 @@\n keep\n-old\n+new\n",
            ))),
            cursor: 2,
            anchor: None,
            scroll: 0,
        });
        let action = open_on_github(&mut app);
        let removed = format!("{anchor}L2");
        assert!(
            matches!(action, Some(Action::OpenBrowser(ref url)) if *url == removed),
            "{action:?}"
        );
    }

    #[test]
    fn y_in_comment_mode_copies_the_line_or_the_range_as_a_snippet() {
        let (mut app, _) = files_tab(vec![file("a.rs", ViewedState::Unviewed)]);
        view_mut(&mut app).line_mode = Some(LineMode {
            path: "a.rs".to_string(),
            rows: Some(Ok(crate::line_diff::parse(
                "@@ -1,2 +1,2 @@\n keep\n-old\n+new\n",
            ))),
            cursor: 3,
            anchor: Some(1),
            scroll: 0,
        });
        let action = app.handle_key(key(KeyCode::Char('y')));
        let expected = "a.rs:1-2\n```diff\n keep\n-old\n+new\n```\n";
        assert!(
            matches!(action, Some(Action::Copy(ref text)) if text == expected),
            "{action:?}"
        );
        assert_eq!(app.saved.as_deref(), Some("copied a.rs:1-2 (3 lines)"));
        assert!(view(&app).line_mode.is_some(), "select lines stays open");
    }

    #[test]
    fn capital_s_opens_and_closes_select_lines_and_c_does_not() {
        let (mut app, pr) = files_tab(vec![file("a.rs", ViewedState::Unviewed)]);
        let clone = LocalRepo {
            dir: PathBuf::from("/clone"),
            renames: HashMap::new(),
        };
        app.set_repo(&pr, Ok(clone));
        view_mut(&mut app).tree_view = false;
        view_mut(&mut app).diff_width = 80;
        view_mut(&mut app).file_state.select(Some(0));
        app.handle_key(key(KeyCode::Char('c')));
        assert!(view(&app).line_mode.is_none(), "c no longer opens it");
        app.handle_key(key(KeyCode::Char('S')));
        assert!(view(&app).line_mode.is_some(), "{:?}", app.error);
        app.handle_key(key(KeyCode::Char('S')));
        assert!(view(&app).line_mode.is_none(), "S closes it again");
    }

    #[test]
    fn an_older_load_that_finishes_last_does_not_replace_the_newer_one() {
        let (mut app, pr) = files_tab(Vec::new());
        let first = app.start_load(&pr).unwrap();
        let second = app.start_load(&pr).unwrap();
        let mut newer = crate::model::tests::sample_detail();
        newer.head_oid = "new".to_string();
        let mut older = crate::model::tests::sample_detail();
        older.head_oid = "old".to_string();
        app.set_loaded_detail(&pr, second, Ok(newer));
        app.set_loaded_detail(&pr, first, Err("late and stale".to_string()));
        app.set_loaded_detail(&pr, first, Ok(older));
        let Some(Ok(shown)) = &view(&app).detail else {
            panic!("the newer detail stays");
        };
        assert_eq!(shown.head_oid, "new");
        let activity = crate::model::tests::sample_activity();
        app.set_loaded_activity(&pr, first, Err("stale".to_string()));
        assert!(
            matches!(view(&app).activity, Some(Ok(_))),
            "stale activity dropped"
        );
        app.set_loaded_activity(&pr, second, Ok(activity));
        assert!(matches!(view(&app).activity, Some(Ok(_))));
    }

    #[test]
    fn the_diff_pane_names_the_block_of_the_hunk_at_its_top() {
        let (mut app, pr) = files_tab(vec![file("app.py", ViewedState::Unviewed)]);
        ready_repo(&mut app, &pr);
        let job = app.take_diff_request().unwrap();
        assert!(app.wants_structure(&job), "asked once");
        assert!(!app.wants_structure(&job), "not again");
        let drawn = "@@ -1,3 +1,3 @@\n a\n b\n@@ -10,3 +10,3 @@\n c\n d\n";
        app.set_diff(
            &pr,
            job.key,
            crate::highlight::to_text(drawn.as_bytes(), None),
        );
        let head = "class A:\n    def one(self):\n        x = 1\n\n\n\n\n\nclass B:\n    def two(self):\n        y = 2\n";
        let outline = crate::syntax::outline("app.py", head.as_bytes());
        app.set_outline(&pr, "head999".to_string(), "app.py".to_string(), outline);
        let firsts = vec![
            crate::line_diff::LineAt::New(3),
            crate::line_diff::LineAt::New(11),
        ];
        app.set_hunk_firsts(&pr, "app.py".to_string(), firsts);
        assert_eq!(view(&app).hunk_place().as_deref(), Some("A > one"));
        view_mut(&mut app).diff_scroll = 3;
        assert_eq!(view(&app).hunk_place().as_deref(), Some("B > two"));
        // A renderer whose hunks do not line up with git's names nothing.
        app.set_hunk_firsts(
            &pr,
            "app.py".to_string(),
            vec![crate::line_diff::LineAt::New(3)],
        );
        assert_eq!(view(&app).hunk_place(), None);
        // Nor does difftastic, even with as many hunks as git.
        let firsts = vec![
            crate::line_diff::LineAt::New(3),
            crate::line_diff::LineAt::New(11),
        ];
        app.set_hunk_firsts(&pr, "app.py".to_string(), firsts);
        let key = view(&app).current_diff_key().unwrap();
        let drawn = "app.py --- 1/2 --- Python\n a\napp.py --- 2/2 --- Python\n b\n";
        app.set_diff(&pr, key, crate::highlight::to_text(drawn.as_bytes(), None));
        assert_eq!(view(&app).hunk_place(), None, "difftastic");
    }

    #[test]
    fn new_commits_after_a_reload_drop_the_stale_files_and_diffs() {
        let (mut app, pr) = files_tab(vec![file("a.md", ViewedState::Unviewed)]);
        ready_repo(&mut app, &pr);
        let same = crate::model::tests::sample_detail();
        app.set_detail(&pr, Ok(same.clone()));
        assert!(
            view(&app).files.is_some(),
            "the same commits keep the files"
        );
        let firsts = vec![crate::line_diff::LineAt::New(1)];
        app.set_hunk_firsts(&pr, "a.md".to_string(), firsts);
        let mut pushed = same;
        pushed.head_oid = "pushed".to_string();
        app.set_detail(&pr, Ok(pushed));
        assert!(view(&app).files.is_none(), "the file list loads again");
        assert!(
            view(&app).hunk_firsts.is_empty(),
            "the hunks are read again"
        );
        assert!(matches!(view(&app).repo, RepoState::Unknown));
        assert_eq!(view(&app).tab, Tab::Files);
        assert!(
            matches!(view(&app).activity, Some(Ok(_))),
            "the activity stays"
        );
        assert!(app.take_files_request().is_some());
    }

    #[test]
    fn a_failed_label_write_loads_the_pr_again_to_show_what_github_has() {
        let (mut app, pr) = files_tab(Vec::new());
        let labels = Action::SetLabels {
            pr: pr.clone(),
            pull_request_id: "PR_node".to_string(),
            add: vec!["L1".to_string()],
            remove: vec!["L2".to_string()],
        };
        app.saving = labels.save_note();
        app.set_written(&pr, Err("label not found".to_string()));
        assert_eq!(app.reload_after_write.as_ref(), Some(&pr));
        assert_eq!(app.error.as_deref(), Some("label not found"));
        let discard = Action::DiscardReview {
            pr: pr.clone(),
            pending_review: "R1".to_string(),
        };
        app.reload_after_write = None;
        app.saving = discard.save_note();
        app.set_written(&pr, Err("offline".to_string()));
        assert!(
            app.reload_after_write.is_none(),
            "other writes keep the screen as it is"
        );
    }

    #[test]
    fn select_lines_names_the_block_around_the_cursor_on_either_side() {
        let (mut app, pr) = files_tab(vec![file("app.py", ViewedState::Unviewed)]);
        let parse = |code: &str| crate::syntax::outline("app.py", code.as_bytes());
        let base = "class A:\n    def old(self):\n        x = 1\n";
        let head = "class A:\n    def new(self):\n        x = 2\n";
        app.set_outline(
            &pr,
            "base000".to_string(),
            "app.py".to_string(),
            parse(base),
        );
        app.set_outline(
            &pr,
            "head999".to_string(),
            "app.py".to_string(),
            parse(head),
        );
        view_mut(&mut app).line_mode = Some(LineMode {
            path: "app.py".to_string(),
            rows: Some(Ok(crate::line_diff::parse(
                "@@ -1,3 +1,3 @@\n class A:\n-    def old(self):\n-        x = 1\n+    def new(self):\n+        x = 2\n",
            ))),
            cursor: 1,
            anchor: None,
            scroll: 0,
        });
        let place = |app: &mut App, cursor| {
            view_mut(app).line_mode.as_mut().unwrap().cursor = cursor;
            view(app).line_place()
        };
        assert_eq!(place(&mut app, 0), None, "the hunk header");
        assert_eq!(place(&mut app, 1).as_deref(), Some("A"));
        assert_eq!(
            place(&mut app, 3).as_deref(),
            Some("A > old"),
            "removed: the base"
        );
        assert_eq!(
            place(&mut app, 5).as_deref(),
            Some("A > new"),
            "added: the head"
        );
    }

    #[test]
    fn d_offers_the_lines_names_then_walks_the_definitions() {
        let (mut app, pr) = files_tab(vec![file("app.py", ViewedState::Unviewed)]);
        ready_repo(&mut app, &pr);
        view_mut(&mut app).line_mode = Some(LineMode {
            path: "app.py".to_string(),
            rows: Some(Ok(crate::line_diff::parse(
                "@@ -1,1 +1,1 @@\n-old()\n+bar = foo(x)\n",
            ))),
            cursor: 2,
            anchor: None,
            scroll: 0,
        });
        let Some(Action::LineNames { commit, line, .. }) = app.handle_key(key(KeyCode::Char('D')))
        else {
            panic!("expected the line's names");
        };
        assert_eq!((commit.as_str(), line), ("head999", 1));
        app.set_line_names(
            &pr,
            vec!["foo".to_string(), "bar".to_string(), "x".to_string()],
        );
        let prompt = app.prompt.as_ref().expect("several names: a prompt");
        assert_eq!(prompt.text, "foo", "the call first");
        let Some(Action::FindDefinition { name, .. }) = app.handle_key(key(KeyCode::Enter)) else {
            panic!("expected a lookup");
        };
        assert_eq!(name, "foo");
        let hit = |path: &str, line| GrepHit {
            path: path.to_string(),
            line,
            text: "def foo():".to_string(),
        };
        app.set_definitions(&pr, name, Ok((vec![hit("a.py", 3), hit("b.py", 9)], false)));
        let Some(Action::ShowFile { path, line, .. }) = app.queued.take() else {
            panic!("expected the first definition");
        };
        assert_eq!((path.as_str(), line), ("a.py", 3));
        let Some(Action::ShowFile { path, .. }) = app.handle_key(key(KeyCode::Char('n'))) else {
            panic!("expected the next definition");
        };
        assert_eq!(path, "b.py");
        assert_eq!(view(&app).overlays.len(), 1, "one file view at a time");
        let Some(Action::ShowFile { path, .. }) = app.handle_key(key(KeyCode::Char('n'))) else {
            panic!("expected a wrap");
        };
        assert_eq!(path, "a.py", "past the last, back to the first");
        app.handle_key(key(KeyCode::Esc));
        assert!(view(&app).overlays.is_empty() && view(&app).def_walk.is_none());
        assert!(view(&app).line_mode.is_some(), "back in select lines");
    }

    #[test]
    fn d_looks_up_a_lone_name_at_once_and_says_when_nothing_defines_it() {
        let (mut app, pr) = files_tab(vec![file("app.py", ViewedState::Unviewed)]);
        ready_repo(&mut app, &pr);
        app.set_line_names(&pr, vec!["foo".to_string()]);
        assert!(app.prompt.is_none());
        assert!(matches!(
            app.queued.take(),
            Some(Action::FindDefinition { .. })
        ));
        app.set_definitions(&pr, "foo".to_string(), Ok((Vec::new(), false)));
        assert_eq!(
            app.error.as_deref(),
            Some("no definition of foo at the PR head; it may come from a library")
        );
        app.set_line_names(&pr, Vec::new());
        assert_eq!(
            app.error.as_deref(),
            Some("no names on this line to look up")
        );
    }

    #[test]
    fn t_opens_every_thread_and_brackets_walk_the_thread_lines() {
        let (mut app, pr) = files_tab(vec![file("b.md", ViewedState::Unviewed)]);
        app.set_activity(&pr, Ok(crate::model::tests::sample_activity()));
        view_mut(&mut app).line_mode = Some(LineMode {
            path: "b.md".to_string(),
            rows: Some(Ok(crate::line_diff::parse(
                "@@ -10,3 +10,3 @@\n x\n y\n z\n",
            ))),
            cursor: 3,
            anchor: None,
            scroll: 0,
        });
        let cursor = |app: &App| view(app).line_mode.as_ref().unwrap().cursor;
        view_mut(&mut app).line_mode.as_mut().unwrap().cursor = 1;
        app.handle_key(key(KeyCode::Char('t')));
        assert!(view(&app).threads_open, "t works on any line");
        app.handle_key(key(KeyCode::Char('t')));
        assert!(!view(&app).threads_open, "t again closes them");
        app.handle_key(key(KeyCode::Char(']')));
        assert_eq!(cursor(&app), 3, "line 12 holds the thread");
        app.handle_key(key(KeyCode::Char(']')));
        assert_eq!(app.error.as_deref(), Some("no review thread below"));
        assert_eq!(cursor(&app), 3);
        app.handle_key(key(KeyCode::Char('[')));
        assert_eq!(app.error.as_deref(), Some("no review thread above"));
        view_mut(&mut app).line_mode.as_mut().unwrap().path = "c.md".to_string();
        app.handle_key(key(KeyCode::Char('t')));
        assert_eq!(
            app.error.as_deref(),
            Some("this file has no review threads")
        );
    }

    #[test]
    fn bracket_up_past_the_first_thread_line_goes_to_the_outdated_threads() {
        let (mut app, pr) = files_tab(vec![file("a.md", ViewedState::Unviewed)]);
        app.set_activity(&pr, Ok(crate::model::tests::sample_activity()));
        view_mut(&mut app).line_mode = Some(LineMode {
            path: "a.md".to_string(),
            rows: Some(Ok(crate::line_diff::parse("@@ -1,3 +1,3 @@\n x\n y\n z\n"))),
            cursor: 3,
            anchor: None,
            scroll: 40,
        });
        app.handle_key(key(KeyCode::Char('[')));
        assert_eq!(app.error, None);
        let mode = view(&app).line_mode.as_ref().unwrap();
        assert_eq!((mode.cursor, mode.scroll), (0, 0), "the top shows them");
        assert_eq!(view(&app).line_followed, Some(0), "and stays there");
    }

    #[test]
    fn a_parsed_file_is_known_so_it_is_not_parsed_again() {
        let (mut app, pr) = files_tab(vec![file("b.py", ViewedState::Unviewed)]);
        assert!(!app.knows_outline(&pr, "c1", "b.py"));
        app.set_outline(&pr, "c1".to_string(), "b.py".to_string(), None);
        assert!(
            app.knows_outline(&pr, "c1", "b.py"),
            "a file with no outline counts too"
        );
        assert!(
            !app.knows_outline(&pr, "c2", "b.py"),
            "another commit is another file"
        );
    }

    #[test]
    fn r_replies_to_the_thread_on_the_line_and_u_resolves_it() {
        let (mut app, pr) = files_tab(vec![file("b.md", ViewedState::Unviewed)]);
        let lines = |cursor| LineMode {
            path: "b.md".to_string(),
            rows: Some(Ok(crate::line_diff::parse(
                "@@ -10,3 +10,3 @@\n x\n y\n z\n",
            ))),
            cursor,
            anchor: None,
            scroll: 0,
        };
        view_mut(&mut app).line_mode = Some(lines(1));
        app.handle_key(key(KeyCode::Char('r')));
        assert_eq!(
            app.error.as_deref(),
            Some("no review thread on this line; ] goes to the next")
        );
        assert!(app.handle_key(key(KeyCode::Char('u'))).is_none());
        view_mut(&mut app).line_mode = Some(lines(3));
        app.handle_key(key(KeyCode::Char('r')));
        let kind = app.composer.as_ref().map(|composer| composer.kind.clone());
        assert_eq!(
            kind,
            Some(ComposeKind::Reply {
                thread: "T2".to_string(),
                author: "lead".to_string(),
                place: "b.md line 12".to_string(),
            })
        );
        type_text(&mut app, "thanks");
        app.handle_key(ctrl('s'));
        let pending = app.composer.as_ref().unwrap().pending_review.is_some();
        let Some(Action::ReplyToThread {
            pr: to,
            thread,
            body,
            pending: joins,
        }) = app.handle_key(key(KeyCode::Char('y')))
        else {
            panic!("y sends the reply");
        };
        assert_eq!(
            (to, thread, body, joins),
            (pr.clone(), "T2".to_string(), "thanks".to_string(), pending)
        );
        app.composer = None;
        let Some(Action::ResolveThread {
            thread, resolved, ..
        }) = app.handle_key(key(KeyCode::Char('u')))
        else {
            panic!("u resolves the thread");
        };
        assert_eq!((thread.as_str(), resolved), ("T2", true));
    }

    #[test]
    fn u_on_the_first_row_reopens_the_outdated_thread() {
        let (mut app, pr) = files_tab(vec![file("a.md", ViewedState::Unviewed)]);
        view_mut(&mut app).line_mode = Some(LineMode {
            path: "a.md".to_string(),
            rows: Some(Ok(crate::line_diff::parse("@@ -1,1 +1,1 @@\n x\n"))),
            cursor: 0,
            anchor: None,
            scroll: 0,
        });
        let Some(Action::ResolveThread {
            pr: on,
            thread,
            resolved,
        }) = app.handle_key(key(KeyCode::Char('u')))
        else {
            panic!("u acts on the outdated thread");
        };
        assert_eq!(on, pr);
        assert_eq!(
            (thread.as_str(), resolved),
            ("T1", false),
            "T1 is resolved, so u reopens it"
        );
    }

    #[test]
    fn a_click_flips_one_thread_and_t_resets_every_one() {
        let (mut app, pr) = files_tab(vec![file("b.md", ViewedState::Unviewed)]);
        app.set_activity(&pr, Ok(crate::model::tests::sample_activity()));
        view_mut(&mut app).line_mode = Some(LineMode {
            path: "b.md".to_string(),
            rows: Some(Ok(crate::line_diff::parse("@@ -12,1 +12,1 @@\n z\n"))),
            cursor: 1,
            anchor: None,
            scroll: 0,
        });
        view_mut(&mut app).clicks = vec![ClickTarget {
            area: ratatui::layout::Rect::new(0, 9, 40, 1),
            click: Click::Thread(0),
        }];
        app.handle_click(5, 9);
        assert!(view(&app).thread_open("T2"), "the click opens it");
        app.handle_click(5, 9);
        assert!(!view(&app).thread_open("T2"), "a second click closes it");
        app.handle_click(5, 9);
        app.handle_key(key(KeyCode::Char('t')));
        assert!(view(&app).thread_open("T2"), "t opens every thread");
        assert!(
            view(&app).flipped_threads.is_empty(),
            "and forgets the clicks"
        );
    }

    #[test]
    fn an_unchanged_line_holds_threads_on_either_side() {
        let rows = crate::line_diff::parse("@@ -9,2 +12,2 @@\n same\n-gone\n+new\n");
        let (same, gone, new) = (&rows[1], &rows[2], &rows[3]);
        use crate::model::Side::{Left, Right};
        assert!(row_holds(same, (Left, 9)) && row_holds(same, (Right, 12)));
        assert!(row_holds(gone, (Left, 10)) && !row_holds(gone, (Right, 13)));
        assert!(row_holds(new, (Right, 13)) && !row_holds(new, (Left, 10)));
    }

    #[test]
    fn select_lines_on_a_binary_file_says_so_instead_of_crashing() {
        let (mut app, _) = files_tab(vec![file("logo.png", ViewedState::Unviewed)]);
        let diff =
            "diff --git a/logo.png b/logo.png\nBinary files a/logo.png and b/logo.png differ\n";
        view_mut(&mut app).line_mode = Some(LineMode {
            path: "logo.png".to_string(),
            rows: Some(Ok(crate::line_diff::parse(diff))),
            cursor: 0,
            anchor: None,
            scroll: 0,
        });
        // o opens the menu, with no line for the editor; esc closes it.
        for code in [
            KeyCode::Enter,
            KeyCode::Char('y'),
            KeyCode::Char('o'),
            KeyCode::Esc,
        ] {
            app.handle_key(key(code));
        }
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(
            app.error.as_deref(),
            Some("this file has no lines to comment on")
        );
    }

    #[test]
    fn the_wheel_moves_the_cursor_in_select_lines() {
        let (mut app, _) = files_tab(vec![file("a.rs", ViewedState::Unviewed)]);
        let diff = "@@ -1,6 +1,6 @@\n a\n b\n c\n d\n e\n f\n";
        view_mut(&mut app).line_mode = Some(LineMode {
            path: "a.rs".to_string(),
            rows: Some(Ok(crate::line_diff::parse(diff))),
            cursor: 0,
            anchor: None,
            scroll: 0,
        });
        let column = view(&app).diff_left + 5;
        app.handle_scroll(column, 1);
        assert_eq!(view(&app).line_mode.as_ref().unwrap().cursor, 3);
        app.handle_scroll(column, 1);
        assert_eq!(
            view(&app).line_mode.as_ref().unwrap().cursor,
            6,
            "stops at the last row"
        );
        app.handle_scroll(column, -1);
        assert_eq!(view(&app).line_mode.as_ref().unwrap().cursor, 3);
    }

    #[test]
    fn comment_mode_targets_a_line_or_a_one_sided_range() {
        let diff = "@@ -1,2 +1,2 @@\n keep\n-old\n+new\n";
        let mut mode = LineMode {
            path: "a.rs".to_string(),
            rows: Some(Ok(crate::line_diff::parse(diff))),
            cursor: 3,
            anchor: None,
            scroll: 0,
        };
        let target = mode.target().unwrap();
        assert_eq!(
            (target.side, target.line, target.start_line),
            (Side::Right, 2, None)
        );
        mode.anchor = Some(1);
        assert!(
            mode.target().unwrap_err().contains("one side"),
            "keep, old, new mixes sides"
        );
        mode.cursor = 1;
        mode.anchor = None;
        assert_eq!(
            mode.target().unwrap().line,
            1,
            "an unchanged line takes a comment"
        );
        mode.cursor = 0;
        assert!(mode.target().is_err(), "the hunk header takes none");
    }

    #[test]
    fn a_write_says_it_is_saving_then_saved_until_the_next_key() {
        let (mut app, pr) = files_tab(vec![file("a.rs", ViewedState::Unviewed)]);
        let action = Action::DiscardReview {
            pr: pr.clone(),
            pending_review: "P1".to_string(),
        };
        app.saving = action.save_note();
        app.set_written(&pr, Ok(()));
        assert_eq!(
            (app.saving, app.saved.as_deref()),
            (None, Some("pending review discarded"))
        );
        app.handle_key(key(KeyCode::Char('j')));
        assert!(app.saved.is_none(), "the next key clears it");
    }

    #[test]
    fn a_review_picks_its_verdict_confirms_and_sends() {
        let (mut app, pr) = files_tab(vec![file("a.rs", ViewedState::Unviewed)]);
        app.handle_key(key(KeyCode::Char('R')));
        app.handle_key(ctrl('s'));
        let error = app.composer.as_ref().unwrap().error.clone();
        assert!(
            error.unwrap().contains("write a comment"),
            "an empty Comment is refused"
        );
        app.handle_key(key(KeyCode::Tab));
        type_text(&mut app, "ship it");
        app.handle_key(ctrl('s'));
        assert!(app.composer.as_ref().unwrap().confirming);
        let action = app.handle_key(key(KeyCode::Char('y')));
        let Some(Action::SubmitReview {
            verdict,
            body,
            head_oid,
            ..
        }) = action
        else {
            panic!("y sends the review");
        };
        assert_eq!((verdict, body.as_str()), (Verdict::Approve, "ship it"));
        assert_eq!(
            head_oid, "head999",
            "the review is pinned to the commit shown"
        );
        app.set_written(&pr, Err("GitHub said no".to_string()));
        let composer = app.composer.as_ref().expect("a failed send keeps the text");
        assert_eq!(composer.editor.text, "ship it");
        app.set_written(&pr, Ok(()));
        assert!(app.composer.is_none());
        assert_eq!(
            app.reload_after_write,
            Some(pr),
            "the PR loads again to show the review"
        );
    }

    #[test]
    fn your_own_pr_takes_only_comment_and_esc_asks_before_dropping_text() {
        let (mut app, _) = files_tab(vec![file("a.rs", ViewedState::Unviewed)]);
        if let Screen::Detail(view) = &mut app.screen
            && let Some(Ok(detail)) = &mut view.detail
        {
            detail.viewer_is_author = true;
        }
        app.handle_key(key(KeyCode::Char('R')));
        app.handle_key(key(KeyCode::Tab));
        let kind = &app.composer.as_ref().unwrap().kind;
        assert!(matches!(
            kind,
            ComposeKind::Review {
                verdict: Verdict::Comment,
                ..
            }
        ));
        type_text(&mut app, "draft");
        app.handle_key(key(KeyCode::Esc));
        assert!(app.composer.is_some(), "the first esc asks");
        app.handle_key(key(KeyCode::Esc));
        assert!(app.composer.is_none());
    }

    #[test]
    fn ctrl_c_in_the_box_copies_the_selection() {
        let (mut app, _) = files_tab(vec![file("a.rs", ViewedState::Unviewed)]);
        app.handle_key(key(KeyCode::Char('R')));
        type_text(&mut app, "one two");
        app.handle_key(KeyEvent::new(
            KeyCode::Left,
            KeyModifiers::SHIFT | KeyModifiers::ALT,
        ));
        let action = app.handle_key(ctrl('c'));
        assert!(matches!(action, Some(Action::Copy(text)) if text == "two"));
        app.handle_paste("2");
        assert_eq!(app.composer.as_ref().unwrap().editor.text, "one 2");
    }

    /// An open PR with its detail loaded and the Files tab showing.
    fn mergeable(state: &str) -> PrDetail {
        let mut detail = crate::model::tests::sample_detail();
        detail.merge_state = state.to_string();
        detail.merge = crate::model::MergeOptions {
            methods: vec![MergeMethod::Squash, MergeMethod::Rebase],
            default_method: Some(MergeMethod::Rebase),
            can_write: true,
            ..Default::default()
        };
        detail
    }

    #[test]
    fn the_merge_menu_offers_what_github_would() {
        let items = |detail: &PrDetail| merge_items(detail);
        let methods = vec![
            MergeItem::Method(MergeMethod::Squash),
            MergeItem::Method(MergeMethod::Rebase),
        ];
        assert_eq!(items(&mergeable("CLEAN")), Ok(methods.clone()));
        assert_eq!(items(&mergeable("UNSTABLE")), Ok(methods.clone()));
        let blocked = mergeable("BLOCKED");
        assert!(items(&blocked).unwrap_err().contains("auto-merge is off"));
        let mut auto = mergeable("BLOCKED");
        auto.merge.can_enable_auto_merge = true;
        assert_eq!(items(&auto), Ok(methods.clone()));
        let mut admin = mergeable("BEHIND");
        admin.merge.can_bypass = true;
        let mut with_bypass = methods.clone();
        with_bypass.push(MergeItem::Bypass);
        assert_eq!(items(&admin), Ok(with_bypass));
        let mut on = mergeable("BLOCKED");
        on.merge.auto_merge = Some(MergeMethod::Squash);
        assert!(items(&on).unwrap_err().contains("auto-merge is on"));
        on.merge.can_disable_auto_merge = true;
        assert_eq!(items(&on), Ok(vec![MergeItem::DisableAuto]));
        let mut reader = mergeable("CLEAN");
        reader.merge.can_write = false;
        assert!(items(&reader).unwrap_err().contains("cannot merge"));
        assert!(
            items(&mergeable("DIRTY"))
                .unwrap_err()
                .contains("conflicts")
        );
        for checking in ["UNKNOWN", ""] {
            let error = items(&mergeable(checking)).unwrap_err();
            assert!(error.contains("still checking"), "{checking}: {error}");
        }
        let mut merged = mergeable("CLEAN");
        merged.lifecycle = Lifecycle::Merged { by: None, at: None };
        assert!(items(&merged).unwrap_err().contains("merged already"));
        let mut draft = mergeable("CLEAN");
        draft.lifecycle = Lifecycle::Draft;
        assert!(items(&draft).unwrap_err().contains("draft"));
    }

    /// Opens the PR on the given detail, presses M, moves j times, Enter, then y Enter.
    fn merge_through_menu(detail: PrDetail, keys: &[KeyCode]) -> (App, Option<Action>) {
        let (mut app, pr) = files_tab(Vec::new());
        app.set_detail(&pr, Ok(detail));
        app.handle_key(key(KeyCode::Char('M')));
        for code in keys {
            app.handle_key(key(*code));
        }
        if app.prompt.is_none() {
            return (app, None);
        }
        type_text(&mut app, "y");
        let action = app.handle_key(key(KeyCode::Enter));
        (app, action)
    }

    fn sent_request(action: Option<Action>) -> Option<MergeRequest> {
        match action {
            Some(Action::Merge {
                request, head_oid, ..
            }) => {
                assert_eq!(head_oid, "head999", "the head you saw guards the merge");
                Some(request)
            }
            _ => None,
        }
    }

    #[test]
    fn m_merges_with_the_picked_method_after_a_y() {
        let (app, action) = merge_through_menu(mergeable("CLEAN"), &[KeyCode::Enter]);
        assert_eq!(
            sent_request(action),
            Some(MergeRequest::Now(MergeMethod::Rebase)),
            "the cursor starts on GitHub's default method"
        );
        assert!(app.merge_menu.is_none() && app.prompt.is_none());
        let (_, action) =
            merge_through_menu(mergeable("CLEAN"), &[KeyCode::Char('k'), KeyCode::Enter]);
        assert_eq!(
            sent_request(action),
            Some(MergeRequest::Now(MergeMethod::Squash))
        );
        let (app, _) = merge_through_menu(mergeable("CLEAN"), &[KeyCode::Esc]);
        assert!(
            app.merge_menu.is_none() && app.prompt.is_none(),
            "esc sends nothing"
        );
    }

    #[test]
    fn a_blocked_merge_turns_on_auto_merge_or_bypasses_when_ticked() {
        let mut auto = mergeable("BLOCKED");
        auto.merge.can_enable_auto_merge = true;
        let (_, action) = merge_through_menu(auto, &[KeyCode::Enter]);
        assert_eq!(
            sent_request(action),
            Some(MergeRequest::Auto(MergeMethod::Rebase))
        );

        let mut admin = mergeable("BLOCKED");
        admin.merge.can_bypass = true;
        let (app, action) = merge_through_menu(admin.clone(), &[KeyCode::Enter]);
        assert!(action.is_none(), "blocked, and bypass not ticked");
        assert!(app.merge_menu.as_ref().unwrap().note.is_some());
        let tick_then_merge = [
            KeyCode::Char('j'),
            KeyCode::Enter,
            KeyCode::Char('k'),
            KeyCode::Enter,
        ];
        let (app, action) = merge_through_menu(admin, &tick_then_merge);
        assert_eq!(
            sent_request(action),
            Some(MergeRequest::Now(MergeMethod::Rebase))
        );
        assert!(app.merge_menu.is_none());

        let mut on = mergeable("BLOCKED");
        on.merge.auto_merge = Some(MergeMethod::Squash);
        on.merge.can_disable_auto_merge = true;
        let (_, action) = merge_through_menu(on, &[KeyCode::Enter]);
        assert_eq!(sent_request(action), Some(MergeRequest::DisableAuto));
    }

    #[test]
    fn m_on_a_pr_that_cannot_merge_says_why() {
        let mut merged = mergeable("CLEAN");
        merged.lifecycle = Lifecycle::Merged { by: None, at: None };
        let (app, action) = merge_through_menu(merged, &[]);
        assert!(action.is_none() && app.merge_menu.is_none());
        assert_eq!(app.error.as_deref(), Some("the PR is merged already"));
    }

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
            matches!(action, Some(Action::SetViewed { ref paths, viewed: true, ref pull_request_id, .. })
            if paths == &["a.md"] && pull_request_id == "PR_node")
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
        app.set_viewed_result(
            &pr,
            &["a.md".to_string()],
            true,
            Err("forbidden".to_string()),
        );
        assert_eq!(
            view(&app).loaded_files().unwrap()[0].viewed,
            ViewedState::Unviewed
        );
        assert!(app.error.as_deref().unwrap().contains("forbidden"));
    }

    #[test]
    fn a_failed_sync_restores_what_github_last_had_after_quick_presses() {
        let state = |app: &App| view(app).loaded_files().unwrap()[0].viewed;
        let paths = ["a.md".to_string()];
        let press_v = |app: &mut App| {
            view_mut(app).file_state.select(Some(0));
            app.handle_key(key(KeyCode::Char('v')));
        };
        let (mut app, pr) = files_tab(vec![file("a.md", ViewedState::Dismissed)]);
        press_v(&mut app);
        app.set_viewed_result(&pr, &paths, true, Err("offline".to_string()));
        assert_eq!(
            state(&app),
            ViewedState::Dismissed,
            "not the opposite of Viewed"
        );

        let (mut app, pr) = files_tab(vec![file("a.md", ViewedState::Unviewed)]);
        press_v(&mut app);
        press_v(&mut app);
        app.set_viewed_result(&pr, &paths, true, Err("offline".to_string()));
        assert_eq!(
            state(&app),
            ViewedState::Unviewed,
            "the second write is still out"
        );
        app.set_viewed_result(&pr, &paths, false, Err("offline".to_string()));
        assert_eq!(state(&app), ViewedState::Unviewed, "neither reached GitHub");

        let (mut app, pr) = files_tab(vec![file("a.md", ViewedState::Unviewed)]);
        press_v(&mut app);
        press_v(&mut app);
        app.set_viewed_result(&pr, &paths, true, Ok(()));
        app.set_viewed_result(&pr, &paths, false, Err("offline".to_string()));
        assert_eq!(
            state(&app),
            ViewedState::Viewed,
            "GitHub kept the first write"
        );
    }

    /// The files tab as a folder tree, with the cursor on the `src` folder row.
    fn on_src_folder(files: Vec<ChangedFile>) -> (App, PrRef) {
        let (mut app, pr) = files_tab(files);
        view_mut(&mut app).tree_view = true;
        let rows = view(&app).file_rows();
        let folder = rows
            .iter()
            .position(|row| matches!(row, FileRow::Dir { path, .. } if path == "src"))
            .expect("a src folder row");
        view_mut(&mut app).file_state.select(Some(folder));
        (app, pr)
    }

    fn viewed_states(app: &App) -> Vec<ViewedState> {
        let files = view(app).loaded_files().unwrap();
        files.iter().map(|file| file.viewed).collect()
    }

    #[test]
    fn v_on_a_folder_marks_every_unviewed_file_under_it() {
        let (mut app, _) = on_src_folder(vec![
            file("src/a.rs", ViewedState::Unviewed),
            file("src/deep/b.rs", ViewedState::Viewed),
            file("src/deep/c.rs", ViewedState::Unviewed),
            file("srcx/d.rs", ViewedState::Unviewed),
            file("e.md", ViewedState::Unviewed),
        ]);
        let action = app.handle_key(key(KeyCode::Char('v')));
        assert!(
            matches!(action, Some(Action::SetViewed { ref paths, viewed: true, .. })
            if paths == &["src/a.rs", "src/deep/c.rs"]),
            "only the unviewed files under src/ are sent: {action:?}"
        );
        use ViewedState::{Unviewed, Viewed};
        assert_eq!(
            viewed_states(&app),
            [Viewed, Viewed, Viewed, Unviewed, Unviewed]
        );
    }

    #[test]
    fn v_on_a_fully_viewed_folder_unmarks_it_and_a_failure_restores_it() {
        let (mut app, pr) = on_src_folder(vec![
            file("src/a.rs", ViewedState::Viewed),
            file("src/b.rs", ViewedState::Viewed),
            file("c.md", ViewedState::Viewed),
        ]);
        let action = app.handle_key(key(KeyCode::Char('v')));
        let Some(Action::SetViewed {
            paths,
            viewed: false,
            ..
        }) = action
        else {
            panic!("expected an unmark of the folder, got {action:?}");
        };
        use ViewedState::{Unviewed, Viewed};
        assert_eq!(viewed_states(&app), [Unviewed, Unviewed, Viewed]);
        app.set_viewed_result(&pr, &paths, false, Err("forbidden".to_string()));
        assert_eq!(viewed_states(&app), [Viewed, Viewed, Viewed]);
        assert!(app.error.as_deref().unwrap().contains("2 files"));
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

    #[test]
    fn the_gallery_opens_at_the_image_row_and_pages_through_the_pr() {
        let image = |url: &str| crate::markdown::Visual::Image(url.to_string());
        let (mut app, _) = files_tab(Vec::new());
        let view = view_mut(&mut app);
        view.tab = Tab::Overview;
        let mut rows: Vec<TextRow> = (0..3)
            .map(|_| TextRow::plain(ratatui::text::Line::from("text")))
            .collect();
        rows[1].image = Some(image("https://a/2.png"));
        view.overview = Some(OverviewLayout {
            width: 60,
            rows,
            images: vec![image("https://a/1.png"), image("https://a/2.png")],
        });
        view.cursor = 1;

        let load = app.handle_key(key(KeyCode::Enter));
        assert!(matches!(load, Some(Action::LoadImage(item)) if item == image("https://a/2.png")));
        assert_eq!(app.gallery.as_ref().unwrap().at, 1);
        assert!(
            app.handle_key(key(KeyCode::Char('l'))).is_none(),
            "no image after the last"
        );
        let back = app.handle_key(key(KeyCode::Char('h')));
        assert!(matches!(back, Some(Action::LoadImage(item)) if item == image("https://a/1.png")));
        let open = app.handle_key(key(KeyCode::Char('o')));
        assert!(matches!(open, Some(Action::OpenBrowser(url)) if url == "https://a/1.png"));
        app.handle_key(key(KeyCode::Esc));
        assert!(app.gallery.is_none());
        assert!(
            matches!(app.screen, Screen::Detail(_)),
            "esc leaves the PR open"
        );

        let view = view_mut(&mut app);
        view.cursor = 0;
        view.scroll = 0;
        view.page = 3;
        let on_screen = app.handle_key(key(KeyCode::Char('i')));
        assert!(
            matches!(on_screen, Some(Action::LoadImage(item)) if item == image("https://a/2.png")),
            "i off an image row opens at the first image on screen"
        );
        app.handle_key(key(KeyCode::Esc));

        let view = view_mut(&mut app);
        view.scroll = 2;
        view.page = 1;
        let first = app.handle_key(key(KeyCode::Char('i')));
        assert!(
            matches!(first, Some(Action::LoadImage(item)) if item == image("https://a/1.png")),
            "with no image on screen, i opens at the PR's first image"
        );
        app.handle_key(key(KeyCode::Esc));

        let view = view_mut(&mut app);
        view.scroll = 0;
        view.clicks = vec![ClickTarget {
            area: ratatui::layout::Rect::new(0, 0, 60, 3),
            click: Click::Page { first: 0 },
        }];
        let clicked = app.handle_click(5, 1);
        assert!(
            matches!(clicked, Some(Action::LoadImage(item)) if item == image("https://a/2.png")),
            "a click on an image row opens the gallery at it"
        );
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
    fn n_goes_on_from_the_diff_through_the_rest_of_the_repo_and_back() {
        let (mut app, pr) = files_tab(vec![
            file("a.md", ViewedState::Unviewed),
            file("b.md", ViewedState::Unviewed),
        ]);
        ready_repo(&mut app, &pr);
        let job = app.take_diff_request().unwrap();
        app.set_diff(&pr, job.key, crate::highlight::to_text(b"needle\n", None));
        app.handle_key(key(KeyCode::Char('/')));
        type_text(&mut app, "Needle");
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(
            view(&app).find.as_deref(),
            Some("Needle"),
            "the find keeps its case"
        );
        app.set_find_files(&pr, "Needle", Ok(vec![("a.md".to_string(), 1)]));
        let hit = |path: &str, line| GrepHit {
            path: path.to_string(),
            line,
            text: "needle".to_string(),
        };
        let found = vec![hit("a.md", 1), hit("x.py", 3), hit("y.py", 40)];
        app.set_find_repo(&pr, "Needle", Ok(found));
        let repo = view(&app).find_repo.clone().unwrap().unwrap();
        assert_eq!(
            repo.len(),
            2,
            "a.md is changed, so n reaches it in the diff"
        );
        let shown = |action: Option<Action>| match action {
            Some(Action::ShowFile {
                path, line, commit, ..
            }) => (path, line, commit),
            other => panic!("expected a file, got {other:?}"),
        };
        let first = shown(app.handle_key(key(KeyCode::Char('n'))));
        assert_eq!(first, ("x.py".to_string(), 3, "head999".to_string()));
        assert_eq!(view(&app).repo_hit, Some(0));
        assert_eq!(shown(app.handle_key(key(KeyCode::Char('n')))).0, "y.py");
        assert_eq!(view(&app).overlays.len(), 1, "one file view at a time");
        assert!(app.handle_key(key(KeyCode::Char('n'))).is_none());
        assert!(
            view(&app).overlays.is_empty(),
            "past the last: back to the diff"
        );
        assert_eq!(view(&app).selected_file(), Some(0));
        assert_eq!(
            view(&app).current_find_hit().map(|hit| hit.ordinal),
            Some(1)
        );
        assert_eq!(shown(app.handle_key(key(KeyCode::Char('N')))).0, "y.py");
        app.handle_key(key(KeyCode::Esc));
        assert!(view(&app).overlays.is_empty());
        assert_eq!(view(&app).repo_hit, None);
    }

    #[test]
    fn a_new_find_ignores_the_repo_matches_of_the_old_one() {
        let (mut app, pr) = files_tab(vec![file("a.md", ViewedState::Unviewed)]);
        ready_repo(&mut app, &pr);
        app.handle_key(key(KeyCode::Char('/')));
        type_text(&mut app, "new");
        app.handle_key(key(KeyCode::Enter));
        app.set_find_repo(&pr, "old", Ok(Vec::new()));
        assert!(view(&app).find_repo.is_none());
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
