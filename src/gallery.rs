//! The image gallery: a full-screen view of the PR's images, one at a time. The terminal's
//! graphics protocol draws them (kitty, iTerm2 or sixel); without one they are drawn in
//! colored half blocks.

use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use image::DynamicImage;
use ratatui::layout::{Rect, Size};
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::Protocol;
use ratatui_image::{FontSize, Resize};

/// The largest image revu downloads.
const MAX_BYTES: &str = "25M";

pub struct Gallery {
    pub urls: Vec<String>,
    pub at: usize,
    /// Each image seen since the gallery opened, by URL.
    pub loaded: HashMap<String, Loaded>,
}

pub enum Loaded {
    Loading,
    Ready(Shown),
    Failed(String),
}

/// An image scaled to the screen, and its encoding for the space it was drawn in.
pub struct Shown {
    pub image: DynamicImage,
    pub protocol: Protocol,
    /// The space the protocol was encoded for; a resized terminal encodes it again.
    pub fit: Size,
}

impl Gallery {
    /// Opens at `start` when the list holds it, else at the first image. None when the
    /// PR shows no image.
    pub fn open(urls: Vec<String>, start: Option<&str>) -> Option<Gallery> {
        if urls.is_empty() {
            return None;
        }
        let at = start
            .and_then(|url| urls.iter().position(|each| each == url))
            .unwrap_or(0);
        Some(Gallery {
            urls,
            at,
            loaded: HashMap::new(),
        })
    }

    pub fn current(&self) -> &str {
        &self.urls[self.at]
    }

    /// Moves one image on or back. It stops at either end.
    pub fn step(&mut self, forward: bool) {
        self.at = if forward {
            (self.at + 1).min(self.urls.len() - 1)
        } else {
            self.at.saturating_sub(1)
        };
    }

    /// The current image's URL when it still needs a fetch, marked as loading.
    pub fn start_load(&mut self) -> Option<String> {
        let url = self.current().to_string();
        if self.loaded.contains_key(&url) {
            return None;
        }
        self.loaded.insert(url.clone(), Loaded::Loading);
        Some(url)
    }
}

/// Where the image goes inside the full-screen gallery: inside the border, above the
/// key line.
pub fn image_area(screen: Rect) -> Rect {
    Rect {
        x: screen.x + 1,
        y: screen.y + 1,
        width: screen.width.saturating_sub(2),
        height: screen.height.saturating_sub(3),
    }
}

/// How this terminal draws images, from what it says about itself. revu does not ask the
/// terminal: where nothing answers, as in tmux, the question's reader is left waiting on
/// stdin and takes the user's next key.
pub fn picker() -> Picker {
    let term = std::env::var("TERM").unwrap_or_default();
    let program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    #[allow(deprecated)]
    let mut picker = Picker::from_fontsize(cell_size());
    let in_tmux = picker.tmux_detected() || std::env::var_os("TMUX").is_some();
    picker.set_protocol_type(protocol_for(&term, &program, in_tmux));
    picker
}

/// Ghostty and kitty draw kitty images. iTerm2 draws its own kind, and WezTerm draws
/// those better than kitty's. Inside tmux the outer terminal is out of reach, even when
/// tmux is set to name it in TERM.
fn protocol_for(term: &str, program: &str, in_tmux: bool) -> ProtocolType {
    if in_tmux {
        ProtocolType::Halfblocks
    } else if term.contains("kitty") || term.contains("ghostty") || program == "ghostty" {
        ProtocolType::Kitty
    } else if program == "iTerm.app" || program == "WezTerm" {
        ProtocolType::Iterm2
    } else {
        ProtocolType::Halfblocks
    }
}

/// A cell's size in pixels, from the terminal's size; 10 by 20 when it gives no pixels.
fn cell_size() -> FontSize {
    let fallback = FontSize::new(10, 20);
    let Ok(size) = crossterm::terminal::window_size() else {
        return fallback;
    };
    if size.width == 0 || size.height == 0 || size.columns == 0 || size.rows == 0 {
        return fallback;
    }
    FontSize::new(size.width / size.columns, size.height / size.rows)
}

/// GitHub's token goes only to GitHub's own hosts. An image on any other host is
/// fetched without it.
pub fn sends_token(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = host.rsplit('@').next().unwrap_or_default();
    let host = host
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    host == "github.com" || host.ends_with(".githubusercontent.com")
}

/// Fetches, decodes and encodes one image for `fit` cells. It runs on a worker thread.
pub fn load(url: &str, picker: &Picker, fit: Size) -> Result<Shown, String> {
    let bytes = fetch(url)?;
    let image = image::load_from_memory(&bytes)
        .map_err(|error| format!("revu cannot show this image ({error})"))?;
    let image = scale_to(image, picker, fit);
    let protocol = encode(&image, picker, fit)?;
    Ok(Shown {
        image,
        protocol,
        fit,
    })
}

/// Encodes the image again for a new space, after the terminal changed size.
pub fn refit(shown: &mut Shown, picker: &Picker, fit: Size) -> Result<(), String> {
    shown.protocol = encode(&shown.image, picker, fit)?;
    shown.fit = fit;
    Ok(())
}

fn encode(image: &DynamicImage, picker: &Picker, fit: Size) -> Result<Protocol, String> {
    picker
        .new_protocol(image.clone(), fit, Resize::Fit(None))
        .map_err(|error| format!("revu cannot draw this image ({error})"))
}

/// Shrinks a large image to the screen's pixels once, so it is not encoded at full size.
fn scale_to(image: DynamicImage, picker: &Picker, fit: Size) -> DynamicImage {
    let font = picker.font_size();
    let width = u32::from(fit.width) * u32::from(font.width);
    let height = u32::from(fit.height) * u32::from(font.height);
    if image.width() <= width && image.height() <= height {
        return image;
    }
    image.thumbnail(width.max(1), height.max(1))
}

/// Downloads with curl. The token goes in on stdin, so other processes cannot read it
/// from the command line, and curl drops it when the URL redirects to another host.
fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let token = sends_token(url).then(token).flatten();
    let mut command = Command::new("curl");
    command.args([
        "--silent",
        "--show-error",
        "--fail",
        "--location",
        "--max-time",
        "30",
        "--max-filesize",
        MAX_BYTES,
    ]);
    if token.is_some() {
        command.args(["--header", "@-"]);
    }
    command
        .arg("--")
        .arg(url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("curl did not start: {error}"))?;
    if let (Some(token), Some(mut stdin)) = (token, child.stdin.take()) {
        writeln!(stdin, "Authorization: token {token}")
            .map_err(|error| format!("curl did not take the token: {error}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("curl failed: {error}"))?;
    if !output.status.success() {
        let reason = String::from_utf8_lossy(&output.stderr);
        return Err(format!("download failed: {}", reason.trim()));
    }
    Ok(output.stdout)
}

/// gh's token, read once. Without it, GitHub's private images fail with 404.
fn token() -> Option<String> {
    static TOKEN: OnceLock<Option<String>> = OnceLock::new();
    TOKEN
        .get_or_init(|| {
            let output = Command::new("gh").args(["auth", "token"]).output().ok()?;
            let token = String::from_utf8(output.stdout).ok()?.trim().to_string();
            (output.status.success() && !token.is_empty()).then_some(token)
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls() -> Vec<String> {
        ["https://a/1.png", "https://a/2.png", "https://a/3.png"]
            .map(String::from)
            .to_vec()
    }

    #[test]
    fn the_gallery_opens_at_the_image_under_the_cursor_else_the_first() {
        let at = |start| Gallery::open(urls(), start).unwrap().at;
        assert_eq!(at(Some("https://a/2.png")), 1);
        assert_eq!(at(None), 0);
        assert_eq!(at(Some("https://elsewhere/x.png")), 0);
        assert!(Gallery::open(Vec::new(), None).is_none());
    }

    #[test]
    fn stepping_stops_at_either_end() {
        let mut gallery = Gallery::open(urls(), None).unwrap();
        gallery.step(false);
        assert_eq!(gallery.at, 0);
        gallery.step(true);
        gallery.step(true);
        gallery.step(true);
        assert_eq!(gallery.current(), "https://a/3.png");
    }

    #[test]
    fn an_image_is_fetched_once() {
        let mut gallery = Gallery::open(urls(), None).unwrap();
        assert_eq!(gallery.start_load().as_deref(), Some("https://a/1.png"));
        assert_eq!(gallery.start_load(), None);
    }

    #[test]
    fn the_terminal_names_its_image_protocol() {
        let kitty = ProtocolType::Kitty;
        let iterm2 = ProtocolType::Iterm2;
        let blocks = ProtocolType::Halfblocks;
        assert_eq!(protocol_for("xterm-ghostty", "ghostty", false), kitty);
        assert_eq!(
            protocol_for("xterm-ghostty", "", false),
            kitty,
            "over ssh only TERM comes"
        );
        assert_eq!(protocol_for("xterm-kitty", "", false), kitty);
        assert_eq!(protocol_for("xterm-256color", "iTerm.app", false), iterm2);
        assert_eq!(protocol_for("xterm-256color", "WezTerm", false), iterm2);
        assert_eq!(protocol_for("tmux-256color", "tmux", true), blocks);
        assert_eq!(
            protocol_for("xterm-ghostty", "ghostty", true),
            blocks,
            "tmux set to TERM xterm-ghostty still cannot pass kitty images"
        );
        assert_eq!(
            protocol_for("xterm-256color", "Apple_Terminal", false),
            blocks
        );
    }

    #[test]
    fn the_token_goes_only_to_github_hosts() {
        assert!(sends_token(
            "https://github.com/user-attachments/assets/88f601c1"
        ));
        assert!(sends_token(
            "https://private-user-images.githubusercontent.com/1/a.png?jwt=x"
        ));
        assert!(sends_token("https://GitHub.com:443/o/r/raw/main/a.png"));
        assert!(!sends_token("https://i.imgur.com/a.png"));
        assert!(!sends_token("https://github.com.evil.example/a.png"));
        assert!(!sends_token("https://evil.example/github.com/a.png"));
        assert!(!sends_token("https://github.com@evil.example/a.png"));
        assert!(
            !sends_token("http://github.com/a.png"),
            "never over plain http"
        );
        assert!(!sends_token("https://notgithubusercontent.com/a.png"));
    }
}
