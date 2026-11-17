mod app;
mod config;
mod file_tree;
mod github;
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
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::supports_keyboard_enhancement;
use ratatui::DefaultTerminal;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use app::{Action, App, DiffJob, DiffKey, PromptKind, RepoJob, RepoState};
use config::Config;
use model::{ChangedFile, Group, PrActivity, PrDetail, PrRef};
use repo::{CloneLookup, LocalRepo};

/// Results from the fetch threads. Errors are strings because they only get displayed.
enum Message {
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
    Detail {
        pr: PrRef,
        result: Result<Box<PrDetail>, String>,
    },
    Files {
        pr: PrRef,
        result: Result<Vec<ChangedFile>, String>,
    },
    Viewed {
        pr: PrRef,
        path: String,
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
        result: Result<Vec<u8>, String>,
    },
    Grep {
        pr: PrRef,
        query: String,
        result: Result<Vec<repo::GrepHit>, String>,
    },
    File {
        pr: PrRef,
        path: String,
        result: Result<String, String>,
    },
}

enum Input {
    Key(KeyEvent),
    Paste(String),
    /// A mouse wheel tick: the pointer's column, and +1 down or -1 up.
    Scroll(u16, isize),
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
        terminal.draw(|frame| ui::render(frame, app, &session.config.theme, Utc::now()))?;
        apply_messages(app, &session.receiver);
        dispatch_requests(app, session);
        let key = match next_input()? {
            None => continue,
            Some(Input::Paste(text)) => {
                app.handle_paste(&text);
                continue;
            }
            Some(Input::Scroll(column, delta)) => {
                app.handle_scroll(column, delta);
                continue;
            }
            Some(Input::Key(key)) => key,
        };
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(());
        }
        let Some(action) = app.handle_key(key) else {
            continue;
        };
        if !perform(action, terminal, app, session)? {
            return Ok(());
        }
    }
}

/// Waits briefly so fetch results still redraw when no key arrives.
fn next_input() -> Result<Option<Input>> {
    if !event::poll(Duration::from_millis(100))? {
        return Ok(None);
    }
    match event::read()? {
        Event::Key(key) if key.kind == KeyEventKind::Press => Ok(Some(Input::Key(key))),
        Event::Paste(text) => Ok(Some(Input::Paste(text))),
        Event::Mouse(mouse) => Ok(match mouse.kind {
            MouseEventKind::ScrollDown => Some(Input::Scroll(mouse.column, 1)),
            MouseEventKind::ScrollUp => Some(Input::Scroll(mouse.column, -1)),
            _ => None,
        }),
        _ => Ok(None),
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
            Message::Activity { pr, result } => {
                app.set_activity(&pr, result.map(|activity| *activity))
            }
            Message::Detail { pr, result } => app.set_detail(&pr, result.map(|detail| *detail)),
            Message::Files { pr, result } => app.set_files(&pr, result),
            Message::Viewed {
                pr,
                path,
                viewed,
                result,
            } => app.set_viewed_result(&pr, &path, viewed, result),
            Message::Repo { pr, result } => app.set_repo(&pr, result),
            Message::Diff { pr, key, result } => app.set_diff(&pr, key, result),
            Message::Grep { pr, query, result } => app.set_grep(&pr, &query, result),
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
        Action::ShowFile {
            pr,
            dir,
            commit,
            path,
        } => {
            let sender = session.sender.clone();
            thread::spawn(move || {
                let result =
                    repo::show_file(&dir, &commit, &path).map_err(|error| format!("{error:#}"));
                let _ = sender.send(Message::File { pr, path, result });
            });
        }
        Action::SetWorkspace(text) => set_workspace(app, session, text),
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
            path,
            viewed,
        } => start_viewed_sync(session, pr, pull_request_id, path, viewed),
    }
    Ok(true)
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
    let Some(workspace) = &session.config.workspace_dir else {
        app.set_repo_state(RepoState::Busy(
            "waiting for the workspace folder".to_string(),
        ));
        app.ask(PromptKind::Workspace, "~/Workspace");
        return;
    };
    let workspace = repo::expand_home(workspace);
    match repo::find_clone(&workspace, &job.pr.repo) {
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

/// Saves the workspace folder, then lets the open PR look for its clone again.
fn set_workspace(app: &mut App, session: &mut Session, text: String) {
    let dir = repo::expand_home(&text);
    if text.is_empty() || !dir.is_dir() {
        let error = format!("{} is not a folder", dir.display());
        app.set_prompt_error(PromptKind::Workspace, &text, error);
        return;
    }
    if let Err(error) = config::append_workspace_dir(&session.config_path, &text) {
        app.set_prompt_error(PromptKind::Workspace, &text, format!("{error:#}"));
        return;
    }
    session.config.workspace_dir = Some(text);
    app.set_repo_state(RepoState::Unknown);
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

fn start_diff(session: &Session, job: DiffJob) {
    let Some(renderer) = session.config.diff_renderers.get(job.key.renderer) else {
        return;
    };
    let paths: Vec<String> = job
        .paths
        .iter()
        .map(|path| repo::shell_quote(path))
        .collect();
    let command = repo::fill_template(
        &renderer.command,
        &[
            ("base", repo::shell_quote(&job.base)),
            ("head", repo::shell_quote(&job.head)),
            ("paths", paths.join(" ")),
            ("width", job.key.width.to_string()),
        ],
    );
    let sender = session.sender.clone();
    thread::spawn(move || {
        let result = repo::run_renderer(&job.dir, &command).map_err(|error| format!("{error:#}"));
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

fn start_activity_fetch(session: &Session, pr: PrRef) {
    let sender = session.sender.clone();
    thread::spawn(move || {
        let result = github::fetch_activity(&pr.repo, pr.number)
            .map(Box::new)
            .map_err(|error| format!("{error:#}"));
        let _ = sender.send(Message::Activity { pr, result });
    });
}

fn start_detail_fetch(session: &Session, pr: PrRef) {
    let sender = session.sender.clone();
    thread::spawn(move || {
        let result = github::fetch_detail(&pr.repo, pr.number)
            .map(Box::new)
            .map_err(|error| format!("{error:#}"));
        let _ = sender.send(Message::Detail { pr, result });
    });
}

fn start_files_fetch(session: &Session, pr: PrRef) {
    let sender = session.sender.clone();
    thread::spawn(move || {
        let result = github::fetch_files(&pr.repo, pr.number).map_err(|error| format!("{error:#}"));
        let _ = sender.send(Message::Files { pr, result });
    });
}

fn start_viewed_sync(
    session: &Session,
    pr: PrRef,
    pull_request_id: String,
    path: String,
    viewed: bool,
) {
    let sender = session.sender.clone();
    thread::spawn(move || {
        let result = github::set_viewed(&pull_request_id, &path, viewed)
            .map_err(|error| format!("{error:#}"));
        let _ = sender.send(Message::Viewed {
            pr,
            path,
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
