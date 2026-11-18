//! Syntax themes revu ships for `bat` and `delta`, for app themes neither tool has.
//! revu builds them into a `bat` cache of its own and points `bat` and the diff
//! renderers at it, so the user's own `bat` cache is never touched.

use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

/// The themes revu ships: the name `bat --theme` takes, and the TextMate theme file.
const SHIPPED: [(&str, &str); 1] = [(
    "JetBrains Dark",
    include_str!("../assets/JetBrains Dark.tmTheme"),
)];

/// The cache folder for the theme in use, when revu ships it; None for the themes
/// `bat` has itself, which read the user's own cache.
static ACTIVE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Builds the cache the theme needs, if revu ships it, and makes the commands that
/// follow use it. A theme `bat` has itself needs nothing.
pub fn use_theme(bat_theme: &str) -> Result<()> {
    let cache = match shipped(bat_theme) {
        Some(_) => Some(prepare(&default_root()?)?),
        None => None,
    };
    *ACTIVE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = cache;
    Ok(())
}

/// Points `bat`, or a renderer that runs `bat` or `delta`, at revu's cache while a
/// shipped theme is in use. `bat` reads BAT_CACHE_PATH; `delta` reads XDG_CACHE_HOME.
pub fn apply(command: &mut Command) {
    let active = ACTIVE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(cache) = active.as_ref() {
        command.env("BAT_CACHE_PATH", cache.join("bat"));
        command.env("XDG_CACHE_HOME", cache);
    }
}

fn shipped(bat_theme: &str) -> Option<&'static str> {
    SHIPPED
        .iter()
        .find(|(name, _)| *name == bat_theme)
        .map(|(_, text)| *text)
}

/// $XDG_CACHE_HOME/revu/syntax, else ~/.cache/revu/syntax, where `bat` keeps its own.
fn default_root() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_CACHE_HOME").filter(|value| !value.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".cache"),
    };
    Ok(base.join("revu").join("syntax"))
}

/// Writes the shipped themes under `root` and builds `bat`'s cache from them, unless
/// a build for the same themes and the same `bat` is there already. Returns the folder
/// that holds the cache as `bat/`.
fn prepare(root: &Path) -> Result<PathBuf> {
    let cache = root.join("xdg");
    let stamp_path = root.join("stamp");
    let stamp = format!("{}\n{}", bat_version()?, themes_text());
    let built = cache.join("bat").join("themes.bin").is_file();
    if built && fs::read_to_string(&stamp_path).is_ok_and(|old| old == stamp) {
        return Ok(cache);
    }
    let themes = root.join("source").join("themes");
    fs::create_dir_all(&themes).with_context(|| format!("create {}", themes.display()))?;
    for (name, text) in SHIPPED {
        let path = themes.join(format!("{name}.tmTheme"));
        fs::write(&path, text).with_context(|| format!("write {}", path.display()))?;
    }
    let target = cache.join("bat");
    fs::create_dir_all(&target).with_context(|| format!("create {}", target.display()))?;
    // Run inside the source folder: bat reads "cache" as a file name when the current
    // folder holds one.
    let output = Command::new("bat")
        .args(["cache", "--build", "--source"])
        .arg(root.join("source"))
        .arg("--target")
        .arg(&target)
        .current_dir(root.join("source"))
        .output()
        .context("run bat cache --build")?;
    if !output.status.success() {
        bail!(
            "bat cache --build failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    fs::write(&stamp_path, stamp).with_context(|| format!("write {}", stamp_path.display()))?;
    Ok(cache)
}

/// A cache built by another `bat` may not load, so a new `bat` rebuilds it.
fn bat_version() -> Result<String> {
    let output = Command::new("bat")
        .arg("--version")
        .output()
        .context("run bat --version")?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn themes_text() -> String {
    SHIPPED.iter().map(|(_, text)| *text).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bat_installed() -> bool {
        Command::new("bat").arg("--version").output().is_ok()
    }

    #[test]
    fn a_shipped_theme_builds_a_cache_bat_and_delta_both_read() {
        if !bat_installed() {
            return;
        }
        let root = std::env::temp_dir().join(format!("revu-syntax-{}", std::process::id()));
        let cache = prepare(&root).unwrap();
        let list = |program: &str, flag: &str| {
            let output = Command::new(program)
                .arg(flag)
                .env("BAT_CACHE_PATH", cache.join("bat"))
                .env("XDG_CACHE_HOME", &cache)
                .output();
            output.map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        };
        let bat = list("bat", "--list-themes").unwrap();
        assert!(
            bat.lines().any(|line| line.trim() == "JetBrains Dark"),
            "{bat}"
        );
        if let Ok(delta) = list("delta", "--list-syntax-themes") {
            assert!(delta.contains("JetBrains Dark"), "{delta}");
        }
        let built = fs::metadata(cache.join("bat").join("themes.bin")).unwrap();
        prepare(&root).unwrap();
        let again = fs::metadata(cache.join("bat").join("themes.bin")).unwrap();
        assert_eq!(
            built.modified().unwrap(),
            again.modified().unwrap(),
            "the same themes and bat build once"
        );
    }

    #[test]
    fn a_theme_bat_has_needs_no_cache() {
        assert!(shipped("Dracula").is_none());
        assert!(shipped("JetBrains Dark").is_some());
    }
}
