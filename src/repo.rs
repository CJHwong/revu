//! Local clones: finding the user's clone of a repo, fetching a PR into it without
//! touching the working tree, and running diff renderers inside it.

use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Where the PR's commits are kept in the user's clone. Nothing else writes here.
const REF_NAMESPACE: &str = "refs/rs-github-inbox/pr";

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

/// Runs a renderer command inside the clone and returns its colored output.
pub fn run_renderer(dir: &Path, command: &str) -> Result<Vec<u8>> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(dir)
        .output()
        .context("run the diff renderer")?;
    // Some diff tools exit 1 when files differ, so output counts as success.
    if output.status.success() || !output.stdout.is_empty() {
        return Ok(output.stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!("renderer exited with {}: {}", output.status, stderr.trim())
}

fn git(dir: &Path, args: &[&str]) -> Result<Output> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .with_context(|| format!("run git {}", args.first().unwrap_or(&"")))
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
    fn renderer_output_survives_a_nonzero_exit() {
        let dir = std::env::temp_dir();
        assert_eq!(run_renderer(&dir, "printf diff; exit 1").unwrap(), b"diff");
        assert!(
            run_renderer(&dir, "echo broken >&2; exit 2")
                .unwrap_err()
                .to_string()
                .contains("broken")
        );
    }
}
