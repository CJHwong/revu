use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::theme::{self, Theme};

pub const DEFAULT_CONFIG: &str = r#"# rs-github-inbox config. Press r in the inbox to reload it.
# Every group query gets "is:pr is:open archived:false" added,
# plus "updated:>=<date>" when updated_within_days is set, and
# "sort:updated-desc" unless the query has its own sort:.
updated_within_days: 30

# Color theme, or press T in the app to pick one. dracula, terminal (your terminal's
# own colors), catppuccin_mocha, catppuccin_macchiato, catppuccin_frappe, nord,
# gruvbox_dark, solarized_dark, one_half_dark, monokai, and the light ones:
# catppuccin_latte, gruvbox_light, solarized_light, one_half_light, github.
theme: dracula

# Runs when you press d. {repo}, {number} and {url} are replaced.
open_diff: "gh pr diff {number} -R {repo} | delta"

# The folder that holds your clones, such as ~/Workspace. The Files changed tab asks
# for it the first time and adds it here.
# workspace_dir: ~/Workspace

# Diff views for the Files changed and Commits tabs; m switches between them.
# Each command runs in the repo's clone. {base} and {head} are commits, {paths} is
# the file (both names for a rename), {width} is the pane width. {syntax_theme},
# {added_bg} and {removed_bg} come from the app's theme, so a renderer that colors
# code itself can match the others. Values are quoted for the shell, so write
# {paths}, not "{paths}". highlight: true adds syntax colors from bat to a renderer
# that has none of its own. requires names the program a renderer needs: when it is
# not on PATH, m skips that renderer and the app says how to install it.
diff_renderers:
  - name: word
    command: git diff --color=always --word-diff=color {base}...{head} -- {paths}
    highlight: true
  - name: delta
    requires: delta
    install: brew install git-delta
    command: git diff {base}...{head} -- {paths} | delta --paging=never --width={width} --syntax-theme={syntax_theme} --plus-style="syntax "{added_bg} --plus-emph-style="syntax "{added_bg} --plus-non-emph-style=syntax --minus-style="syntax "{removed_bg} --minus-emph-style="syntax "{removed_bg} --minus-non-emph-style=syntax
  - name: difftastic
    requires: difft
    install: brew install difftastic
    command: GIT_EXTERNAL_DIFF="difft --color=always --width={width}" git diff --ext-diff {base}...{head} -- {paths}
    highlight: true

groups:
  - title: Needs your review
    query: user-review-requested:@me
  - title: Needs your teams' review
    query: review-requested:@me -user-review-requested:@me
    collapsed: true
  - title: Your drafts
    query: draft:true author:@me
    collapsed: true
  - title: Waiting for review or checks
    query: draft:false author:@me
  - title: Reviewed by you
    query: reviewed-by:@me -author:@me
    collapsed: true
"#;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub updated_within_days: Option<u32>,
    pub open_diff: String,
    pub groups: Vec<GroupConfig>,
    #[serde(default = "default_theme", deserialize_with = "theme_from_name")]
    pub theme: Theme,
    pub workspace_dir: Option<String>,
    #[serde(default = "default_renderers")]
    pub diff_renderers: Vec<Renderer>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Renderer {
    pub name: String,
    pub command: String,
    /// Add syntax colors from `bat` under the renderer's change colors.
    #[serde(default)]
    pub highlight: bool,
    /// The program the renderer needs; m skips the renderer when it is not on PATH.
    #[serde(default)]
    pub requires: Option<String>,
    /// How to install that program, for the warning.
    #[serde(default)]
    pub install: Option<String>,
}

/// The renderers in DEFAULT_CONFIG, for config files written before the key existed.
fn default_renderers() -> Vec<Renderer> {
    parse(DEFAULT_CONFIG)
        .map(|config| config.diff_renderers)
        .unwrap_or_default()
}

/// The folder as a quoted YAML string, so spaces and quotes in it survive.
pub fn quoted(text: &str) -> String {
    let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

pub fn save_theme(path: &Path, name: &str) -> Result<()> {
    save_setting(path, "theme", name)
}

/// Replaces the `key:` line, or adds one, and leaves every other line as it was, so the
/// user's comments survive. `value` is written as given; quote it with `quoted` when needed.
pub fn save_setting(path: &Path, key: &str, value: &str) -> Result<()> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let setting = format!("{key}: {value}");
    let prefix = format!("{key}:");
    let mut lines: Vec<&str> = text.lines().collect();
    match lines.iter().position(|line| line.starts_with(&prefix)) {
        Some(index) => lines[index] = &setting,
        None => lines.push(&setting),
    }
    let text = lines.join("\n") + "\n";
    parse(&text).context("the config would no longer parse")?;
    fs::write(path, text).with_context(|| format!("write {}", path.display()))
}

/// The view toggles the app brings back on the next start. The app writes them to
/// view.yaml next to the config, so config.yaml stays as the user wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewPrefs {
    pub newest_first: bool,
    pub no_bots: bool,
    pub diff_wrap: bool,
    /// The renderer's name, so a reordered renderer list keeps the choice.
    pub renderer: Option<String>,
    pub tree_view: bool,
    pub hide_viewed: bool,
    pub file_list_hidden: bool,
}

impl Default for ViewPrefs {
    fn default() -> Self {
        Self {
            newest_first: false,
            no_bots: false,
            diff_wrap: true,
            renderer: None,
            tree_view: true,
            hide_viewed: false,
            file_list_hidden: false,
        }
    }
}

pub fn prefs_path(config_path: &Path) -> PathBuf {
    config_path.with_file_name("view.yaml")
}

/// The saved toggles, or the defaults when the file does not exist yet.
pub fn load_prefs(path: &Path) -> Result<ViewPrefs> {
    if !path.exists() {
        return Ok(ViewPrefs::default());
    }
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_yaml::from_str(&text).with_context(|| format!("parse {}", path.display()))
}

pub fn save_prefs(path: &Path, prefs: &ViewPrefs) -> Result<()> {
    let text = format!(
        "# Written by rs-github-inbox when you change a view toggle.\n{}",
        serde_yaml::to_string(prefs)?
    );
    fs::write(path, text).with_context(|| format!("write {}", path.display()))
}

fn default_theme() -> Theme {
    theme::by_name("dracula").expect("dracula is a built-in theme")
}

fn theme_from_name<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Theme, D::Error> {
    let name = String::deserialize(deserializer)?;
    theme::by_name(&name).map_err(serde::de::Error::custom)
}

#[derive(Debug, Clone, Deserialize)]
pub struct GroupConfig {
    pub title: String,
    pub query: String,
    #[serde(default)]
    pub collapsed: bool,
}

pub fn default_path() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => {
            let home = std::env::var_os("HOME").context("HOME is not set")?;
            PathBuf::from(home).join(".config")
        }
    };
    Ok(base.join("rs-github-inbox").join("config.yaml"))
}

/// Writes the default config on first run, so the user has a file to edit.
pub fn load_or_init(path: &Path) -> Result<Config> {
    if !path.exists() {
        let dir = path
            .parent()
            .context("config path has no parent directory")?;
        fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        fs::write(path, DEFAULT_CONFIG).with_context(|| format!("write {}", path.display()))?;
    }
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    parse(&text).with_context(|| format!("parse {}", path.display()))
}

pub fn parse(text: &str) -> Result<Config> {
    Ok(serde_yaml::from_str(text)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_parses_into_five_groups() {
        let config = parse(DEFAULT_CONFIG).unwrap();
        assert_eq!(config.updated_within_days, Some(30));
        assert_eq!(config.groups.len(), 5);
        assert_eq!(config.groups[0].title, "Needs your review");
        assert!(!config.groups[0].collapsed);
        assert!(config.groups[1].collapsed);
    }

    #[test]
    fn theme_defaults_to_dracula_and_rejects_unknown_names() {
        let config = parse("open_diff: x\ngroups: []\n").unwrap();
        assert_eq!(config.theme, theme::by_name("dracula").unwrap());
        let error = parse("open_diff: x\ngroups: []\ntheme: nope\n").unwrap_err();
        assert!(format!("{error:#}").contains("unknown theme"));
    }

    #[test]
    fn missing_renderers_fall_back_to_the_defaults() {
        let config = parse("open_diff: x\ngroups: []\n").unwrap();
        let names: Vec<_> = config
            .diff_renderers
            .iter()
            .map(|renderer| renderer.name.as_str())
            .collect();
        assert_eq!(names, ["word", "delta", "difftastic"]);
    }

    #[test]
    fn saving_the_theme_replaces_its_line_or_adds_one() {
        let dir =
            std::env::temp_dir().join(format!("rs-github-inbox-theme-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");
        fs::write(&path, "# keep me\nopen_diff: x\ngroups: []").unwrap();
        save_theme(&path, "terminal").unwrap();
        save_theme(&path, "dracula").unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            "# keep me\nopen_diff: x\ngroups: []\ntheme: dracula\n"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn saving_the_workspace_twice_keeps_one_line_and_comments() {
        let dir = std::env::temp_dir().join(format!("rs-github-inbox-ws-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");
        fs::write(&path, "# keep me\nopen_diff: x\ngroups: []").unwrap();
        save_setting(&path, "workspace_dir", &quoted("~/Old")).unwrap();
        save_setting(&path, "workspace_dir", &quoted("~/My \"Work\"")).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# keep me\n"));
        assert_eq!(text.matches("workspace_dir:").count(), 1);
        assert_eq!(
            parse(&text).unwrap().workspace_dir.as_deref(),
            Some("~/My \"Work\"")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn view_prefs_round_trip_and_default_when_missing() {
        let dir =
            std::env::temp_dir().join(format!("rs-github-inbox-prefs-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = prefs_path(&dir.join("config.yaml"));
        assert_eq!(load_prefs(&path).unwrap(), ViewPrefs::default());
        let prefs = ViewPrefs {
            newest_first: true,
            no_bots: true,
            renderer: Some("delta".to_string()),
            ..ViewPrefs::default()
        };
        save_prefs(&path, &prefs).unwrap();
        assert_eq!(load_prefs(&path).unwrap(), prefs);
        fs::write(&path, "no_bots: true\n").unwrap();
        assert!(
            load_prefs(&path).unwrap().diff_wrap,
            "a missing key keeps its default"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn updated_within_days_is_optional() {
        let config = parse("open_diff: x\ngroups: []\n").unwrap();
        assert_eq!(config.updated_within_days, None);
    }

    #[test]
    fn first_run_writes_the_default_file() {
        let dir = std::env::temp_dir().join(format!("rs-github-inbox-test-{}", std::process::id()));
        let path = dir.join("nested").join("config.yaml");
        let config = load_or_init(&path).unwrap();
        assert_eq!(config.groups.len(), 5);
        assert_eq!(fs::read_to_string(&path).unwrap(), DEFAULT_CONFIG);
        fs::remove_dir_all(dir).unwrap();
    }
}
