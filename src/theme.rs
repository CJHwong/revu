use anyhow::{Result, bail};
use ratatui::style::Color;

/// Colors by role, so the UI never names a palette color directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub background: Color,
    pub foreground: Color,
    /// Secondary text: repo names, timestamps, help.
    pub muted: Color,
    /// Background of the selected row.
    pub selection: Color,
    pub heading: Color,
    pub accent: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    /// Background of a `<details>` summary bar.
    pub fold: Color,
}

pub const THEME_NAMES: [&str; 2] = ["dracula", "terminal"];

/// https://draculatheme.com/contribute#color-palette
const DRACULA: Theme = Theme {
    background: Color::Rgb(0x28, 0x2a, 0x36),
    foreground: Color::Rgb(0xf8, 0xf8, 0xf2),
    muted: Color::Rgb(0x62, 0x72, 0xa4),
    selection: Color::Rgb(0x44, 0x47, 0x5a),
    heading: Color::Rgb(0xbd, 0x93, 0xf9),
    accent: Color::Rgb(0x8b, 0xe9, 0xfd),
    success: Color::Rgb(0x50, 0xfa, 0x7b),
    warning: Color::Rgb(0xf1, 0xfa, 0x8c),
    danger: Color::Rgb(0xff, 0x55, 0x55),
    fold: Color::Rgb(0xbd, 0x93, 0xf9),
};

/// Uses the terminal's own palette and background.
const TERMINAL: Theme = Theme {
    background: Color::Reset,
    foreground: Color::Reset,
    muted: Color::DarkGray,
    selection: Color::Rgb(0x3a, 0x3d, 0x4d),
    heading: Color::Reset,
    accent: Color::Cyan,
    success: Color::Green,
    warning: Color::Yellow,
    danger: Color::Red,
    fold: Color::Magenta,
};

pub fn by_name(name: &str) -> Result<Theme> {
    match name {
        "dracula" => Ok(DRACULA),
        "terminal" => Ok(TERMINAL),
        _ => bail!(
            "unknown theme \"{name}\"; choose one of: {}",
            THEME_NAMES.join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_name_resolves() {
        for name in THEME_NAMES {
            assert!(by_name(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn unknown_name_lists_the_choices() {
        let error = by_name("solarized").unwrap_err().to_string();
        assert!(error.contains("dracula, terminal"));
    }
}
