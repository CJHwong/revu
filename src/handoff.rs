//! Hands a spot of a pull request to a coding agent: the context revu knows, plus what
//! the user asks. Targets are a Claude Code pane in tmux, a recent Codex session, a new
//! session of either, or the clipboard.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use crate::model::{PrDetail, PrRef, ThreadLine};

/// What revu attaches to the user's request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    /// One line per part, for the compose box to show.
    pub summary: Vec<String>,
    /// The Markdown the agent gets.
    pub text: String,
    /// The local clone, where a new session starts and running ones are looked for.
    pub dir: Option<PathBuf>,
}

/// Where the cursor is: the marked diff lines and the thread on them, or a file.
#[derive(Default)]
pub struct Spot<'a> {
    /// The lines' place, "src/app.rs:10-14", and their Markdown diff block.
    pub lines: Option<(String, String)>,
    pub thread: Option<&'a ThreadLine>,
    pub file: Option<String>,
}

pub struct Checkout<'a> {
    pub dir: &'a Path,
    /// The branch or commit the clone has checked out, when it is not the PR's head.
    pub elsewhere: Option<&'a str>,
}

pub fn context(pr: &PrRef, detail: &PrDetail, clone: Option<Checkout>, spot: Spot) -> Context {
    let title = &detail.summary.title;
    let mut summary = vec![format!("{}#{} {title}", pr.repo, pr.number)];
    let mut text = format!(
        "I'm reviewing a pull request in revu, and I need your help with the spot below.\n\n\
         Pull request: {}#{} \"{title}\"\n{}\n\
         Base: {} at {}\n\
         Head: {} at {}\n",
        pr.repo,
        pr.number,
        pr.url(),
        detail.base,
        detail.base_oid,
        detail.head,
        detail.head_oid,
    );
    let clone_line = match &clone {
        Some(Checkout {
            dir,
            elsewhere: Some(elsewhere),
        }) => format!(
            "Local clone: {}, on {elsewhere}, not the PR's head",
            dir.display()
        ),
        Some(Checkout { dir, .. }) => format!("Local clone: {}, at the PR's head", dir.display()),
        None => "Local clone: none".to_string(),
    };
    text.push_str(&format!("{clone_line}.\n"));
    // revu fetches the PR's commits into the clone to draw the diff, so git can
    // read them even when another branch is checked out.
    if let Some(Checkout {
        elsewhere: Some(_), ..
    }) = &clone
    {
        text.push_str(&format!(
            "The PR's commits are in the clone. Read the PR's version of a file with \
             `git show {head}:<path>`, and its changes with `git diff {base}...{head}`.\n",
            head = detail.head_oid,
            base = detail.base_oid,
        ));
    }
    summary.push(clone_line);
    if let Some(file) = &spot.file {
        summary.push(format!("File: {file}"));
        text.push_str(&format!("\nFile: {file}\n"));
    }
    // The same Markdown y copies, so both pastes read alike.
    if let Some((place, block)) = &spot.lines {
        summary.push(format!("Lines: {place}"));
        text.push_str(&format!("\n{block}"));
    }
    if let Some(thread) = spot.thread {
        summary.push(thread_summary(thread));
        text.push_str(&thread_text(thread));
    }
    Context {
        summary,
        text,
        dir: clone.map(|clone| clone.dir.to_path_buf()),
    }
}

fn thread_summary(thread: &ThreadLine) -> String {
    let author = thread
        .comments
        .first()
        .map_or("someone", |comment| comment.author.login.as_str());
    let count = thread.comments.len();
    let unit = if count == 1 { "comment" } else { "comments" };
    format!("Thread by {author} on {}, {count} {unit}", thread.lines)
}

fn thread_text(thread: &ThreadLine) -> String {
    let state = if thread.resolved { "resolved" } else { "open" };
    let mut text = format!(
        "\nReview thread on {} {} ({state}, ID {}):\n",
        thread.path, thread.lines, thread.id
    );
    for comment in &thread.comments {
        text.push_str(&format!("{}:\n", comment.author.login));
        for line in comment.body.lines() {
            text.push_str(&format!("> {line}\n"));
        }
        text.push('\n');
    }
    text
}

/// The text the agent gets: the context, then the request.
pub fn message(context: &Context, ask: &str) -> String {
    format!(
        "{}\n\nMy request:\n{}\n",
        context.text.trim_end(),
        ask.trim()
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A Claude Code session in a tmux pane: the pane ID, and its session and window.
    ClaudePane {
        pane: String,
        place: String,
    },
    /// A Codex session, by its ID, last active `minutes` ago.
    CodexSession {
        id: String,
        minutes: u64,
    },
    NewClaude,
    NewCodex,
    Clipboard,
}

impl Target {
    pub fn label(&self) -> String {
        match self {
            Target::ClaudePane { place, .. } => format!("Claude Code in tmux {place}"),
            Target::CodexSession { minutes, .. } => {
                format!("Codex session, active {}", ago(*minutes))
            }
            Target::NewClaude => "New Claude Code session".to_string(),
            Target::NewCodex => "New Codex session".to_string(),
            Target::Clipboard => "Copy to clipboard".to_string(),
        }
    }

    pub fn note(&self) -> &'static str {
        match self {
            Target::ClaudePane { .. } => "pastes it in that pane and takes you there; Enter sends",
            Target::CodexSession { .. } => "queues it; the session runs it after its current turn",
            Target::NewClaude | Target::NewCodex => "starts in the clone with this as the prompt",
            Target::Clipboard => "paste it into any agent",
        }
    }
}

fn ago(minutes: u64) -> String {
    match minutes {
        0 => "just now".to_string(),
        1..=59 => format!("{minutes}m ago"),
        _ => format!("{}h ago", minutes / 60),
    }
}

/// Every target for a clone, running sessions first. Without a clone, only the clipboard.
pub fn find_targets(dir: Option<&Path>) -> Vec<Target> {
    let mut targets = Vec::new();
    if let Some(dir) = dir.and_then(|dir| std::fs::canonicalize(dir).ok()) {
        targets.extend(claude_panes(&dir));
        targets.extend(codex_sessions(&dir));
        if installed("claude") {
            targets.push(Target::NewClaude);
        }
        if installed("codex") {
            targets.push(Target::NewCodex);
        }
    }
    targets.push(Target::Clipboard);
    targets
}

fn installed(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(&path).any(|folder| {
        std::fs::metadata(folder.join(program))
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    })
}

/// One tmux pane: its ID, the PID of its first process, its folder, and where it is.
#[derive(Debug, PartialEq, Eq)]
struct Pane {
    id: String,
    pid: u32,
    path: PathBuf,
    place: String,
}

const PANE_FORMAT: &str =
    "#{pane_id}\t#{pane_pid}\t#{pane_current_path}\t#{session_name}:#{window_index}.#{pane_index}";

fn parse_panes(output: &str) -> Vec<Pane> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            Some(Pane {
                id: fields.next()?.to_string(),
                pid: fields.next()?.parse().ok()?,
                path: PathBuf::from(fields.next()?),
                place: fields.next()?.to_string(),
            })
        })
        .collect()
}

/// Each process's parent and program name, from `ps -A -o pid=,ppid=,comm=`.
fn parse_processes(output: &str) -> HashMap<u32, (u32, String)> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let parent = fields.next()?.parse().ok()?;
            let command = fields.collect::<Vec<_>>().join(" ");
            let name = command.rsplit('/').next()?.to_string();
            Some((pid, (parent, name)))
        })
        .collect()
}

/// The pane's first process or one under it runs `program`. tmux names Claude Code's
/// pane by its version, so the process name decides, not the pane's command.
fn runs_in_tree(root: u32, program: &str, processes: &HashMap<u32, (u32, String)>) -> bool {
    processes.iter().any(|(&pid, (_, name))| {
        if name != program {
            return false;
        }
        let mut current = pid;
        // A cap guards against a cycle in a table that changed while ps read it.
        for _ in 0..64 {
            if current == root {
                return true;
            }
            match processes.get(&current) {
                Some(&(parent, _)) if parent != current => current = parent,
                _ => return false,
            }
        }
        false
    })
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn claude_panes(dir: &Path) -> Vec<Target> {
    let Some(panes) = command_output("tmux", &["list-panes", "-a", "-F", PANE_FORMAT]) else {
        return Vec::new();
    };
    let Some(processes) = command_output("ps", &["-A", "-o", "pid=,ppid=,comm="]) else {
        return Vec::new();
    };
    let processes = parse_processes(&processes);
    parse_panes(&panes)
        .into_iter()
        .filter(|pane| std::fs::canonicalize(&pane.path).is_ok_and(|path| in_checkout(&path, dir)))
        .filter(|pane| runs_in_tree(pane.pid, "claude", &processes))
        .map(|pane| Target::ClaudePane {
            pane: pane.id,
            place: pane.place,
        })
        .collect()
}

/// The folder is the clone or inside it, and not inside another checkout nested in it,
/// such as a worktree under `.claude/worktrees`, which can be on another branch.
fn in_checkout(path: &Path, dir: &Path) -> bool {
    if !path.starts_with(dir) {
        return false;
    }
    !path
        .ancestors()
        .take_while(|folder| *folder != dir)
        .any(|folder| folder.join(".git").exists())
}

/// Codex keeps each session in `sessions/YYYY/MM/DD/*.jsonl`; the first line names its
/// ID and folder. Sessions written in the last day, in the clone, newest first.
fn codex_sessions(dir: &Path) -> Vec<Target> {
    let root = match std::env::var_os("CODEX_HOME") {
        Some(home) => PathBuf::from(home),
        None => match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(".codex"),
            None => return Vec::new(),
        },
    };
    let today = chrono::Local::now().date_naive();
    let days = [today, today - chrono::Days::new(1)];
    let now = SystemTime::now();
    let mut sessions: Vec<(Duration, String)> = days
        .iter()
        .map(|day| {
            root.join("sessions")
                .join(day.format("%Y/%m/%d").to_string())
        })
        .filter_map(|folder| std::fs::read_dir(folder).ok())
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let age = now
                .duration_since(entry.metadata().ok()?.modified().ok()?)
                .ok()?;
            if age > Duration::from_secs(24 * 3600) {
                return None;
            }
            let (id, cwd) = codex_session_meta(&entry.path())?;
            let cwd = std::fs::canonicalize(cwd).ok()?;
            in_checkout(&cwd, dir).then_some((age, id))
        })
        .collect();
    sessions.sort();
    sessions
        .into_iter()
        .take(3)
        .map(|(age, id)| Target::CodexSession {
            id,
            minutes: age.as_secs() / 60,
        })
        .collect()
}

fn codex_session_meta(path: &Path) -> Option<(String, String)> {
    let mut first = String::new();
    BufReader::new(std::fs::File::open(path).ok()?)
        .read_line(&mut first)
        .ok()?;
    parse_codex_meta(&first)
}

fn parse_codex_meta(line: &str) -> Option<(String, String)> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    if value.get("type")?.as_str()? != "session_meta" {
        return None;
    }
    let payload = value.get("payload")?;
    let id = payload.get("id")?.as_str()?.to_string();
    let cwd = payload.get("cwd")?.as_str()?.to_string();
    Some((id, cwd))
}

/// How a target takes the message.
pub enum Delivery {
    /// Done; the footer says this.
    Sent(String),
    /// A program that needs the terminal; revu steps aside while it runs.
    RunHere(Command),
}

/// Sends the message. The clipboard is the caller's, as revu already copies text.
pub fn deliver(target: &Target, dir: Option<&Path>, message: &str) -> Result<Delivery, String> {
    match target {
        Target::ClaudePane { pane, place } => {
            paste_into_pane(pane, message)?;
            // Moving to the pane is a convenience: the text is there either way.
            let note = match move_to_pane(pane) {
                Ok(()) => format!("pasted into Claude Code in {place}; press Enter there to send"),
                Err(error) => format!(
                    "pasted into Claude Code in {place}, but could not take you there ({error}); press Enter in that pane to send"
                ),
            };
            Ok(Delivery::Sent(note))
        }
        Target::CodexSession { id, .. } => {
            run("codex", &["queue", "--thread", id, "--message", message])?;
            Ok(Delivery::Sent(
                "queued to the Codex session; it runs after its current turn".to_string(),
            ))
        }
        Target::NewClaude => start_session("claude", dir, message),
        Target::NewCodex => start_session("codex", dir, message),
        Target::Clipboard => Err("the clipboard is not delivered here".to_string()),
    }
}

/// A bracketed paste, so newlines stay in the prompt instead of sending it. No Enter
/// follows: a dialog open in the pane would take it as a yes.
fn paste_into_pane(pane: &str, message: &str) -> Result<(), String> {
    let mut load = Command::new("tmux")
        .args(["load-buffer", "-b", "revu-handoff", "-"])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("tmux did not start: {error}"))?;
    if let Some(mut input) = load.stdin.take()
        && let Err(error) = input.write_all(message.as_bytes())
    {
        let _ = load.kill();
        let _ = load.wait();
        return Err(format!("could not give tmux the text: {error}"));
    }
    let output = load
        .wait_with_output()
        .map_err(|error| format!("tmux load-buffer failed: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "tmux load-buffer failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    run(
        "tmux",
        &["paste-buffer", "-p", "-d", "-b", "revu-handoff", "-t", pane],
    )
}

fn move_to_pane(pane: &str) -> Result<(), String> {
    if std::env::var_os("TMUX").is_none() {
        return Err("revu is not in tmux".to_string());
    }
    run("tmux", &["switch-client", "-t", pane])
}

/// In tmux, a new window beside revu. Else a new tab of a terminal revu knows. Else
/// the session takes revu's terminal until it exits.
fn start_session(program: &str, dir: Option<&Path>, message: &str) -> Result<Delivery, String> {
    let dir = dir.ok_or("no local clone to start the session in")?;
    if std::env::var_os("TMUX").is_none() {
        if let Some(terminal) = crate::agent_tab::detect(|key| std::env::var(key).ok()) {
            crate::agent_tab::open(terminal, program, dir, message)?;
            return Ok(Delivery::Sent(format!(
                "started {program} in a new {} {}",
                terminal.name(),
                terminal.opens()
            )));
        }
        let mut command = Command::new(program);
        command.arg(message).current_dir(dir);
        return Ok(Delivery::RunHere(command));
    }
    let folder = dir.to_string_lossy();
    run(
        "tmux",
        &["new-window", "-c", &folder, "-n", program, program, message],
    )?;
    Ok(Delivery::Sent(format!(
        "started {program} in a new tmux window"
    )))
}

fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|error| format!("{program} did not start: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "{program} {} failed: {}",
        args.first().unwrap_or(&""),
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tmux_panes() {
        let output = "%0\t95905\t/private/tmp/repo\twork:1.0\n%1\tnot-a-pid\t/x\tw:2.0\n";
        assert_eq!(
            parse_panes(output),
            vec![Pane {
                id: "%0".to_string(),
                pid: 95905,
                path: PathBuf::from("/private/tmp/repo"),
                place: "work:1.0".to_string(),
            }]
        );
    }

    #[test]
    fn finds_an_agent_under_the_pane_shell() {
        let table = "  100     1 -zsh\n  200   100 /opt/homebrew/bin/codex\n  300     1 claude\n  400   300 node\n";
        let processes = parse_processes(table);
        assert!(runs_in_tree(100, "codex", &processes));
        assert!(!runs_in_tree(100, "claude", &processes));
        // A pane started with claude itself has it as its first process.
        assert!(runs_in_tree(300, "claude", &processes));
    }

    #[test]
    fn a_process_cycle_ends() {
        let processes = parse_processes("  5 6 claude\n  6 5 zsh\n");
        assert!(!runs_in_tree(1, "claude", &processes));
    }

    #[test]
    fn reads_the_codex_session_header() {
        let line = r#"{"type":"session_meta","payload":{"id":"01a1","cwd":"/tmp/repo","originator":"codex-tui"}}"#;
        assert_eq!(
            parse_codex_meta(line),
            Some(("01a1".to_string(), "/tmp/repo".to_string()))
        );
        assert_eq!(parse_codex_meta(r#"{"type":"event","payload":{}}"#), None);
        assert_eq!(parse_codex_meta("not json"), None);
    }

    #[test]
    fn the_request_follows_the_context() {
        let context = Context {
            summary: Vec::new(),
            text: "I'm reviewing a pull request in revu.\n\nFile: a.rs\n".to_string(),
            dir: None,
        };
        assert_eq!(
            message(&context, "  why is this here?\n"),
            "I'm reviewing a pull request in revu.\n\nFile: a.rs\n\nMy request:\nwhy is this here?\n"
        );
    }

    #[test]
    fn context_names_the_pr_the_lines_and_the_thread() {
        let detail = crate::model::tests::sample_detail();
        let activity = crate::model::tests::sample_activity();
        let thread = activity
            .timeline
            .iter()
            .flat_map(|comment| &comment.threads)
            .next()
            .expect("the fixture has a thread");
        let pr = PrRef {
            repo: "acme/widgets".to_string(),
            number: 7,
        };
        let spot = Spot {
            lines: Some((
                "a.rs:3".to_string(),
                "a.rs:3\n```diff\n+x\n```\n".to_string(),
            )),
            thread: Some(thread),
            file: None,
        };
        let checkout = Checkout {
            dir: Path::new("/src/widgets"),
            elsewhere: Some("main"),
        };
        let context = context(&pr, &detail, Some(checkout), spot);
        assert!(context.text.contains("acme/widgets#7"));
        assert!(context.text.contains(&detail.head_oid));
        assert!(context.text.contains("on main, not the PR's head"));
        assert!(
            context
                .text
                .contains(&format!("git show {}:<path>", detail.head_oid))
        );
        assert!(context.text.contains("+x"));
        assert!(context.text.contains(&format!("ID {}", thread.id)));
        assert_eq!(context.dir, Some(PathBuf::from("/src/widgets")));
        assert_eq!(context.summary.len(), 4, "PR, clone, lines, thread");
    }

    #[test]
    fn a_nested_checkout_is_not_the_clone() {
        let clone = std::env::temp_dir().join(format!("revu-handoff-{}", std::process::id()));
        let worktree = clone.join(".claude/worktrees/other");
        let sub = clone.join("src");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(worktree.join(".git"), "gitdir: elsewhere\n").unwrap();
        assert!(in_checkout(&clone, &clone));
        assert!(in_checkout(&sub, &clone));
        assert!(!in_checkout(&worktree, &clone));
        assert!(!in_checkout(&worktree.join("src"), &clone));
        assert!(!in_checkout(&std::env::temp_dir(), &clone));
        std::fs::remove_dir_all(&clone).unwrap();
    }

    #[test]
    fn labels_say_how_recent() {
        let session = Target::CodexSession {
            id: "x".to_string(),
            minutes: 125,
        };
        assert_eq!(session.label(), "Codex session, active 2h ago");
        assert_eq!(ago(0), "just now");
    }
}
