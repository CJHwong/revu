//! Opens a new tab or window that runs a coding agent, in the terminals revu knows.
//! Every terminal runs the same small script, so the message never passes through a
//! terminal's own quoting.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Emulator {
    Ghostty,
    AppleTerminal,
    ITerm,
    WezTerm,
    Kitty,
    GnomeTerminal,
    Konsole,
}

impl Emulator {
    pub fn name(self) -> &'static str {
        match self {
            Emulator::Ghostty => "Ghostty",
            Emulator::AppleTerminal => "Terminal",
            Emulator::ITerm => "iTerm2",
            Emulator::WezTerm => "WezTerm",
            Emulator::Kitty => "kitty",
            Emulator::GnomeTerminal => "GNOME Terminal",
            Emulator::Konsole => "Konsole",
        }
    }

    /// Terminal.app opens a window; the others open a tab.
    pub fn opens(self) -> &'static str {
        match self {
            Emulator::AppleTerminal => "window",
            _ => "tab",
        }
    }
}

/// The terminal revu runs in, from the variables each one sets.
pub fn detect(var: impl Fn(&str) -> Option<String>) -> Option<Emulator> {
    let program = var("TERM_PROGRAM").unwrap_or_default();
    match program.as_str() {
        "ghostty" => return Some(Emulator::Ghostty),
        "Apple_Terminal" => return Some(Emulator::AppleTerminal),
        "iTerm.app" => return Some(Emulator::ITerm),
        "WezTerm" => return Some(Emulator::WezTerm),
        _ => {}
    }
    if var("KITTY_WINDOW_ID").is_some() {
        return Some(Emulator::Kitty);
    }
    if var("GNOME_TERMINAL_SCREEN").is_some() || var("GNOME_TERMINAL_SERVICE").is_some() {
        return Some(Emulator::GnomeTerminal);
    }
    if var("KONSOLE_VERSION").is_some() {
        return Some(Emulator::Konsole);
    }
    None
}

/// Starts `program` in a new tab of `terminal`, in `dir`, with `message` as its prompt.
pub fn open(terminal: Emulator, program: &str, dir: &Path, message: &str) -> Result<(), String> {
    let script = write_script(program, dir, message)
        .map_err(|error| format!("could not write the start script: {error}"))?;
    let result = run(launcher(terminal, &script));
    if result.is_err() {
        let _ = std::fs::remove_file(&script);
        let _ = std::fs::remove_file(script.with_extension("md"));
    }
    result
}

/// The script takes revu's PATH, cds to the clone, reads the message, deletes both
/// files, and runs the agent. It waits for Enter when the agent fails, so the tab does
/// not vanish. A terminal started from the Dock gives its tabs a bare PATH, without the
/// agent or the node it runs on.
fn write_script(program: &str, dir: &Path, message: &str) -> std::io::Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let base = std::env::temp_dir().join(format!("revu-handoff-{}-{nanos}", std::process::id()));
    let script = base.with_extension("sh");
    let text = base.with_extension("md");
    write_private(&text, message)?;
    let path = std::env::var("PATH").unwrap_or_default();
    write_private(&script, &script_body(program, &path, dir, &text, &script))?;
    Ok(script)
}

/// A new file only its owner reads. On Linux the temp folder is shared, so this also
/// refuses a file or symlink someone put at the name first.
fn write_private(path: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(content.as_bytes())
}

fn script_body(program: &str, path: &str, dir: &Path, text: &Path, script: &Path) -> String {
    let (dir, text, script) = (
        shell_quote(&dir.to_string_lossy()),
        shell_quote(&text.to_string_lossy()),
        shell_quote(&script.to_string_lossy()),
    );
    let quoted = shell_quote(program);
    let path = shell_quote(path);
    format!(
        "export PATH={path}\n\
         cd {dir} || exit 1\n\
         message=$(cat {text})\n\
         rm -f {text} {script}\n\
         {quoted} \"$message\" || {{ echo; echo {quoted}' failed. Press Enter to close.'; read _; }}\n"
    )
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

fn applescript_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', r"\\").replace('"', "\\\""))
}

fn launcher(terminal: Emulator, script: &Path) -> Command {
    let script_path = script.to_string_lossy().into_owned();
    let shell_line = format!("/bin/sh {}", shell_quote(&script_path));
    let osascript = |lines: &[String]| {
        let mut command = Command::new("osascript");
        for line in lines {
            command.arg("-e").arg(line);
        }
        command
    };
    match terminal {
        Emulator::Ghostty => osascript(&[
            "tell application \"Ghostty\"".to_string(),
            "set cfg to new surface configuration".to_string(),
            format!("set command of cfg to {}", applescript_string(&shell_line)),
            "new tab in front window with configuration cfg".to_string(),
            "activate".to_string(),
            "end tell".to_string(),
        ]),
        Emulator::AppleTerminal => osascript(&[
            "tell application \"Terminal\"".to_string(),
            format!("do script {}", applescript_string(&shell_line)),
            "activate".to_string(),
            "end tell".to_string(),
        ]),
        Emulator::ITerm => osascript(&[
            "tell application \"iTerm2\"".to_string(),
            format!(
                "tell current window to create tab with default profile command {}",
                applescript_string(&shell_line)
            ),
            "activate".to_string(),
            "end tell".to_string(),
        ]),
        Emulator::WezTerm => argv("wezterm", &["cli", "spawn", "--", "/bin/sh"], &script_path),
        Emulator::Kitty => argv(
            "kitty",
            &["@", "launch", "--type=tab", "/bin/sh"],
            &script_path,
        ),
        Emulator::GnomeTerminal => {
            argv("gnome-terminal", &["--tab", "--", "/bin/sh"], &script_path)
        }
        Emulator::Konsole => argv("konsole", &["--new-tab", "-e", "/bin/sh"], &script_path),
    }
}

fn argv(program: &str, args: &[&str], script: &str) -> Command {
    let mut command = Command::new(program);
    command.args(args).arg(script);
    command
}

fn run(mut command: Command) -> Result<(), String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let output = command
        .output()
        .map_err(|error| format!("{program} did not start: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "{program} could not open a tab: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        move |key| {
            pairs
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
        }
    }

    #[test]
    fn detects_terminals_by_their_variables() {
        assert_eq!(
            detect(env(&[("TERM_PROGRAM", "ghostty")])),
            Some(Emulator::Ghostty)
        );
        assert_eq!(
            detect(env(&[("TERM_PROGRAM", "Apple_Terminal")])),
            Some(Emulator::AppleTerminal)
        );
        assert_eq!(
            detect(env(&[("TERM_PROGRAM", "iTerm.app")])),
            Some(Emulator::ITerm)
        );
        assert_eq!(
            detect(env(&[("TERM_PROGRAM", "WezTerm")])),
            Some(Emulator::WezTerm)
        );
        assert_eq!(
            detect(env(&[("KITTY_WINDOW_ID", "1")])),
            Some(Emulator::Kitty)
        );
        assert_eq!(
            detect(env(&[("GNOME_TERMINAL_SCREEN", "/org/x")])),
            Some(Emulator::GnomeTerminal)
        );
        assert_eq!(
            detect(env(&[("KONSOLE_VERSION", "230800")])),
            Some(Emulator::Konsole)
        );
        assert_eq!(detect(env(&[("TERM_PROGRAM", "Alacritty")])), None);
        assert_eq!(detect(env(&[])), None);
    }

    #[test]
    fn the_script_quotes_paths_and_passes_the_message_whole() {
        let body = script_body(
            "claude",
            "/opt/bin:/usr/bin",
            Path::new("/src/it's here"),
            Path::new("/tmp/m.md"),
            Path::new("/tmp/m.sh"),
        );
        assert!(
            body.starts_with(
                "export PATH='/opt/bin:/usr/bin'\ncd '/src/it'\\''s here' || exit 1\n"
            )
        );
        assert!(body.contains("message=$(cat '/tmp/m.md')\n"));
        assert!(body.contains("'claude' \"$message\" ||"));
    }

    #[test]
    fn the_script_runs_and_cleans_up() {
        let dir = std::env::temp_dir();
        let script = write_script("printf", &dir, "line one\nit's \"two\"").unwrap();
        let output = Command::new("/bin/sh").arg(&script).output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "line one\nit's \"two\""
        );
        assert!(!script.exists());
        assert!(!script.with_extension("md").exists());
    }

    #[test]
    fn applescript_strings_escape_quotes_and_backslashes() {
        assert_eq!(applescript_string(r#"a "b" \c"#), r#""a \"b\" \\c""#);
    }
}
