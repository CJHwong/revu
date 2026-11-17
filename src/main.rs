mod app;
mod config;
mod github;
mod model;
mod ui;

use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use app::{Action, App};
use config::Config;
use model::{Group, PrDetail, PrSummary};

/// Results from the fetch threads. Errors are strings because they only get displayed.
enum Message {
    Inbox(Result<Vec<Group>, String>, DateTime<Utc>),
    Detail { repo: String, number: u64, result: Result<Box<PrDetail>, String> },
}

struct Session {
    config: Config,
    config_path: PathBuf,
    sender: Sender<Message>,
    receiver: Receiver<Message>,
}

fn main() -> Result<()> {
    let config_path = config::default_path()?;
    let config = config::load_or_init(&config_path)?;
    let (sender, receiver) = mpsc::channel();
    let mut session = Session { config, config_path, sender, receiver };
    let mut app = App::new();
    start_inbox_fetch(&session, &mut app);

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app, &mut session);
    ratatui::restore();
    result
}

fn run(terminal: &mut DefaultTerminal, app: &mut App, session: &mut Session) -> Result<()> {
    loop {
        terminal.draw(|frame| ui::render(frame, app, Utc::now()))?;
        apply_messages(app, &session.receiver);
        let Some(key) = next_key()? else {
            continue;
        };
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(());
        }
        let Some(action) = app.handle_key(key.code) else {
            continue;
        };
        if !perform(action, terminal, app, session)? {
            return Ok(());
        }
    }
}

/// Waits briefly so fetch results still redraw when no key arrives.
fn next_key() -> Result<Option<KeyEvent>> {
    if !event::poll(Duration::from_millis(100))? {
        return Ok(None);
    }
    match event::read()? {
        Event::Key(key) if key.kind == KeyEventKind::Press => Ok(Some(key)),
        _ => Ok(None),
    }
}

fn apply_messages(app: &mut App, receiver: &Receiver<Message>) {
    for message in receiver.try_iter() {
        match message {
            Message::Inbox(Ok(groups), fetched_at) => app.set_groups(groups, fetched_at),
            Message::Inbox(Err(error), _) => app.set_inbox_error(error),
            Message::Detail { repo, number, result } => app.set_detail(&repo, number, result.map(|detail| *detail)),
        }
    }
}

/// Returns false when the app should quit.
fn perform(action: Action, terminal: &mut DefaultTerminal, app: &mut App, session: &mut Session) -> Result<bool> {
    match action {
        Action::Quit => return Ok(false),
        Action::Refresh => refresh(app, session),
        Action::FetchDetail(pr) => start_detail_fetch(session, pr),
        Action::OpenBrowser(url) => open_browser(app, &url),
        Action::RunDiff(pr) => run_diff(terminal, app, &session.config.open_diff, &pr)?,
    }
    Ok(true)
}

/// Reloads the config first, so edits to the groups apply without a restart.
fn refresh(app: &mut App, session: &mut Session) {
    match config::load_or_init(&session.config_path) {
        Ok(config) => session.config = config,
        Err(error) => {
            app.set_inbox_error(format!("config not reloaded: {error:#}"));
            return;
        }
    }
    start_inbox_fetch(session, app);
}

fn start_inbox_fetch(session: &Session, app: &mut App) {
    app.loading = true;
    let config = session.config.clone();
    let sender = session.sender.clone();
    thread::spawn(move || {
        let result = github::fetch_inbox(&config, Local::now().date_naive()).map_err(|error| format!("{error:#}"));
        // The receiver only goes away when the app exits, so a failed send needs no handling.
        let _ = sender.send(Message::Inbox(result, Utc::now()));
    });
}

fn start_detail_fetch(session: &Session, pr: PrSummary) {
    let sender = session.sender.clone();
    thread::spawn(move || {
        let result = github::fetch_detail(&pr.repo, pr.number).map(Box::new).map_err(|error| format!("{error:#}"));
        let _ = sender.send(Message::Detail { repo: pr.repo, number: pr.number, result });
    });
}

fn open_browser(app: &mut App, url: &str) {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    if let Err(error) = Command::new(opener).arg(url).status() {
        app.error = Some(format!("{opener} failed: {error}"));
    }
}

/// Hands the terminal to the diff command, then takes it back.
fn run_diff(terminal: &mut DefaultTerminal, app: &mut App, template: &str, pr: &PrSummary) -> Result<()> {
    let command = template
        .replace("{repo}", &pr.repo)
        .replace("{number}", &pr.number.to_string())
        .replace("{url}", &pr.url);
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
    terminal.clear()?;
    app.error = failure;
    Ok(())
}

/// Keeps a failed command's output on screen until the user has read it.
fn wait_for_enter(message: &str) -> Result<()> {
    let mut stdout = std::io::stdout();
    write!(stdout, "\n{message}\nPress Enter to return to the inbox.")?;
    stdout.flush()?;
    std::io::stdin().read_line(&mut String::new()).context("read Enter")?;
    Ok(())
}
