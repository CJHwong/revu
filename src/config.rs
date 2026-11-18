use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::theme::{self, Theme};

/// The top of the first-run config: the settings that have no built-in value to fall
/// back on, written live.
const CONFIG_HEAD: &str = r#"# revu config. Press r in the inbox to reload it.
# Every group query gets "is:pr is:open archived:false" added,
# plus "updated:>=<date>" when updated_within_days is set, and
# "sort:updated-desc" unless the query has its own sort:.
updated_within_days: 30

# Color theme, or press T in the app to pick one. dracula, terminal (your terminal's
# own colors), catppuccin_mocha, catppuccin_macchiato, catppuccin_frappe, nord,
# gruvbox_dark, solarized_dark, one_half_dark, monokai, and the light ones:
# catppuccin_latte, gruvbox_light, solarized_light, one_half_light, github.
theme: dracula

# The folders that hold your clones. The Files changed tab asks for one the first
# time and adds it here. revu searches a list in order, and clones into the first.
# workspace_dir: ~/Workspace
# workspace_dir: [~/Workspace, ~/Workspace/side_projects]

# Everything below is revu's built-in defaults, shown commented out. While a key
# stays commented, revu uses its current default, so a new release can improve it.
# To set your own, uncomment the key and edit it; it then replaces that default.

"#;

/// The built-in open_diff, diff renderers and inbox groups. A config without one of
/// these keys uses the one here.
const DEFAULTS: &str = r#"# Runs when you press d. {repo}, {number} and {url} are replaced. Without delta,
# the diff opens in less.
open_diff: "if command -v delta >/dev/null; then gh pr diff {number} -R {repo} | delta; else gh pr diff {number} -R {repo} --color=always | less -R; fi"

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

/// The config the first run writes: the head live, then the defaults commented out
/// line by line, so the file always matches the defaults it describes.
pub fn default_config_file() -> String {
    let defaults: String = DEFAULTS
        .lines()
        .map(|line| match line {
            "" => "\n".to_string(),
            comment if comment.starts_with('#') => format!("{comment}\n"),
            setting => format!("# {setting}\n"),
        })
        .collect();
    format!("{CONFIG_HEAD}{defaults}")
}

fn defaults() -> Config {
    parse(DEFAULTS).expect("the built-in defaults parse")
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub updated_within_days: Option<u32>,
    #[serde(default = "default_open_diff")]
    pub open_diff: String,
    #[serde(default = "default_groups")]
    pub groups: Vec<GroupConfig>,
    #[serde(default = "default_theme", deserialize_with = "theme_from_name")]
    pub theme: Theme,
    /// Empty when the config has no workspace_dir.
    #[serde(rename = "workspace_dir", default, deserialize_with = "one_or_many")]
    pub workspace_dirs: Vec<String>,
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

pub fn default_renderers() -> Vec<Renderer> {
    defaults().diff_renderers
}

fn default_open_diff() -> String {
    defaults().open_diff
}

fn default_groups() -> Vec<GroupConfig> {
    defaults().groups
}

/// The folder as a quoted YAML string, so spaces and quotes in it survive.
/// workspace_dir is one folder in older configs, and may be a list now.
fn one_or_many<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match Option::<OneOrMany>::deserialize(deserializer)? {
        None => Vec::new(),
        Some(OneOrMany::One(folder)) => vec![folder],
        Some(OneOrMany::Many(folders)) => folders,
    })
}

/// The folders as typed in the settings prompt, separated by commas.
pub fn split_folders(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|folder| !folder.is_empty())
        .map(str::to_string)
        .collect()
}

/// A plain value for one folder, so older configs keep their shape; a one-line list
/// for more.
pub fn workspace_value(folders: &[String]) -> String {
    match folders {
        [folder] => quoted(folder),
        _ => format!(
            "[{}]",
            folders
                .iter()
                .map(|folder| quoted(folder))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

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
        Some(index) => {
            // A value written as a block, such as a list, continues on indented lines.
            let rest = &lines[index + 1..];
            let block = rest
                .iter()
                .take_while(|line| line.starts_with([' ', '\t', '-']))
                .count();
            lines.splice(index..=index + block, [setting.as_str()]);
        }
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

/// The file's toggles with only the ones this revu changed, from `before` to `after`,
/// laid over them. Another revu may have saved other toggles since this one loaded the
/// file, and writing all of ours would undo them.
pub fn changed_prefs(on_disk: &ViewPrefs, before: &ViewPrefs, after: &ViewPrefs) -> ViewPrefs {
    fn pick<T: PartialEq + Clone>(disk: &T, before: &T, after: &T) -> T {
        if after == before { disk } else { after }.clone()
    }
    ViewPrefs {
        newest_first: pick(
            &on_disk.newest_first,
            &before.newest_first,
            &after.newest_first,
        ),
        no_bots: pick(&on_disk.no_bots, &before.no_bots, &after.no_bots),
        diff_wrap: pick(&on_disk.diff_wrap, &before.diff_wrap, &after.diff_wrap),
        renderer: pick(&on_disk.renderer, &before.renderer, &after.renderer),
        tree_view: pick(&on_disk.tree_view, &before.tree_view, &after.tree_view),
        hide_viewed: pick(
            &on_disk.hide_viewed,
            &before.hide_viewed,
            &after.hide_viewed,
        ),
        file_list_hidden: pick(
            &on_disk.file_list_hidden,
            &before.file_list_hidden,
            &after.file_list_hidden,
        ),
    }
}

pub fn save_prefs(path: &Path, prefs: &ViewPrefs) -> Result<()> {
    let text = format!(
        "# Written by revu when you change a view toggle.\n{}",
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
    Ok(base.join(APP_DIR).join("config.yaml"))
}

const APP_DIR: &str = "revu";
/// The folder the app used before it was named revu.
const OLD_APP_DIR: &str = "rs-github-inbox";

/// Moves the config folder from the app's old name to its new one, once, so the
/// config and the saved view toggles carry over. Returns true when it moved.
pub fn move_old_dir(config_path: &Path) -> Result<bool> {
    let Some(dir) = config_path.parent() else {
        return Ok(false);
    };
    let old = dir.with_file_name(OLD_APP_DIR);
    if dir.exists() || !old.is_dir() {
        return Ok(false);
    }
    fs::rename(&old, dir)
        .with_context(|| format!("move {} to {}", old.display(), dir.display()))?;
    Ok(true)
}

/// Writes the default config on first run, so the user has a file to edit.
pub fn load_or_init(path: &Path) -> Result<Config> {
    if !path.exists() {
        let dir = path
            .parent()
            .context("config path has no parent directory")?;
        fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        fs::write(path, default_config_file())
            .with_context(|| format!("write {}", path.display()))?;
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
    fn the_old_config_folder_moves_to_the_new_name_once() {
        let base = std::env::temp_dir().join(format!("revu-move-{}", std::process::id()));
        let old = base.join(OLD_APP_DIR);
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("view.yaml"), "renderer: difftastic\n").unwrap();
        let path = base.join(APP_DIR).join("config.yaml");
        assert!(move_old_dir(&path).unwrap());
        assert_eq!(
            fs::read_to_string(base.join(APP_DIR).join("view.yaml")).unwrap(),
            "renderer: difftastic\n"
        );
        assert!(!old.exists());
        fs::create_dir_all(&old).unwrap();
        assert!(
            !move_old_dir(&path).unwrap(),
            "the new folder wins once it exists"
        );
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn missing_keys_use_the_built_in_defaults_and_set_ones_replace_them() {
        let config = parse("theme: dracula\n").unwrap();
        assert_eq!(config.groups.len(), 5, "no groups key, the built-in groups");
        assert!(
            config.open_diff.contains("| delta;"),
            "{}",
            config.open_diff
        );
        assert!(
            config.open_diff.contains("| less -R"),
            "no delta, still a diff"
        );
        assert_eq!(config.diff_renderers.len(), 3);
        let own = parse("groups:\n  - title: Mine\n    query: author:@me\n").unwrap();
        assert_eq!(
            own.groups.len(),
            1,
            "a list of your own replaces the defaults"
        );
        assert!(
            parse("groups: []\n").unwrap().groups.is_empty(),
            "an empty list stays empty"
        );
    }

    #[test]
    fn the_first_run_file_shows_the_defaults_commented_out() {
        let text = default_config_file();
        for key in ["groups:", "open_diff:", "diff_renderers:"] {
            assert!(
                !text.lines().any(|line| line.starts_with(key)),
                "{key} is not live"
            );
            assert!(
                text.contains(&format!("# {key}")),
                "{key} shows as a comment"
            );
        }
        // Uncomment the settings, as a user would, and leave the explanations.
        let settings: Vec<&str> = DEFAULTS
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .collect();
        let uncommented: String = text
            .lines()
            .map(|line| match line.strip_prefix("# ") {
                Some(setting) if settings.contains(&setting) => setting,
                _ => line,
            })
            .map(|line| format!("{line}\n"))
            .collect();
        let config = parse(&uncommented).unwrap();
        assert_eq!(
            config.groups.len(),
            5,
            "uncommenting gives the defaults back"
        );
        assert_eq!(config.diff_renderers.len(), 3);
    }

    #[test]
    fn default_config_parses_into_five_groups() {
        let config = parse(&default_config_file()).unwrap();
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
        let dir = std::env::temp_dir().join(format!("revu-theme-{}", std::process::id()));
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
        let dir = std::env::temp_dir().join(format!("revu-ws-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");
        fs::write(&path, "# keep me\nopen_diff: x\ngroups: []").unwrap();
        save_setting(&path, "workspace_dir", &quoted("~/Old")).unwrap();
        save_setting(&path, "workspace_dir", &quoted("~/My \"Work\"")).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# keep me\n"));
        assert_eq!(text.matches("workspace_dir:").count(), 1);
        assert_eq!(parse(&text).unwrap().workspace_dirs, ["~/My \"Work\""]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn workspace_dir_takes_one_folder_or_a_list() {
        let base = "open_diff: x\ngroups: []\n";
        assert!(parse(base).unwrap().workspace_dirs.is_empty());
        let one = parse(&format!("{base}workspace_dir: ~/Work\n")).unwrap();
        assert_eq!(one.workspace_dirs, ["~/Work"]);
        let list = format!("{base}workspace_dir:\n  - ~/Work\n  - ~/Work/side\n");
        assert_eq!(
            parse(&list).unwrap().workspace_dirs,
            ["~/Work", "~/Work/side"]
        );
    }

    #[test]
    fn workspace_folders_save_as_one_line_and_read_back() {
        let one = ["~/Work".to_string()];
        assert_eq!(workspace_value(&one), "\"~/Work\"");
        let two = ["~/Work".to_string(), "~/My \"Side\"".to_string()];
        let line = format!(
            "open_diff: x\ngroups: []\nworkspace_dir: {}\n",
            workspace_value(&two)
        );
        assert_eq!(parse(&line).unwrap().workspace_dirs, two);
        assert_eq!(
            split_folders(" ~/Work, ~/Work/side ,, "),
            ["~/Work", "~/Work/side"]
        );
    }

    #[test]
    fn saving_a_setting_replaces_the_list_written_under_it() {
        let dir = std::env::temp_dir().join(format!("revu-list-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");
        let text = "open_diff: x\ngroups: []\nworkspace_dir:\n  - ~/A\n  - ~/B\n# keep me\ntheme: dracula\n";
        fs::write(&path, text).unwrap();
        save_setting(&path, "workspace_dir", "[\"~/C\"]").unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "open_diff: x\ngroups: []\nworkspace_dir: [\"~/C\"]\n# keep me\ntheme: dracula\n"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_save_keeps_toggles_another_revu_changed_since_this_one_loaded() {
        let loaded = ViewPrefs::default();
        // Another revu switched the renderer after this one started.
        let on_disk = ViewPrefs {
            renderer: Some("delta".to_string()),
            ..loaded.clone()
        };
        // This one only hid the bots.
        let changed = ViewPrefs {
            no_bots: true,
            ..loaded.clone()
        };
        assert_eq!(
            changed_prefs(&on_disk, &loaded, &changed),
            ViewPrefs {
                no_bots: true,
                renderer: Some("delta".to_string()),
                ..loaded
            }
        );
    }

    #[test]
    fn view_prefs_round_trip_and_default_when_missing() {
        let dir = std::env::temp_dir().join(format!("revu-prefs-{}", std::process::id()));
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
    fn updated_within_days_saved_as_null_means_no_limit() {
        let dir = std::env::temp_dir().join(format!("revu-null-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");
        fs::write(&path, "# mine\nupdated_within_days: 30\ngroups: []\n").unwrap();
        save_setting(&path, "updated_within_days", "null").unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# mine\n"), "{text}");
        assert_eq!(parse(&text).unwrap().updated_within_days, None);
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn updated_within_days_is_optional() {
        let config = parse("open_diff: x\ngroups: []\n").unwrap();
        assert_eq!(config.updated_within_days, None);
    }

    #[test]
    fn first_run_writes_the_default_file() {
        let dir = std::env::temp_dir().join(format!("revu-test-{}", std::process::id()));
        let path = dir.join("nested").join("config.yaml");
        let config = load_or_init(&path).unwrap();
        assert_eq!(config.groups.len(), 5);
        assert_eq!(fs::read_to_string(&path).unwrap(), default_config_file());
        fs::remove_dir_all(dir).unwrap();
    }
}
