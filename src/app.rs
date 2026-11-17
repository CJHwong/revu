//! Screen state and key handling. It does no I/O: keys that need I/O return an `Action`.

use chrono::{DateTime, Utc};
use crossterm::event::KeyCode;
use ratatui::widgets::ListState;

use crate::model::{Group, PrDetail, PrSummary};

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
    FetchDetail(PrSummary),
    OpenBrowser(String),
    RunDiff(PrSummary),
}

pub struct DetailView {
    pub pr: PrSummary,
    /// `None` while the request runs.
    pub detail: Option<Result<PrDetail, String>>,
    pub scroll: u16,
}

pub enum Screen {
    Inbox,
    Detail(Box<DetailView>),
}

pub struct App {
    pub groups: Vec<Group>,
    pub list_state: ListState,
    pub screen: Screen,
    pub loading: bool,
    pub error: Option<String>,
    pub fetched_at: Option<DateTime<Utc>>,
}

impl App {
    pub fn new() -> Self {
        Self {
            groups: Vec::new(),
            list_state: ListState::default().with_selected(Some(0)),
            screen: Screen::Inbox,
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

    /// Ignores a response for a PR the user already left.
    pub fn set_detail(&mut self, repo: &str, number: u64, result: Result<PrDetail, String>) {
        let Screen::Detail(view) = &mut self.screen else {
            return;
        };
        if view.pr.repo == repo && view.pr.number == number {
            view.detail = Some(result);
        }
    }

    pub fn handle_key(&mut self, key: KeyCode) -> Option<Action> {
        match self.screen {
            Screen::Inbox => self.inbox_key(key),
            Screen::Detail(_) => self.detail_key(key),
        }
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
            KeyCode::Char('d') => return self.selected_pr().cloned().map(Action::RunDiff),
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
                self.screen = Screen::Inbox
            }
            KeyCode::Char('j') | KeyCode::Down => view.scroll = view.scroll.saturating_add(1),
            KeyCode::Char('k') | KeyCode::Up => view.scroll = view.scroll.saturating_sub(1),
            KeyCode::Char(' ') | KeyCode::PageDown => view.scroll = view.scroll.saturating_add(20),
            KeyCode::Char('b') | KeyCode::PageUp => view.scroll = view.scroll.saturating_sub(20),
            KeyCode::Char('g') | KeyCode::Home => view.scroll = 0,
            KeyCode::Char('o') => return Some(Action::OpenBrowser(view.pr.url.clone())),
            KeyCode::Char('d') => return Some(Action::RunDiff(view.pr.clone())),
            KeyCode::Char('r') => {
                view.detail = None;
                return Some(Action::FetchDetail(view.pr.clone()));
            }
            _ => {}
        }
        None
    }

    fn open_selected(&mut self) -> Option<Action> {
        if let Some(Row::Header(_)) = self.selected_row() {
            self.toggle_selected_group();
            return None;
        }
        let pr = self.selected_pr()?.clone();
        self.screen = Screen::Detail(Box::new(DetailView {
            pr: pr.clone(),
            detail: None,
            scroll: 0,
        }));
        Some(Action::FetchDetail(pr))
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
        app.handle_key(KeyCode::Char('k'));
        assert_eq!(app.list_state.selected(), Some(0));
        app.handle_key(KeyCode::Char('j'));
        app.handle_key(KeyCode::Char('j'));
        assert_eq!(app.list_state.selected(), Some(1));
    }

    #[test]
    fn enter_on_a_header_folds_the_group() {
        let mut app = app_with(vec![group("A", &[1], false)]);
        assert!(app.handle_key(KeyCode::Enter).is_none());
        assert!(app.groups[0].collapsed);
    }

    #[test]
    fn folding_from_a_pr_row_selects_its_header() {
        let mut app = app_with(vec![group("A", &[1], false), group("B", &[2, 3], false)]);
        app.list_state.select(Some(4)); // B's second PR
        app.handle_key(KeyCode::Char(' '));
        assert!(app.groups[1].collapsed);
        assert_eq!(app.selected_row(), Some(Row::Header(1)));
    }

    #[test]
    fn enter_on_a_pr_opens_detail_and_requests_it() {
        let mut app = app_with(vec![group("A", &[7], false)]);
        app.list_state.select(Some(1));
        let action = app.handle_key(KeyCode::Enter);
        assert!(matches!(action, Some(Action::FetchDetail(ref pr)) if pr.number == 7));
        assert!(matches!(app.screen, Screen::Detail(_)));
        app.handle_key(KeyCode::Esc);
        assert!(matches!(app.screen, Screen::Inbox));
    }

    #[test]
    fn a_late_detail_for_another_pr_is_ignored() {
        let mut app = app_with(vec![group("A", &[7], false)]);
        app.list_state.select(Some(1));
        app.handle_key(KeyCode::Enter);
        app.set_detail("acme/app", 8, Err("boom".to_string()));
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
}
