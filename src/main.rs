mod app;
mod config;
mod editor;
mod file_tree;
mod find;
mod github;
mod highlight;
mod line_diff;
mod markdown;
mod model;
mod overview;
mod repo;
mod theme;
mod ui;

use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseButton,
    MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::supports_keyboard_enhancement;
use highlight::Syntax;
use ratatui::DefaultTerminal;
use ratatui::text::Text;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use app::{Action, App, DiffJob, DiffKey, PromptKind, RepoJob, RepoState};
use config::Config;
use model::{ChangedFile, Group, PrActivity, PrDetail, PrRef};
use repo::{CloneLookup, LocalRepo};

/// Results from the fetch threads. Errors are strings because they only get displayed.
enum Message {
    /// gh is missing or logged out; found after an inbox fetch failed.
    GhProblem(github::GhProblem),
    /// The repo's labels, for the label menu.
    Labels {
        pr: PrRef,
        result: Result<Vec<github::RepoLabel>, String>,
    },
    /// One file's plain diff, for comment mode.
    LineDiff {
        pr: PrRef,
        path: String,
        result: Result<String, String>,
    },
    /// A write to GitHub finished: a review, a line comment, or a discard.
    Written {
        pr: PrRef,
        result: Result<(), String>,
    },
    Group {
        generation: u64,
        index: usize,
        result: Result<Group, String>,
        fetched_at: DateTime<Utc>,
    },
    Activity {
        pr: PrRef,
        result: Result<Box<PrActivity>, String>,
    },
    /// The code under each review thread, by thread ID, after the activity.
    Hunks {
        pr: PrRef,
        hunks: std::collections::HashMap<String, Vec<model::HunkLine>>,
    },
    Detail {
        pr: PrRef,
        result: Result<Box<PrDetail>, String>,
    },
    Files {
        pr: PrRef,
        result: Result<Vec<ChangedFile>, String>,
    },
    /// A page of the file list arrived: the files so far and the PR's total.
    FilesProgress {
        pr: PrRef,
        loaded: usize,
        total: usize,
    },
    Viewed {
        pr: PrRef,
        paths: Vec<String>,
        viewed: bool,
        result: Result<(), String>,
    },
    Repo {
        pr: PrRef,
        result: Result<LocalRepo, String>,
    },
    Diff {
        pr: PrRef,
        key: DiffKey,
        result: Result<Text<'static>, String>,
    },
    Grep {
        pr: PrRef,
        query: String,
        result: Result<Vec<repo::GrepHit>, String>,
    },
    FindFiles {
        pr: PrRef,
        query: String,
        result: Result<Vec<(String, usize)>, String>,
    },
    FindWords {
        pr: PrRef,
        result: Result<Vec<String>, String>,
    },
    File {
        pr: PrRef,
        path: String,
        result: Result<Text<'static>, String>,
    },
}

enum Input {
    Key(KeyEvent),
    Paste(String),
    /// A mouse wheel tick: the pointer's column, and +1 down or -1 up.
    Scroll(u16, isize),
    /// A left click at a column and row.
    Click(u16, u16),
}

struct Session {
    config: Config,
    config_path: PathBuf,
    sender: Sender<Message>,
    receiver: Receiver<Message>,
    /// Whether the terminal reports modifiers such as Cmd (kitty keyboard protocol).
    enhanced_keys: bool,
    /// The PR waiting on the user's answer to "clone it?".
    pending_clone: Option<RepoJob>,
}

fn main() -> Result<()> {
    let config_path = config::default_path()?;
    config::move_old_dir(&config_path)?;
    let config = config::load_or_init(&config_path)?;
    let (sender, receiver) = mpsc::channel();
    let mut session = Session {
        config,
        config_path,
        sender,
        receiver,
        enhanced_keys: false,
        pending_clone: None,
    };
    let mut app = App::new();
    app.renderer_names = renderer_names(&session.config);
    check_renderers(&session.config, &mut app);
    app.workspace_dirs = session.config.workspace_dirs.clone();
    app.updated_within_days = session.config.updated_within_days;
    match config::load_prefs(&config::prefs_path(&session.config_path)) {
        Ok(prefs) => app.prefs = prefs,
        Err(error) => app.set_inbox_error(format!("view toggles not loaded: {error:#}")),
    }
    app.theme_name = theme::name_of(&session.config.theme).unwrap_or(app.theme_name);
    start_inbox_fetch(&session, &mut app);

    let mut terminal = ratatui::init();
    // Query once, in raw mode, before any extra terminal modes are on.
    session.enhanced_keys = supports_keyboard_enhancement().unwrap_or(false);
    enable_input_modes(session.enhanced_keys)?;
    let result = run(&mut terminal, &mut app, &mut session);
    let restored = disable_input_modes(session.enhanced_keys);
    ratatui::restore();
    result.and(restored)
}

/// Bracketed paste delivers a pasted link as one event. The keyboard flags make
/// terminals that support them report Cmd as the Super modifier.
fn enable_input_modes(enhanced_keys: bool) -> Result<()> {
    let mut stdout = std::io::stdout();
    // Without mouse capture the terminal sends the wheel as arrow keys, which move the list.
    execute!(stdout, EnableBracketedPaste, EnableMouseCapture)?;
    if enhanced_keys {
        let flags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES;
        execute!(stdout, PushKeyboardEnhancementFlags(flags))?;
    }
    Ok(())
}

fn disable_input_modes(enhanced_keys: bool) -> Result<()> {
    let mut stdout = std::io::stdout();
    if enhanced_keys {
        execute!(stdout, PopKeyboardEnhancementFlags)?;
    }
    execute!(stdout, DisableBracketedPaste, DisableMouseCapture)?;
    Ok(())
}

fn run(terminal: &mut DefaultTerminal, app: &mut App, session: &mut Session) -> Result<()> {
    loop {
        app.rate_limit = github::rate_limit();
        terminal.draw(|frame| ui::render(frame, app, &session.config.theme, Utc::now()))?;
        apply_messages(app, &session.receiver);
        app.settle_find();
        dispatch_requests(app, session);
        let mut input = next_input(Duration::from_millis(100))?;
        // Every input already waiting is handled before the next draw, so a long wheel
        // fling costs a few frames, not one per event. The time cap keeps the screen
        // moving while events still stream in.
        let started = Instant::now();
        while let Some(current) = input {
            if !handle_input(current, terminal, app, session)? {
                return Ok(());
            }
            if started.elapsed() > Duration::from_millis(30) {
                break;
            }
            input = next_input(Duration::ZERO)?;
        }
        save_prefs(app, session);
    }
}

/// Saves the open PR's view toggles when they changed, so the next start keeps them.
fn save_prefs(app: &mut App, session: &Session) {
    if !app.sync_prefs() {
        return;
    }
    let path = config::prefs_path(&session.config_path);
    if let Err(error) = config::save_prefs(&path, &app.prefs) {
        app.error = Some(format!("view toggles not saved: {error:#}"));
    }
}

/// Does what one input asks. Returns false when the app should quit.
fn handle_input(
    input: Input,
    terminal: &mut DefaultTerminal,
    app: &mut App,
    session: &mut Session,
) -> Result<bool> {
    let action = match input {
        Input::Paste(text) => {
            app.handle_paste(&text);
            None
        }
        Input::Scroll(column, delta) => {
            app.handle_scroll(column, delta);
            None
        }
        Input::Click(column, row) => app.handle_click(column, row),
        // In the text box ctrl+c copies; everywhere else it quits.
        Input::Key(key)
            if key.code == KeyCode::Char('c')
                && key.modifiers.contains(KeyModifiers::CONTROL)
                && app.composer.is_none() =>
        {
            return Ok(false);
        }
        Input::Key(key) => app.handle_key(key),
    };
    match action {
        Some(action) => {
            app.saving = action.save_note().or(app.saving);
            perform(action, terminal, app, session)
        }
        None => Ok(true),
    }
}

/// Waits up to `wait` for an input; a short wait lets fetch results redraw when no key
/// arrives.
/// Events the app does not use, such as a mouse move, are skipped; after one, the
/// wait drops to zero, so a resize still redraws at once.
fn next_input(mut wait: Duration) -> Result<Option<Input>> {
    while event::poll(wait)? {
        if let Some(input) = to_input(event::read()?) {
            return Ok(Some(input));
        }
        wait = Duration::ZERO;
    }
    Ok(None)
}

fn to_input(event: Event) -> Option<Input> {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => Some(Input::Key(key)),
        Event::Paste(text) => Some(Input::Paste(text)),
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollDown => Some(Input::Scroll(mouse.column, 1)),
            MouseEventKind::ScrollUp => Some(Input::Scroll(mouse.column, -1)),
            MouseEventKind::Down(MouseButton::Left) => Some(Input::Click(mouse.column, mouse.row)),
            _ => None,
        },
        _ => None,
    }
}

fn apply_messages(app: &mut App, receiver: &Receiver<Message>) {
    for message in receiver.try_iter() {
        match message {
            Message::Group {
                generation,
                index,
                result,
                fetched_at,
            } => app.set_group(generation, index, result, fetched_at),
            Message::GhProblem(problem) => {
                app.gh_problem = Some(problem);
                app.gh_checked_at = Some(Utc::now());
            }
            Message::Activity { pr, result } => {
                app.set_activity(&pr, result.map(|activity| *activity))
            }
            Message::Hunks { pr, hunks } => app.set_hunks(&pr, hunks),
            Message::Detail { pr, result } => app.set_detail(&pr, result.map(|detail| *detail)),
            Message::Files { pr, result } => app.set_files(&pr, result),
            Message::Written { pr, result } => app.set_written(&pr, result),
            Message::LineDiff { pr, path, result } => app.set_line_rows(&pr, &path, result),
            Message::Labels { pr, result } => app.set_repo_labels(&pr, result),
            Message::FilesProgress { pr, loaded, total } => {
                app.set_files_progress(&pr, loaded, total)
            }
            Message::Viewed {
                pr,
                paths,
                viewed,
                result,
            } => app.set_viewed_result(&pr, &paths, viewed, result),
            Message::Repo { pr, result } => app.set_repo(&pr, result),
            Message::Diff { pr, key, result } => app.set_diff(&pr, key, result),
            Message::Grep { pr, query, result } => app.set_grep(&pr, &query, result),
            Message::FindFiles { pr, query, result } => app.set_find_files(&pr, &query, result),
            // Without the words the prompt only loses its completion.
            Message::FindWords { pr, result } => {
                app.set_find_words(&pr, result.unwrap_or_default())
            }
            Message::File { pr, path, result } => app.set_file(&pr, &path, result),
        }
    }
}

/// Returns false when the app should quit.
fn perform(
    action: Action,
    terminal: &mut DefaultTerminal,
    app: &mut App,
    session: &mut Session,
) -> Result<bool> {
    match action {
        Action::Quit => return Ok(false),
        Action::Refresh => refresh(app, session),
        Action::FetchDetail(pr) => {
            start_activity_fetch(session, pr.clone());
            start_detail_fetch(session, pr);
        }
        Action::OpenBrowser(url) => open_browser(app, &url),
        Action::RunDiff(pr) => run_diff(terminal, app, session, &pr)?,
        Action::Grep {
            pr,
            dir,
            commit,
            query,
        } => {
            let sender = session.sender.clone();
            thread::spawn(move || {
                let result =
                    repo::grep(&dir, &commit, &query).map_err(|error| format!("{error:#}"));
                let _ = sender.send(Message::Grep { pr, query, result });
            });
        }
        Action::FindWords {
            pr,
            dir,
            base,
            head,
        } => {
            let sender = session.sender.clone();
            thread::spawn(move || {
                let result =
                    repo::diff_words(&dir, &base, &head).map_err(|error| format!("{error:#}"));
                let _ = sender.send(Message::FindWords { pr, result });
            });
        }
        Action::FindFiles {
            pr,
            dir,
            base,
            head,
            query,
        } => {
            let sender = session.sender.clone();
            thread::spawn(move || {
                let result = repo::files_with_text(&dir, &base, &head, &query)
                    .map_err(|error| format!("{error:#}"));
                let _ = sender.send(Message::FindFiles { pr, query, result });
            });
        }
        Action::ShowFile {
            pr,
            dir,
            commit,
            path,
            line,
        } => {
            let sender = session.sender.clone();
            let bat_theme = session.config.theme.bat_theme;
            thread::spawn(move || {
                let result = repo::highlight_file(&dir, &commit, &path, line, bat_theme)
                    .map_err(|error| format!("{error:#}"))
                    .and_then(|bytes| highlight::to_text(&bytes, None));
                let _ = sender.send(Message::File { pr, path, result });
            });
        }
        Action::ShowWholeDiff(job) => {
            let Some(command) = renderer_command(session, &job) else {
                return Ok(true);
            };
            let syntax = renderer_syntax(session, &job);
            let sender = session.sender.clone();
            thread::spawn(move || {
                let result = render_diff(&job, &command, true, syntax);
                let path = job.paths.last().cloned().unwrap_or_default();
                let _ = sender.send(Message::File {
                    pr: job.pr,
                    path,
                    result,
                });
            });
        }
        Action::SetWorkspace(text) => set_workspace(app, session, text),
        Action::SetTheme(name) => set_theme(app, session, name),
        Action::SetUpdatedWithin(days) => set_updated_within(app, session, days),
        Action::Copy(text) => copy_to_clipboard(app, &text),
        Action::SubmitReview {
            pr,
            pull_request_id,
            pending_review,
            verdict,
            body,
        } => start_write(session, pr, move || {
            github::submit_review(&pull_request_id, pending_review.as_deref(), verdict, &body)
        }),
        Action::AddComment {
            pr,
            pull_request_id,
            pending_review,
            target,
            body,
        } => start_write(session, pr, move || {
            let comment = github::NewComment {
                path: &target.path,
                side: target.side,
                line: target.line,
                start_line: target.start_line,
                body: &body,
            };
            github::add_line_comment(&pull_request_id, pending_review.as_deref(), &comment)
                .map(|_| ())
        }),
        Action::LineDiff(job) => {
            let sender = session.sender.clone();
            thread::spawn(move || {
                let path = job.paths.last().cloned().unwrap_or_default();
                let result = repo::file_diff(&job.dir, &job.base, &job.head, &job.paths)
                    .map_err(|error| format!("{error:#}"));
                let _ = sender.send(Message::LineDiff {
                    pr: job.pr,
                    path,
                    result,
                });
            });
        }
        Action::FetchLabels { pr } => {
            let sender = session.sender.clone();
            thread::spawn(move || {
                let result = github::repo_labels(&pr.repo).map_err(|error| format!("{error:#}"));
                let _ = sender.send(Message::Labels { pr, result });
            });
        }
        Action::SetLabels {
            pr,
            pull_request_id,
            add,
            remove,
        } => start_write(session, pr, move || {
            github::set_labels(&pull_request_id, &add, &remove)
        }),
        Action::DiscardReview { pr, pending_review } => {
            start_write(session, pr, move || github::discard_review(&pending_review))
        }
        Action::CloneRepo { pr, dir } => match session.pending_clone.take() {
            Some(job) if job.pr == pr => {
                app.set_repo_state(RepoState::Busy(format!(
                    "cloning {} into {}",
                    pr.repo,
                    dir.display()
                )));
                start_repo_fetch(session, job, dir, true);
            }
            _ => app.set_repo(&pr, Err("the clone request expired; press r".to_string())),
        },
        Action::SetViewed {
            pr,
            pull_request_id,
            paths,
            viewed,
        } => start_viewed_sync(session, pr, pull_request_id, paths, viewed),
    }
    Ok(true)
}

/// Leaves renderers whose program is not on PATH out of the m cycle, and words the
/// warning the diff footer shows once.
fn check_renderers(config: &Config, app: &mut App) {
    app.missing_renderers.clear();
    // Every renderer runs git, so without it the warning is about git alone.
    if !repo::on_path("git") {
        app.notice = Some(format!(
            "git is not installed, so diffs, find and search cannot run. To add it: {}",
            repo::GIT_INSTALL
        ));
        return;
    }
    let mut missing = Vec::new();
    let mut installs = Vec::new();
    for (index, renderer) in config.diff_renderers.iter().enumerate() {
        let Some(program) = &renderer.requires else {
            continue;
        };
        if repo::on_path(program) {
            continue;
        }
        app.missing_renderers.insert(index);
        missing.push(renderer.name.clone());
        installs.extend(renderer.install.clone());
    }
    app.notice = renderer_warning(&missing, &installs);
}

/// "difftastic is not installed, so m skips it. To add it: brew install difftastic"
fn renderer_warning(missing: &[String], installs: &[String]) -> Option<String> {
    let (names, verb, them) = match missing {
        [] => return None,
        [one] => (one.clone(), "is", "it"),
        [rest @ .., last] => (format!("{} and {last}", rest.join(", ")), "are", "them"),
    };
    let mut warning = format!("{names} {verb} not installed, so m skips {them}");
    if !installs.is_empty() {
        warning.push_str(&format!(". To add {them}: {}", installs.join("; ")));
    }
    Some(warning)
}

fn renderer_names(config: &Config) -> Vec<String> {
    config
        .diff_renderers
        .iter()
        .map(|renderer| renderer.name.clone())
        .collect()
}

/// Starts whatever the screen now needs: a file list, a local clone, or a diff.
fn dispatch_requests(app: &mut App, session: &mut Session) {
    if let Some(pr) = app.reload_after_write.take() {
        start_activity_fetch(session, pr.clone());
        start_detail_fetch(session, pr);
    }
    if let Some(pr) = app.take_files_request() {
        start_files_fetch(session, pr);
    }
    if let Some(job) = app.take_repo_request() {
        prepare_repo(app, session, job);
    }
    if let Some(job) = app.take_diff_request() {
        start_diff(session, job);
    }
}

/// Finds the clone in the workspace, asking for the workspace or for a clone first when needed.
fn prepare_repo(app: &mut App, session: &mut Session, job: RepoJob) {
    if session.config.workspace_dirs.is_empty() {
        app.set_repo_state(RepoState::Busy(
            "waiting for the workspace folder".to_string(),
        ));
        app.ask(PromptKind::Workspace, "~/Workspace");
        return;
    }
    let workspaces: Vec<_> = session
        .config
        .workspace_dirs
        .iter()
        .map(|folder| repo::expand_home(folder))
        .collect();
    match repo::find_clone(&workspaces, &job.pr.repo) {
        Ok(CloneLookup::Found(dir)) => {
            app.set_repo_state(RepoState::Busy(format!(
                "fetching the PR into {}",
                dir.display()
            )));
            start_repo_fetch(session, job, dir, false);
        }
        Ok(CloneLookup::Missing(dir)) => {
            app.set_repo_state(RepoState::Busy("waiting for your answer".to_string()));
            app.ask(
                PromptKind::ConfirmClone {
                    pr: job.pr.clone(),
                    dir,
                },
                "",
            );
            session.pending_clone = Some(job);
        }
        Ok(CloneLookup::Conflict { dir, origin }) => {
            let message = format!(
                "{} is a clone of {origin}, not {}",
                dir.display(),
                job.pr.repo
            );
            app.set_repo(&job.pr, Err(message));
        }
        Err(error) => app.set_repo(&job.pr, Err(format!("{error:#}"))),
    }
}

/// Applies the theme at once, redraws the diffs in its colors, and saves it.
fn set_theme(app: &mut App, session: &mut Session, name: &'static str) {
    let theme = match theme::by_name(name) {
        Ok(theme) => theme,
        Err(error) => {
            app.error = Some(format!("{error:#}"));
            return;
        }
    };
    if theme != session.config.theme {
        session.config.theme = theme;
        app.forget_colors();
    }
    app.theme_name = name;
    if let Err(error) = config::save_theme(&session.config_path, name) {
        app.error = Some(format!("theme applied but not saved: {error:#}"));
    }
}

/// Saves the workspace folders, typed with commas between them, then lets the open PR
/// look for its clone again.
fn set_workspace(app: &mut App, session: &mut Session, text: String) {
    let folders = config::split_folders(&text);
    let not_a_folder = folders
        .iter()
        .map(|folder| repo::expand_home(folder))
        .find(|dir| !dir.is_dir());
    if let Some(dir) = not_a_folder {
        let error = format!("{} is not a folder", dir.display());
        app.set_prompt_error(PromptKind::Workspace, &text, error);
        return;
    }
    if folders.is_empty() {
        app.set_prompt_error(PromptKind::Workspace, &text, "type a folder".to_string());
        return;
    }
    let saved = config::save_setting(
        &session.config_path,
        "workspace_dir",
        &config::workspace_value(&folders),
    );
    if let Err(error) = saved {
        app.set_prompt_error(PromptKind::Workspace, &text, format!("{error:#}"));
        return;
    }
    session.config.workspace_dirs = folders.clone();
    app.workspace_dirs = folders;
    // An open PR looks for its clone again, in the new folder.
    app.set_repo_state(RepoState::Unknown);
}

/// Saves how many days back the inbox looks, then reloads the inbox with it.
fn set_updated_within(app: &mut App, session: &mut Session, days: u32) {
    let saved = config::save_setting(
        &session.config_path,
        "updated_within_days",
        &days.to_string(),
    );
    match saved {
        Ok(()) => refresh(app, session),
        Err(error) => app.error = Some(format!("not saved: {error:#}")),
    }
}

fn start_repo_fetch(session: &Session, job: RepoJob, dir: PathBuf, clone_first: bool) {
    let sender = session.sender.clone();
    thread::spawn(move || {
        let result =
            fetch_local_repo(&job, &dir, clone_first).map_err(|error| format!("{error:#}"));
        let _ = sender.send(Message::Repo { pr: job.pr, result });
    });
}

fn fetch_local_repo(job: &RepoJob, dir: &std::path::Path, clone_first: bool) -> Result<LocalRepo> {
    if clone_first {
        repo::clone(&job.pr.repo, dir)?;
    }
    repo::fetch_pr(
        dir,
        job.pr.number,
        &job.base_ref,
        &job.base_oid,
        &job.head_oid,
    )?;
    let renames = repo::renames(dir, &job.base_oid, &job.head_oid)?;
    Ok(LocalRepo {
        dir: dir.to_path_buf(),
        renames,
    })
}

/// The renderer's shell command for this job, with its values quoted.
fn renderer_command(session: &Session, job: &DiffJob) -> Option<String> {
    let renderer = session.config.diff_renderers.get(job.key.renderer)?;
    let theme = &session.config.theme;
    let paths: Vec<String> = job
        .paths
        .iter()
        .map(|path| repo::shell_quote(path))
        .collect();
    Some(repo::fill_template(
        &renderer.command,
        &[
            ("base", repo::shell_quote(&job.base)),
            ("head", repo::shell_quote(&job.head)),
            ("paths", paths.join(" ")),
            ("width", job.key.width.to_string()),
            ("syntax_theme", repo::shell_quote(theme.bat_theme)),
            (
                "added_bg",
                repo::shell_quote(&theme::color_name(theme.added_bg)),
            ),
            (
                "removed_bg",
                repo::shell_quote(&theme::color_name(theme.removed_bg)),
            ),
        ],
    ))
}

/// The syntax colors to add to this job's renderer, when its config asks for them.
fn renderer_syntax(session: &Session, job: &DiffJob) -> Option<Syntax> {
    let renderer = session.config.diff_renderers.get(job.key.renderer)?;
    let theme = &session.config.theme;
    renderer.highlight.then_some(Syntax {
        bat_theme: theme.bat_theme,
        added: theme.added_bg,
        removed: theme.removed_bg,
    })
}

/// Runs the renderer and turns its output into styled text, off the UI thread.
fn render_diff(
    job: &DiffJob,
    command: &str,
    whole_file: bool,
    syntax: Option<Syntax>,
) -> Result<Text<'static>, String> {
    let bytes = repo::run_renderer(&job.dir, command, whole_file, job.key.width)
        .map_err(|error| format!("{error:#}"))?;
    highlight::to_text(&bytes, syntax)
}

fn start_diff(session: &Session, job: DiffJob) {
    let Some(command) = renderer_command(session, &job) else {
        return;
    };
    let syntax = renderer_syntax(session, &job);
    let sender = session.sender.clone();
    thread::spawn(move || {
        let result = render_diff(&job, &command, false, syntax);
        let _ = sender.send(Message::Diff {
            pr: job.pr,
            key: job.key,
            result,
        });
    });
}

/// Reloads the config first, so edits to the groups apply without a restart.
fn refresh(app: &mut App, session: &mut Session) {
    match config::load_or_init(&session.config_path) {
        Ok(config) => {
            app.renderer_names = renderer_names(&config);
            check_renderers(&config, app);
            if config.theme != session.config.theme {
                app.forget_colors();
            }
            app.theme_name = theme::name_of(&config.theme).unwrap_or(app.theme_name);
            app.workspace_dirs = config.workspace_dirs.clone();
            app.updated_within_days = config.updated_within_days;
            session.config = config;
        }
        Err(error) => {
            app.set_inbox_error(format!("config not reloaded: {error:#}"));
            return;
        }
    }
    start_inbox_fetch(session, app);
}

/// One thread per group, so each group shows as soon as its own search returns.
fn start_inbox_fetch(session: &Session, app: &mut App) {
    let generation = app.start_inbox(&session.config.groups);
    let updated_since = session
        .config
        .updated_within_days
        .map(|days| Local::now().date_naive() - chrono::Duration::days(i64::from(days)));
    for (index, group) in session.config.groups.iter().cloned().enumerate() {
        let sender = session.sender.clone();
        thread::spawn(move || {
            let result =
                github::fetch_group(&group, updated_since).map_err(|error| format!("{error:#}"));
            if let Err(error) = &result {
                report_gh_problem(&sender, error);
            }
            // The receiver only goes away when the app exits, so a failed send needs no handling.
            let _ = sender.send(Message::Group {
                generation,
                index,
                result,
                fetched_at: Utc::now(),
            });
        });
    }
}

/// After a GitHub call fails, asks gh why. A missing gh, a lost login or no network
/// takes the whole screen, since nothing else can load until it is fixed; any other
/// failure stays where it happened.
/// A fetch that already failed on the network says so at once; asking gh would wait
/// out the same timeout again.
fn report_gh_problem(sender: &Sender<Message>, error: &str) {
    let problem = match github::network_error(error) {
        Some(error) => Some(github::GhProblem::Offline(error)),
        None => github::gh_problem(),
    };
    if let Some(problem) = problem {
        let _ = sender.send(Message::GhProblem(problem));
    }
}

fn start_activity_fetch(session: &Session, pr: PrRef) {
    let sender = session.sender.clone();
    thread::spawn(move || {
        let result = github::fetch_activity(&pr.repo, pr.number)
            .map(Box::new)
            .map_err(|error| format!("{error:#}"));
        if let Err(error) = &result {
            report_gh_problem(&sender, error);
        }
        let loaded = result.is_ok();
        let _ = sender.send(Message::Activity {
            pr: pr.clone(),
            result,
        });
        if !loaded {
            return;
        }
        // The code is extra: when it fails, the threads stay as they are, without it.
        if let Ok(hunks) = github::fetch_hunks(&pr.repo, pr.number) {
            let _ = sender.send(Message::Hunks { pr, hunks });
        }
    });
}

fn start_detail_fetch(session: &Session, pr: PrRef) {
    let sender = session.sender.clone();
    thread::spawn(move || {
        let result = github::fetch_detail(&pr.repo, pr.number)
            .map(Box::new)
            .map_err(|error| format!("{error:#}"));
        if let Err(error) = &result {
            report_gh_problem(&sender, error);
        }
        let _ = sender.send(Message::Detail { pr, result });
    });
}

fn start_files_fetch(session: &Session, pr: PrRef) {
    let sender = session.sender.clone();
    thread::spawn(move || {
        let progress = |loaded, total| {
            let _ = sender.send(Message::FilesProgress {
                pr: pr.clone(),
                loaded,
                total,
            });
        };
        let result = github::fetch_files(&pr.repo, pr.number, progress)
            .map_err(|error| format!("{error:#}"));
        if let Err(error) = &result {
            report_gh_problem(&sender, error);
        }
        let _ = sender.send(Message::Files { pr, result });
    });
}

/// Runs a write to GitHub off the UI thread; the PR reloads when it succeeds.
fn start_write(
    session: &Session,
    pr: PrRef,
    write: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
) {
    let sender = session.sender.clone();
    thread::spawn(move || {
        let result = write().map_err(|error| format!("{error:#}"));
        let _ = sender.send(Message::Written { pr, result });
    });
}

/// OSC 52 asks the terminal to set the clipboard, which also works over ssh; pbcopy
/// covers a terminal or tmux that ignores it. Neither reports a failure, so both run.
fn copy_to_clipboard(app: &mut App, text: &str) {
    let mut stdout = std::io::stdout();
    let osc = execute!(
        stdout,
        crossterm::clipboard::CopyToClipboard::to_clipboard_from(text)
    );
    let pbcopy = Command::new("pbcopy")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            if let Some(mut input) = child.stdin.take() {
                input.write_all(text.as_bytes())?;
            }
            child.wait()
        });
    if osc.is_err() && pbcopy.is_err() {
        app.error = Some("could not reach the clipboard".to_string());
    }
}

fn start_viewed_sync(
    session: &Session,
    pr: PrRef,
    pull_request_id: String,
    paths: Vec<String>,
    viewed: bool,
) {
    let sender = session.sender.clone();
    thread::spawn(move || {
        let result = github::set_viewed(&pull_request_id, &paths, viewed)
            .map_err(|error| format!("{error:#}"));
        let _ = sender.send(Message::Viewed {
            pr,
            paths,
            viewed,
            result,
        });
    });
}

fn open_browser(app: &mut App, url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    if let Err(error) = Command::new(opener).arg(url).status() {
        app.error = Some(format!("{opener} failed: {error}"));
    }
}

/// Hands the terminal to the diff command, then takes it back.
fn run_diff(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    session: &Session,
    pr: &PrRef,
) -> Result<()> {
    let command = session
        .config
        .open_diff
        .replace("{repo}", &repo::shell_quote(&pr.repo))
        .replace("{number}", &pr.number.to_string())
        .replace("{url}", &repo::shell_quote(&pr.url()));
    disable_input_modes(session.enhanced_keys)?;
    ratatui::restore();
    let status = Command::new("sh").arg("-c").arg(&command).status();
    let failure = match status {
        Ok(status) if status.success() => None,
        Ok(status) => Some(format!("diff command exited with {status}: {command}")),
        Err(error) => Some(format!("diff command did not start: {error}")),
    };
    if let Some(message) = &failure {
        wait_for_enter(message)?;
    }
    *terminal = ratatui::init();
    enable_input_modes(session.enhanced_keys)?;
    terminal.clear()?;
    app.error = failure;
    Ok(())
}

/// Keeps a failed command's output on screen until the user has read it.
fn wait_for_enter(message: &str) -> Result<()> {
    let mut stdout = std::io::stdout();
    write!(stdout, "\n{message}\nPress Enter to return to the inbox.")?;
    stdout.flush()?;
    std::io::stdin()
        .read_line(&mut String::new())
        .context("read Enter")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_renderer_warning_names_what_is_missing_and_how_to_add_it() {
        let names = |list: &[&str]| list.iter().map(|name| name.to_string()).collect::<Vec<_>>();
        assert_eq!(renderer_warning(&[], &[]), None);
        assert_eq!(
            renderer_warning(
                &names(&["difftastic"]),
                &names(&["brew install difftastic"])
            )
            .unwrap(),
            "difftastic is not installed, so m skips it. To add it: brew install difftastic"
        );
        assert_eq!(
            renderer_warning(&names(&["delta", "difftastic"]), &[]).unwrap(),
            "delta and difftastic are not installed, so m skips them"
        );
    }
}
