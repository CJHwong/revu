mod app;
mod config;
mod github;
mod model;
mod theme;
mod ui;

use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
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

use app::{Action, App};
use config::Config;
use model::{Group, PrDetail, PrRef};

/// Results from the fetch threads. Errors are strings because they only get displayed.
enum Message {
    Inbox(Result<Vec<Group>, String>, DateTime<Utc>),
    Detail {
        pr: PrRef,
        result: Result<Box<PrDetail>, String>,
    },
}

enum Input {
    Key(KeyEvent),
    Paste(String),
}

struct Session {
    config: Config,
    config_path: PathBuf,
    sender: Sender<Message>,
    receiver: Receiver<Message>,
    /// Whether the terminal reports modifiers such as Cmd (kitty keyboard protocol).
    enhanced_keys: bool,
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
    };
    let mut app = App::new();
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
    execute!(stdout, EnableBracketedPaste)?;
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
    execute!(stdout, DisableBracketedPaste)?;
    Ok(())
}

fn run(terminal: &mut DefaultTerminal, app: &mut App, session: &mut Session) -> Result<()> {
    loop {
        terminal.draw(|frame| ui::render(frame, app, &session.config.theme, Utc::now()))?;
        apply_messages(app, &session.receiver);
        let key = match next_input()? {
            None => continue,
            Some(Input::Paste(text)) => {
                app.handle_paste(&text);
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
        _ => Ok(None),
    }
}

fn apply_messages(app: &mut App, receiver: &Receiver<Message>) {
    for message in receiver.try_iter() {
        match message {
            Message::Inbox(Ok(groups), fetched_at) => app.set_groups(groups, fetched_at),
            Message::Inbox(Err(error), _) => app.set_inbox_error(error),
            Message::Detail { pr, result } => app.set_detail(&pr, result.map(|detail| *detail)),
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
        Action::FetchDetail(pr) => start_detail_fetch(session, pr),
        Action::OpenBrowser(url) => open_browser(app, &url),
        Action::RunDiff(pr) => run_diff(terminal, app, session, &pr)?,
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
        let result = github::fetch_inbox(&config, Local::now().date_naive())
            .map_err(|error| format!("{error:#}"));
        // The receiver only goes away when the app exits, so a failed send needs no handling.
        let _ = sender.send(Message::Inbox(result, Utc::now()));
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
        .replace("{repo}", &pr.repo)
        .replace("{number}", &pr.number.to_string())
        .replace("{url}", &pr.url());
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
