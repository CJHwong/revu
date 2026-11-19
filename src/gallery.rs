//! The image gallery: a full-screen view of the PR's images and mermaid diagrams, one at a
//! time. The terminal's graphics protocol draws them (kitty, iTerm2 or sixel); without one
//! they are drawn in colored half blocks.

use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use image::DynamicImage;
use ratatui::layout::{Rect, Size};
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::Protocol;
use ratatui_image::{FontSize, Resize};

use crate::markdown::Visual;

/// The largest image revu downloads.
const MAX_BYTES: &str = "25M";

pub struct Gallery {
    pub items: Vec<Visual>,
    pub at: usize,
    /// Each item seen since the gallery opened.
    pub loaded: HashMap<Visual, Loaded>,
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
    /// Opens at `start` when the list holds it, else at the first item. None when the
    /// PR shows no image or diagram.
    pub fn open(items: Vec<Visual>, start: Option<&Visual>) -> Option<Gallery> {
        if items.is_empty() {
            return None;
        }
        let at = start
            .and_then(|start| items.iter().position(|each| each == start))
            .unwrap_or(0);
        Some(Gallery {
            items,
            at,
            loaded: HashMap::new(),
        })
    }

    pub fn current(&self) -> &Visual {
        &self.items[self.at]
    }

    /// Moves one image on or back. It stops at either end.
    pub fn step(&mut self, forward: bool) {
        self.at = if forward {
            (self.at + 1).min(self.items.len() - 1)
        } else {
            self.at.saturating_sub(1)
        };
    }

    /// The current item when it still needs loading, marked as loading.
    pub fn start_load(&mut self) -> Option<Visual> {
        let item = self.current().clone();
        if self.loaded.contains_key(&item) {
            return None;
        }
        self.loaded.insert(item.clone(), Loaded::Loading);
        Some(item)
    }
}

/// The gallery's box: four fifths of the screen each way, centered, so the PR stays in
/// view around it.
pub fn modal_area(screen: Rect) -> Rect {
    let width = screen.width * 4 / 5;
    let height = screen.height * 4 / 5;
    Rect {
        x: screen.x + (screen.width - width) / 2,
        y: screen.y + (screen.height - height) / 2,
        width,
        height,
    }
}

/// Where the image goes in the gallery's box: inside the border, above the key line.
pub fn image_area(screen: Rect) -> Rect {
    let modal = modal_area(screen);
    Rect {
        x: modal.x + 1,
        y: modal.y + 1,
        width: modal.width.saturating_sub(2),
        height: modal.height.saturating_sub(3),
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

/// Fetches an image, or draws a diagram, and encodes it for `fit` cells. It runs on a
/// worker thread.
pub fn load(item: &Visual, picker: &Picker, fit: Size) -> Result<Shown, String> {
    let image = match item {
        Visual::Image(url) => {
            let bytes = fetch(url)?;
            image::load_from_memory(&bytes)
                .map_err(|error| format!("revu cannot show this image ({error})"))?
        }
        Visual::Diagram(source) => draw_diagram(source, pixels(picker, fit))?,
    };
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
    let (width, height) = pixels(picker, fit);
    if image.width() <= width && image.height() <= height {
        return image;
    }
    image.thumbnail(width.max(1), height.max(1))
}

/// The pixels `fit` cells cover.
fn pixels(picker: &Picker, fit: Size) -> (u32, u32) {
    let font = picker.font_size();
    (
        u32::from(fit.width) * u32::from(font.width),
        u32::from(fit.height) * u32::from(font.height),
    )
}

/// Lays out a mermaid diagram and paints it on white, as large as fits `space` pixels.
pub fn draw_diagram(source: &str, space: (u32, u32)) -> Result<DynamicImage, String> {
    let svg = mermaid_rs_renderer::render(source)
        .map_err(|error| format!("revu cannot draw this diagram ({error})"))?;
    let tree = resvg::usvg::Tree::from_str(&svg, &svg_options())
        .map_err(|error| format!("revu cannot draw this diagram ({error})"))?;
    let size = tree.size();
    let scale = (space.0 as f32 / size.width())
        .min(space.1 as f32 / size.height())
        .max(0.01);
    let width = (size.width() * scale).ceil().max(1.0) as u32;
    let height = (size.height() * scale).ceil().max(1.0) as u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)
        .ok_or("revu cannot draw this diagram (it has no size)")?;
    pixmap.fill(resvg::tiny_skia::Color::WHITE);
    let transform = resvg::tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let image = image::RgbaImage::from_raw(width, height, pixmap.take())
        .ok_or("revu cannot draw this diagram (wrong pixel count)")?;
    Ok(DynamicImage::ImageRgba8(image))
}

/// The system's fonts, read once, for the diagram's text.
fn svg_options() -> resvg::usvg::Options<'static> {
    static FONTS: OnceLock<std::sync::Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
    let fonts = FONTS.get_or_init(|| {
        let mut database = resvg::usvg::fontdb::Database::new();
        database.load_system_fonts();
        std::sync::Arc::new(database)
    });
    resvg::usvg::Options {
        fontdb: fonts.clone(),
        ..resvg::usvg::Options::default()
    }
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

    fn image(url: &str) -> Visual {
        Visual::Image(url.to_string())
    }

    fn items() -> Vec<Visual> {
        ["https://a/1.png", "https://a/2.png", "https://a/3.png"]
            .map(image)
            .to_vec()
    }

    #[test]
    fn the_gallery_opens_at_the_image_under_the_cursor_else_the_first() {
        let at = |start: Option<Visual>| Gallery::open(items(), start.as_ref()).unwrap().at;
        assert_eq!(at(Some(image("https://a/2.png"))), 1);
        assert_eq!(at(None), 0);
        assert_eq!(at(Some(image("https://elsewhere/x.png"))), 0);
        assert!(Gallery::open(Vec::new(), None).is_none());
    }

    #[test]
    fn stepping_stops_at_either_end() {
        let mut gallery = Gallery::open(items(), None).unwrap();
        gallery.step(false);
        assert_eq!(gallery.at, 0);
        gallery.step(true);
        gallery.step(true);
        gallery.step(true);
        assert_eq!(gallery.current(), &image("https://a/3.png"));
    }

    #[test]
    fn an_image_is_loaded_once() {
        let mut gallery = Gallery::open(items(), None).unwrap();
        assert_eq!(gallery.start_load(), Some(image("https://a/1.png")));
        assert_eq!(gallery.start_load(), None);
    }

    #[test]
    fn the_gallery_box_leaves_the_view_around_it() {
        let screen = Rect::new(0, 0, 200, 60);
        let modal = modal_area(screen);
        assert_eq!((modal.width, modal.height), (160, 48));
        assert_eq!((modal.x, modal.y), (20, 6), "centered");
        let image = image_area(screen);
        assert!(modal.contains(image.as_position()) && image.bottom() < modal.bottom());
        assert_eq!(
            (image.width, image.height),
            (158, 45),
            "inside the border, above the key line"
        );
    }

    #[test]
    fn a_diagram_is_drawn_to_fit_the_space() {
        let drawn = draw_diagram("graph TD\n  A[Start] --> B{Done?}\n", (400, 300)).unwrap();
        assert!(drawn.width() <= 400 && drawn.height() <= 300);
        assert!(
            drawn.width() == 400 || drawn.height() == 300,
            "the diagram grows until one side fills the space"
        );
        let error = draw_diagram("this is not mermaid", (400, 300)).unwrap_err();
        assert!(
            error.starts_with("revu cannot draw this diagram"),
            "{error}"
        );
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
