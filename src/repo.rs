//! Local clones: finding the user's clone of a repo, fetching a PR into it without
//! touching the working tree, and running diff renderers inside it.

use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// Where the PR's commits are kept in the user's clone. Nothing else writes here.
const REF_NAMESPACE: &str = "refs/revu/pr";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloneLookup {
    Found(PathBuf),
    /// No clone anywhere in the workspace; this is where a new one would go.
    Missing(PathBuf),
    /// The folder named after the repo is a clone of something else.
    Conflict {
        dir: PathBuf,
        origin: String,
    },
}

/// The local clone, ready to diff: both PR commits exist in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalRepo {
    pub dir: PathBuf,
    /// New path to old path, for files the PR renamed.
    pub renames: HashMap<String, String>,
}

pub fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ if path == "~" => {
            std::env::var_os("HOME").map_or_else(|| PathBuf::from(path), PathBuf::from)
        }
        _ => PathBuf::from(path),
    }
}

/// `owner/name` in lower case from any GitHub remote form, or `None` for other hosts.
pub fn github_repo_of_remote(url: &str) -> Option<String> {
    let url = url.trim();
    let path = [
        "git@github.com:",
        "ssh://git@github.com/",
        "https://github.com/",
        "http://github.com/",
    ]
    .iter()
    .find_map(|prefix| url.strip_prefix(prefix))?;
    let path = path.trim_end_matches('/').trim_end_matches(".git");
    let (owner, name) = path.split_once('/')?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    Some(format!("{owner}/{name}").to_lowercase())
}

fn origin_of(dir: &Path) -> Option<String> {
    let output = git(dir, &["remote", "get-url", "origin"]).ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn is_clone_of(dir: &Path, repo: &str) -> bool {
    origin_of(dir).and_then(|url| github_repo_of_remote(&url)) == Some(repo.to_lowercase())
}

/// Tries `<workspace>/<name>` first, then every folder directly inside the workspace,
/// because a clone's folder name does not always match its repo name.
pub fn find_clone(workspace: &Path, repo: &str) -> Result<CloneLookup> {
    // Without git every folder would look like a clone of nothing, and a clone fails.
    if !on_path("git") {
        bail!("git is not installed, so no diff can show. To add it: {GIT_INSTALL}");
    }
    let name = repo.split_once('/').map_or(repo, |(_, name)| name);
    let named = workspace.join(name);
    if named.exists() && is_clone_of(&named, repo) {
        return Ok(CloneLookup::Found(named));
    }
    let entries =
        fs::read_dir(workspace).with_context(|| format!("read {}", workspace.display()))?;
    for entry in entries.flatten() {
        let dir = entry.path();
        if dir.join(".git").exists() && is_clone_of(&dir, repo) {
            return Ok(CloneLookup::Found(dir));
        }
    }
    if named.exists() {
        let origin = origin_of(&named).unwrap_or_else(|| "no origin remote".to_string());
        return Ok(CloneLookup::Conflict { dir: named, origin });
    }
    Ok(CloneLookup::Missing(named))
}

/// A regular clone through gh, so it uses the user's GitHub login and protocol.
pub fn clone(repo: &str, dir: &Path) -> Result<()> {
    let output = Command::new("gh")
        .args(["repo", "clone", repo])
        .arg(dir)
        .output()
        .context("run gh repo clone")?;
    ensure_success(&output, "gh repo clone")
}

/// Makes both PR commits available without touching the working tree, the index or
/// FETCH_HEAD, which other tools in the same clone may be reading.
pub fn fetch_pr(
    dir: &Path,
    number: u64,
    base_ref: &str,
    base_oid: &str,
    head_oid: &str,
) -> Result<()> {
    if has_commit(dir, base_oid) && has_commit(dir, head_oid) {
        return Ok(());
    }
    let head_spec = format!("+refs/pull/{number}/head:{REF_NAMESPACE}/{number}/head");
    let base_spec = format!("+refs/heads/{base_ref}:{REF_NAMESPACE}/{number}/base");
    let args = [
        "fetch",
        "--no-tags",
        "--no-write-fetch-head",
        "origin",
        &head_spec,
        &base_spec,
    ];
    ensure_success(&git(dir, &args)?, "git fetch")?;
    for oid in [base_oid, head_oid] {
        if !has_commit(dir, oid) {
            bail!("commit {oid} is still missing after the fetch");
        }
    }
    Ok(())
}

fn has_commit(dir: &Path, oid: &str) -> bool {
    let object = format!("{oid}^{{commit}}");
    git(dir, &["cat-file", "-e", &object]).is_ok_and(|output| output.status.success())
}

/// GitHub's file list carries no previous name, so renames come from the local diff.
pub fn renames(dir: &Path, base_oid: &str, head_oid: &str) -> Result<HashMap<String, String>> {
    let range = format!("{base_oid}...{head_oid}");
    let output = git(dir, &["diff", "--name-status", "-M", "-z", &range])?;
    ensure_success(&output, "git diff --name-status")?;
    Ok(parse_renames(&output.stdout))
}

/// Parses `git diff --name-status -z`: a status field, then one path, or two for a
/// rename or copy (`R<score>`, old, new).
pub fn parse_renames(output: &[u8]) -> HashMap<String, String> {
    let mut fields = output
        .split(|byte| *byte == 0)
        .map(|field| String::from_utf8_lossy(field).into_owned());
    let mut renames = HashMap::new();
    while let Some(status) = fields.next() {
        if status.is_empty() {
            break;
        }
        let first = fields.next().unwrap_or_default();
        if status.starts_with('R') || status.starts_with('C') {
            let new_path = fields.next().unwrap_or_default();
            renames.insert(new_path, first);
        }
    }
    renames
}

/// One `git grep` match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepHit {
    pub path: String,
    pub line: usize,
    pub text: String,
}

/// Stops a search for a common word from filling memory and the screen.
pub const MAX_GREP_HITS: usize = 1000;

/// Searches every file at `commit`, case-insensitive and literal, without a checkout.
/// Reads the output as it streams and stops git at `MAX_GREP_HITS`, so a common word
/// in a large repo does not buffer hundreds of megabytes.
pub fn grep(dir: &Path, commit: &str, query: &str) -> Result<Vec<GrepHit>> {
    let args = [
        "grep",
        "-n",
        "-I",
        "-i",
        "-F",
        "-z",
        "--no-color",
        "-e",
        query,
        commit,
    ];
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| git_spawn_error(error, "git grep"))?;
    let stdout = child.stdout.take().context("read git grep output")?;
    let prefix = format!("{commit}:");
    let mut hits = Vec::new();
    for record in BufReader::new(stdout).split(b'\n') {
        hits.extend(parse_grep_record(
            &record.context("read git grep output")?,
            &prefix,
        ));
        if hits.len() == MAX_GREP_HITS {
            // Enough to show; git's exit status no longer matters.
            let _ = child.kill();
            let _ = child.wait();
            return Ok(hits);
        }
    }
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        pipe.read_to_string(&mut stderr)
            .context("read git grep errors")?;
    }
    let status = child.wait().context("wait for git grep")?;
    // git grep exits 1 when nothing matches, which is a result, not a failure.
    if status.success() || (status.code() == Some(1) && stderr.is_empty()) {
        return Ok(hits);
    }
    bail!("git grep failed ({status}): {}", stderr.trim())
}

/// Parses one `git grep -n -z <commit>` record: `<commit>:<path>\0<line>\0<text>`.
pub fn parse_grep_record(record: &[u8], prefix: &str) -> Option<GrepHit> {
    let record = String::from_utf8_lossy(record);
    let mut fields = record.splitn(3, '\0');
    let path = fields.next()?.strip_prefix(prefix)?.to_string();
    let line = fields.next()?.parse().ok()?;
    let text = fields.next().unwrap_or_default().to_string();
    Some(GrepHit { path, line, text })
}

/// The file's full text at `commit`, for reading code the diff does not show.
pub fn show_file(dir: &Path, commit: &str, path: &str) -> Result<String> {
    let object = format!("{commit}:{path}");
    let output = git(dir, &["show", &object])?;
    ensure_success(&output, "git show")?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Context lines that cover any file a reviewer reads.
const WHOLE_FILE_CONTEXT: &str = "1000000";

/// The file at `commit` with syntax colors from `bat`, the `line` highlighted (0 for none).
/// Without `bat` the file comes back plain, with line numbers.
pub fn highlight_file(
    dir: &Path,
    commit: &str,
    path: &str,
    line: usize,
    bat_theme: &str,
) -> Result<Vec<u8>> {
    let text = show_file(dir, commit, path)?;
    let line = line.to_string();
    let mut args = vec!["--color=always", "--style=numbers", "--paging=never"];
    args.extend(["--file-name", path, "--theme", bat_theme]);
    if line != "0" {
        args.extend(["--highlight-line", &line]);
    }
    match bat(text.clone(), &args) {
        Err(error) if is_missing(&error) => Ok(numbered(&text)),
        result => result,
    }
}

/// Runs `bat` on `input` and returns its colored output.
pub fn bat(input: String, args: &[&str]) -> Result<Vec<u8>> {
    let mut child = Command::new("bat")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("run bat")?;
    let mut stdin = child.stdin.take().context("open bat's input")?;
    // Write from another thread: bat fills its output pipe while it reads a large file.
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = child.wait_with_output().context("wait for bat")?;
    let _ = writer.join();
    ensure_success(&output, "bat")?;
    Ok(output.stdout)
}

fn is_missing(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

fn numbered(text: &str) -> Vec<u8> {
    let gutter = text.lines().count().to_string().len();
    let lines = text
        .lines()
        .enumerate()
        .map(|(index, line)| format!("{:>gutter$} {line}\n", index + 1));
    lines.collect::<String>().into_bytes()
}

/// Single-quotes a value for `sh`, so a file name such as `$(rm -rf ~).md` stays text.
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Replaces each `{name}` with its value. Values must already be quoted for the shell.
pub fn fill_template(template: &str, values: &[(&str, String)]) -> String {
    values
        .iter()
        .fold(template.to_string(), |command, (name, value)| {
            command.replace(&format!("{{{name}}}"), value)
        })
}

/// Runs a renderer command inside the clone and returns its colored output. With
/// `whole_file`, git and difftastic show the whole file around the changes, so the
/// renderer draws the full file with its changes marked.
/// How to install git here, for the warnings.
pub const GIT_INSTALL: &str = if cfg!(target_os = "macos") {
    "xcode-select --install"
} else {
    "your package manager, such as apt install git"
};

/// A git that would not start says why: git missing has its own words.
fn git_spawn_error(error: std::io::Error, what: &str) -> anyhow::Error {
    match error.kind() {
        std::io::ErrorKind::NotFound => {
            anyhow::anyhow!("git is not installed, so {what} cannot run. To add it: {GIT_INSTALL}")
        }
        _ => anyhow::Error::new(error).context(format!("run {what}")),
    }
}

/// True when `program` is an executable file in one of PATH's folders.
pub fn on_path(program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        fs::metadata(dir.join(program))
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    })
}

/// The words a renderer's error starts with when its command is missing; the app then
/// leaves that renderer out of the m cycle.
pub const NOT_INSTALLED: &str = "is not installed";

/// Below this many columns difftastic's two columns wrap nearly every line, so it draws
/// one column instead, unless DFT_DISPLAY is already set.
const SIDE_BY_SIDE_MIN_WIDTH: u16 = 100;

pub fn run_renderer(dir: &Path, command: &str, whole_file: bool, width: u16) -> Result<Vec<u8>> {
    let mut shell = Command::new("sh");
    shell.arg("-c").arg(command).current_dir(dir);
    if width < SIDE_BY_SIDE_MIN_WIDTH && std::env::var_os("DFT_DISPLAY").is_none() {
        shell.env("DFT_DISPLAY", "inline");
    }
    if whole_file {
        // git reads config from these variables; difftastic reads its own.
        shell
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "diff.context")
            .env("GIT_CONFIG_VALUE_0", WHOLE_FILE_CONTEXT)
            .env("DFT_CONTEXT", WHOLE_FILE_CONTEXT);
    }
    let output = shell.output().context("run the diff renderer")?;
    // Some diff tools exit 1 when files differ, so output counts as success.
    if output.status.success() || !output.stdout.is_empty() {
        return Ok(output.stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    match missing_program(&stderr) {
        // Every renderer needs git, so m has nothing to switch to.
        Some("git") => bail!("git is not installed, so no diff can show. To add it: {GIT_INSTALL}"),
        Some(program) => bail!(
            "{program} {NOT_INSTALLED}, so this renderer cannot run. m switches to another one."
        ),
        None => {}
    }
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    bail!(
        "renderer exited with {}:\n{}",
        output.status,
        lines.join("\n")
    )
}

/// The program the shell could not find, from "sh: delta: command not found" or git's
/// "difft --width=94: difft: command not found".
fn missing_program(stderr: &str) -> Option<&str> {
    let line = stderr
        .lines()
        .find(|line| line.contains("command not found"))?;
    let before = line.split(": command not found").next()?;
    before.rsplit([':', ' ']).find(|word| !word.is_empty())
}

/// The changed files whose diff holds `query`, ignoring case, each with the number of
/// lines that hold it, in git's order. It reads the plain diff once, so it covers files
/// no renderer has drawn yet.
pub fn files_with_text(
    dir: &Path,
    base: &str,
    head: &str,
    query: &str,
) -> Result<Vec<(String, usize)>> {
    let range = format!("{base}...{head}");
    let output = git(dir, &["diff", "--no-color", "--no-ext-diff", &range])?;
    ensure_success(&output, "git diff")?;
    let diff = String::from_utf8_lossy(&output.stdout);
    Ok(count_lines_with(&diff, &crate::find::Pattern::parse(query)))
}

/// The words of the PR's changed lines, the most used first, for the find prompt to
/// complete. Words shorter than four characters are left out; they type faster than
/// they complete.
/// One file's plain diff, which comment mode turns into numbered rows.
pub fn file_diff(dir: &Path, base: &str, head: &str, paths: &[String]) -> Result<String> {
    let range = format!("{base}...{head}");
    let mut args = vec!["diff", "--no-color", "--no-ext-diff", range.as_str(), "--"];
    args.extend(paths.iter().map(String::as_str));
    let output = git(dir, &args)?;
    ensure_success(&output, "git diff")?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn diff_words(dir: &Path, base: &str, head: &str) -> Result<Vec<String>> {
    let range = format!("{base}...{head}");
    let output = git(dir, &["diff", "--no-color", "--no-ext-diff", &range])?;
    ensure_success(&output, "git diff")?;
    let diff = String::from_utf8_lossy(&output.stdout);
    let mut words = changed_words(&diff);
    for name in file_names(&diff) {
        if !words.contains(&name) {
            words.push(name);
        }
    }
    Ok(words)
}

fn changed_words(diff: &str) -> Vec<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    let changed = diff.lines().filter(|line| {
        let header = line.starts_with("+++") || line.starts_with("---");
        !header && (line.starts_with('+') || line.starts_with('-'))
    });
    for line in changed {
        let words =
            line[1..].split(|character: char| !(character.is_alphanumeric() || character == '_'));
        for word in words.filter(|word| word.chars().count() >= 4) {
            *counts.entry(word).or_default() += 1;
        }
    }
    let mut words: Vec<(&str, usize)> = counts.into_iter().collect();
    words.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(right.0)));
    words
        .into_iter()
        .map(|(word, _)| word.to_string())
        .collect()
}

/// Each changed file's path, file name and folder names, so find completes them too.
/// They come after the code words: those hold no dot or slash, so typing one picks a
/// file name.
fn file_names(diff: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let paths = diff.lines().filter_map(|line| {
        let paths = line.strip_prefix("diff --git ")?;
        Some(paths.rsplit_once(" b/").map_or(paths, |(_, path)| path))
    });
    for path in paths {
        let pieces = std::iter::once(path).chain(path.split('/'));
        for piece in pieces.filter(|piece| piece.chars().count() >= 3) {
            if !names.iter().any(|name| name == piece) {
                names.push(piece.to_string());
            }
        }
    }
    names
}

fn count_lines_with(diff: &str, pattern: &crate::find::Pattern) -> Vec<(String, usize)> {
    let mut found: Vec<(String, usize)> = Vec::new();
    for line in diff.lines() {
        if let Some(paths) = line.strip_prefix("diff --git ") {
            let path = paths.rsplit_once(" b/").map_or(paths, |(_, path)| path);
            found.push((path.to_string(), 0));
        } else if pattern.matches_raw(line)
            && let Some((_, count)) = found.last_mut()
        {
            *count += 1;
        }
    }
    found.retain(|(_, count)| *count > 0);
    found
}

fn git(dir: &Path, args: &[&str]) -> Result<Output> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|error| git_spawn_error(error, &format!("git {}", args.first().unwrap_or(&""))))
}

fn ensure_success(output: &Output, what: &str) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!("{what} failed ({}): {}", output.status, stderr.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_give_the_path_the_name_and_each_folder() {
        let diff = "diff --git a/src/app/main.rs b/src/app/main.rs\n+x\ndiff --git a/src/lib.rs b/src/lib.rs\n";
        assert_eq!(
            file_names(diff),
            [
                "src/app/main.rs",
                "src",
                "app",
                "main.rs",
                "src/lib.rs",
                "lib.rs"
            ]
        );
    }

    #[test]
    fn changed_words_come_from_changed_lines_most_used_first() {
        let diff = "diff --git a/a.py b/a.py\n--- a/a.py\n+++ b/a.py\n@@ -1 +1 @@ header_word\n\
                    +invoice = make_invoice(invoice)\n-old_name\n unchanged_context\n";
        assert_eq!(changed_words(diff), ["invoice", "make_invoice", "old_name"]);
    }

    #[test]
    fn diff_matches_count_per_file_by_the_new_path() {
        let diff = "diff --git a/a.py b/a.py\n+Invoice one\n-invoice two\n x\n\
                    diff --git a/old.py b/new.py\n+nothing here\n\
                    diff --git a/c.py b/c.py\n+an INVOICE\n";
        assert_eq!(
            count_lines_with(diff, &crate::find::Pattern::parse("invoice")),
            [("a.py".to_string(), 2), ("c.py".to_string(), 1)]
        );
    }

    #[test]
    fn remotes_normalize_across_protocols() {
        for url in [
            "git@github.com:Acme/App.git",
            "https://github.com/acme/app",
            "https://github.com/acme/app.git/",
            "ssh://git@github.com/acme/app.git",
        ] {
            assert_eq!(
                github_repo_of_remote(url).as_deref(),
                Some("acme/app"),
                "{url}"
            );
        }
        assert_eq!(github_repo_of_remote("git@gitlab.com:acme/app.git"), None);
    }

    #[test]
    fn quoting_keeps_hostile_names_as_text() {
        let quoted = shell_quote("it's $(touch pwned) `x`.md");
        let output = Command::new("sh")
            .arg("-c")
            .arg(format!("printf %s {quoted}"))
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "it's $(touch pwned) `x`.md"
        );
    }

    #[test]
    fn template_fills_every_placeholder() {
        let command = fill_template(
            "git diff {base}...{head} -- {paths} --width={width}",
            &[
                ("base", shell_quote("b1")),
                ("head", shell_quote("h2")),
                (
                    "paths",
                    format!("{} {}", shell_quote("old name"), shell_quote("new")),
                ),
                ("width", "80".to_string()),
            ],
        );
        assert_eq!(
            command,
            "git diff 'b1'...'h2' -- 'old name' 'new' --width=80"
        );
    }

    #[test]
    fn name_status_maps_renames_new_to_old() {
        let output = b"M\0a.md\0R087\0old/b.md\0new/b.md\0A\0c.md\0C100\0x.md\0y.md\0";
        let renames = parse_renames(output);
        assert_eq!(renames.len(), 2);
        assert_eq!(renames["new/b.md"], "old/b.md");
        assert_eq!(renames["y.md"], "x.md");
    }

    #[test]
    fn home_expands_only_at_the_start() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(
            expand_home("~/Workspace"),
            PathBuf::from(&home).join("Workspace")
        );
        assert_eq!(expand_home("/tmp/~x"), PathBuf::from("/tmp/~x"));
    }

    #[test]
    fn the_whole_file_mode_reaches_git_and_difftastic() {
        let dir = std::env::temp_dir();
        let output = run_renderer(
            &dir,
            "git config diff.context; printf \"$DFT_CONTEXT\"",
            true,
            200,
        )
        .unwrap();
        assert_eq!(String::from_utf8(output).unwrap(), "1000000\n1000000");
    }

    #[test]
    fn a_file_without_bat_comes_back_numbered() {
        assert_eq!(numbered("a\nb\n"), b"1 a\n2 b\n");
    }

    #[test]
    fn a_missing_renderer_command_says_so_and_narrow_difftastic_goes_inline() {
        let dir = std::env::temp_dir();
        let error = run_renderer(&dir, "no-such-renderer-xyz", false, 200)
            .unwrap_err()
            .to_string();
        assert!(
            error.starts_with("no-such-renderer-xyz is not installed"),
            "{error}"
        );
        assert_eq!(
            missing_program("x --width=94: difft: command not found\nfatal: external diff died"),
            Some("difft")
        );
        let narrow = run_renderer(&dir, "printf \"$DFT_DISPLAY\"", false, 60).unwrap();
        if std::env::var_os("DFT_DISPLAY").is_none() {
            assert_eq!(narrow, b"inline");
        }
    }

    #[test]
    fn renderer_output_survives_a_nonzero_exit() {
        let dir = std::env::temp_dir();
        assert_eq!(
            run_renderer(&dir, "printf diff; exit 1", false, 200).unwrap(),
            b"diff"
        );
        assert!(
            run_renderer(&dir, "echo broken >&2; exit 2", false, 200)
                .unwrap_err()
                .to_string()
                .contains("broken")
        );
    }

    #[test]
    fn grep_record_splits_on_nul_and_keeps_colons_in_text() {
        let hit = parse_grep_record(b"abc:src/a.rs\x0012\x00let x: u8 = 1;", "abc:");
        let expected = GrepHit {
            path: "src/a.rs".to_string(),
            line: 12,
            text: "let x: u8 = 1;".to_string(),
        };
        assert_eq!(hit, Some(expected));
        assert_eq!(parse_grep_record(b"other:src/a.rs\x001\x00x", "abc:"), None);
    }

    #[test]
    fn grep_stops_at_the_hit_limit() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let head = String::from_utf8(git(dir, &["rev-parse", "HEAD"]).unwrap().stdout).unwrap();
        // An empty pattern matches every line, far more than the limit.
        assert_eq!(grep(dir, head.trim(), "").unwrap().len(), MAX_GREP_HITS);
    }

    #[test]
    fn grep_finds_text_in_a_real_commit_and_reports_no_match_as_empty() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let head = String::from_utf8(git(dir, &["rev-parse", "HEAD"]).unwrap().stdout).unwrap();
        let head = head.trim();
        let hits = grep(dir, head, "[package]").unwrap();
        assert!(hits.iter().any(|hit| hit.path == "Cargo.toml"));
        // Joined at run time, so the searched text never appears in this file at HEAD.
        let absent = ["zz", "no-such-text", "zz"].join("-");
        assert!(grep(dir, head, &absent).unwrap().is_empty());
        assert!(
            show_file(dir, head, "Cargo.toml")
                .unwrap()
                .contains("[package]")
        );
    }

    #[test]
    fn the_word_renderer_diffs_a_path_with_a_space_and_a_quote() {
        let dir = std::env::temp_dir().join(format!("revu-quote-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = "docs/it's a file.md";
        fs::create_dir_all(dir.join("docs")).unwrap();
        let run = |args: &[&str]| {
            let output = git(&dir, args).unwrap();
            ensure_success(&output, "git").unwrap();
            String::from_utf8(output.stdout).unwrap().trim().to_string()
        };
        run(&["init", "-q"]);
        let commit = [
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ];
        fs::write(dir.join(path), "the old words\n").unwrap();
        run(&["add", "."]);
        run(&[&commit[..], &["commit", "-q", "-m", "one"]].concat());
        let base = run(&["rev-parse", "HEAD"]);
        fs::write(dir.join(path), "the new words\n").unwrap();
        run(&[&commit[..], &["commit", "-q", "-am", "two"]].concat());
        let head = run(&["rev-parse", "HEAD"]);

        let template = "git diff --word-diff=plain {base}...{head} -- {paths}";
        let values = [
            ("base", shell_quote(&base)),
            ("head", shell_quote(&head)),
            ("paths", shell_quote(path)),
        ];
        let output = run_renderer(&dir, &fill_template(template, &values), false, 200).unwrap();
        let _ = fs::remove_dir_all(&dir);
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("[-old-]{+new+}")
        );
    }
}
