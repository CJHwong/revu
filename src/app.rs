//! Screen state and key handling. It does no I/O: keys that need I/O return an `Action`.

use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::ListState;

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
    FetchFiles(PrRef),
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
        }
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

    /// Switches tab, and asks for the file list the first time the Files tab shows.
    fn show_tab(&mut self, tab: Tab) -> Option<Action> {
        self.tab = tab;
        if tab != Tab::Files || self.files_requested {
            return None;
        }
        self.files_requested = true;
        Some(Action::FetchFiles(self.pr.clone()))
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
    }
}

pub enum Screen {
    Inbox,
    Detail(Box<DetailView>),
}

/// The "open a PR link" box.
#[derive(Debug, Default)]
pub struct Prompt {
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
        }
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

    /// A paste anywhere opens the link box with the pasted text.
    pub fn handle_paste(&mut self, text: &str) {
        let prompt = self.prompt.get_or_insert_with(Prompt::default);
        prompt.text.push_str(text.trim());
        prompt.error = None;
    }

    fn prompt_key(&mut self, key: KeyCode) -> Option<Action> {
        let prompt = self.prompt.as_mut()?;
        match key {
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
        let prompt = self.prompt.as_mut()?;
        let Some(pr) = parse_pr_ref(&prompt.text) else {
            prompt.error = Some("not a pull request link or owner/repo#number".to_string());
            return None;
        };
        self.prompt = None;
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
        let Screen::Detail(view) = &mut self.screen else {
            return None;
        };
        match key {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('h') | KeyCode::Left => {
                self.screen = Screen::Inbox;
                self.error = None;
            }
            KeyCode::Tab => return view.show_tab(view.tab.shifted(1)),
            KeyCode::BackTab => return view.show_tab(view.tab.shifted(-1)),
            KeyCode::Char(digit @ '1'..='9') => {
                let index = digit as usize - '1' as usize;
                return view.show_tab(Tab::ALL.get(index).copied().unwrap_or(view.tab));
            }
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

    /// Starts the PR over, keeping the tab, and fetches its files again if that tab shows.
    fn reload(view: &mut DetailView) -> Action {
        let tab = view.tab;
        *view = DetailView::new(view.pr.clone());
        // FetchDetail covers the overview; the Files tab asks again when shown.
        view.tab = tab;
        if tab == Tab::Files {
            view.files_requested = false;
        }
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
        let first = app.handle_key(key(KeyCode::Char('4')));
        assert!(matches!(first, Some(Action::FetchFiles(_))));
        app.handle_key(key(KeyCode::Char('1')));
        assert!(app.handle_key(key(KeyCode::Char('4'))).is_none());
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
}
