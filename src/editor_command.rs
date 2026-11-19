//! The command `o` runs to open a file at a line in the user's editor.

use std::path::Path;

use crate::config::EditorConfig;
use crate::repo::{fill_template, shell_quote};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorCommand {
    /// The program's name, for the menu and the footer.
    pub name: String,
    pub shell: String,
    /// A terminal editor takes over the screen, so revu waits for it to exit. An editor
    /// with its own window starts beside revu, which stays on screen.
    pub wait: bool,
}

/// How an editor takes the line to open at.
#[derive(Debug, PartialEq, Eq)]
enum LineArgument {
    /// `+12 file`: vi, vim, nvim, helix, nano, emacs, micro, and the default for others.
    Plus,
    /// `dir -g file:12`: VS Code and its forks.
    Goto,
    /// `dir file:12`: Zed and Sublime Text.
    Colon,
    /// `dir --line 12 file`: the JetBrains IDEs.
    LineFlag,
}

fn line_argument(program: &str) -> LineArgument {
    match program {
        "code" | "code-insiders" | "codium" | "cursor" | "windsurf" => LineArgument::Goto,
        "zed" | "subl" => LineArgument::Colon,
        "idea" | "pycharm" | "charm" | "goland" | "webstorm" | "clion" | "rubymine"
        | "phpstorm" | "rustrover" => LineArgument::LineFlag,
        _ => LineArgument::Plus,
    }
}

/// True for an editor with its own window, which revu starts beside itself.
pub fn opens_a_window(command: &str) -> bool {
    line_argument(&program_name(command)) != LineArgument::Plus
}

/// The editor the shell would pick: $VISUAL, then $EDITOR, then vi.
pub fn shell_editor() -> String {
    ["VISUAL", "EDITOR"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "vi".to_string())
}

/// The program in a command, without its folder: "hx" for "/opt/homebrew/bin/hx -v".
fn program_name(command: &str) -> String {
    let first = command.split_whitespace().next().unwrap_or("");
    let name = first.rsplit('/').next().unwrap_or(first);
    name.to_string()
}

/// The configured editor's name, else the shell editor's.
pub fn editor_name(custom: Option<&EditorConfig>, shell_editor: &str) -> String {
    program_name(custom.map_or(shell_editor, |custom| &custom.command))
}

/// The command that opens `path` in the clone `dir` at `line`. A configured command
/// wins; otherwise the shell editor, with the line argument its name calls for. An
/// editor with its own window also gets the clone, so it opens as a project.
pub fn editor_command(
    custom: Option<&EditorConfig>,
    shell_editor: &str,
    dir: &Path,
    path: &str,
    line: u64,
) -> EditorCommand {
    let folder = shell_quote(&dir.to_string_lossy());
    let file = shell_quote(&dir.join(path).to_string_lossy());
    let name = editor_name(custom, shell_editor);
    if let Some(custom) = custom {
        let values = [("dir", folder), ("file", file), ("line", line.to_string())];
        return EditorCommand {
            name,
            shell: fill_template(&custom.command, &values),
            wait: custom.wait,
        };
    }
    let (arguments, wait) = match line_argument(&name) {
        LineArgument::Plus => (format!("+{line} {file}"), true),
        LineArgument::Goto => (format!("{folder} -g {file}:{line}"), false),
        LineArgument::Colon => (format!("{folder} {file}:{line}"), false),
        LineArgument::LineFlag => (format!("{folder} --line {line} {file}"), false),
    };
    // Unquoted, so an editor with flags, such as "code -n", splits into its words.
    EditorCommand {
        name,
        shell: format!("{shell_editor} {arguments}"),
        wait,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(editor: &str) -> EditorCommand {
        editor_command(None, editor, Path::new("/w/repo"), "src/a b.rs", 12)
    }

    #[test]
    fn a_terminal_editor_takes_plus_line_and_revu_waits() {
        let command = open("/opt/homebrew/bin/hx");
        assert_eq!(command.name, "hx");
        assert_eq!(
            command.shell,
            "/opt/homebrew/bin/hx +12 '/w/repo/src/a b.rs'"
        );
        assert!(command.wait);
        assert!(open("nvim").wait);
    }

    #[test]
    fn an_unknown_editor_gets_plus_line() {
        assert_eq!(open("kak").shell, "kak +12 '/w/repo/src/a b.rs'");
    }

    #[test]
    fn a_windowed_editor_opens_the_clone_at_the_line_beside_revu() {
        let code = open("code -n");
        assert_eq!(code.shell, "code -n '/w/repo' -g '/w/repo/src/a b.rs':12");
        assert!(!code.wait);
        assert_eq!(open("zed").shell, "zed '/w/repo' '/w/repo/src/a b.rs':12");
        assert_eq!(
            open("/Applications/PyCharm.app/Contents/MacOS/pycharm").shell,
            "/Applications/PyCharm.app/Contents/MacOS/pycharm '/w/repo' --line 12 '/w/repo/src/a b.rs'"
        );
    }

    #[test]
    fn a_configured_command_wins_over_the_shell_editor() {
        let custom = EditorConfig {
            command: "myedit --at {line} {file} --root {dir}".to_string(),
            wait: false,
        };
        let command = editor_command(Some(&custom), "vim", Path::new("/w/repo"), "a.rs", 3);
        assert_eq!(
            command,
            EditorCommand {
                name: "myedit".to_string(),
                shell: "myedit --at 3 '/w/repo/a.rs' --root '/w/repo'".to_string(),
                wait: false,
            }
        );
    }
}
