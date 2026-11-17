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
    /// The `bat` theme that matches, for syntax colors in the file view.
    pub bat_theme: &'static str,
    /// Backgrounds for added and removed text when `bat` colors a diff's code.
    pub added_bg: Color,
    pub removed_bg: Color,
}

/// The built-in themes in menu order: the terminal's own colors, then dark and light
/// themes, each A to Z.
const THEMES: [(&str, Theme); 15] = [
    ("terminal", TERMINAL),
    ("catppuccin_frappe", CATPPUCCIN_FRAPPE),
    ("catppuccin_macchiato", CATPPUCCIN_MACCHIATO),
    ("catppuccin_mocha", CATPPUCCIN_MOCHA),
    ("dracula", DRACULA),
    ("gruvbox_dark", GRUVBOX_DARK),
    ("monokai", MONOKAI),
    ("nord", NORD),
    ("one_half_dark", ONE_HALF_DARK),
    ("solarized_dark", SOLARIZED_DARK),
    ("catppuccin_latte", CATPPUCCIN_LATTE),
    ("github", GITHUB),
    ("gruvbox_light", GRUVBOX_LIGHT),
    ("one_half_light", ONE_HALF_LIGHT),
    ("solarized_light", SOLARIZED_LIGHT),
];

pub const THEME_NAMES: [&str; THEMES.len()] = theme_names();

const fn theme_names() -> [&'static str; THEMES.len()] {
    let mut names = [""; THEMES.len()];
    let mut index = 0;
    while index < THEMES.len() {
        names[index] = THEMES[index].0;
        index += 1;
    }
    names
}

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
    bat_theme: "Dracula",
    added_bg: Color::Rgb(0x1f, 0x3d, 0x2b),
    removed_bg: Color::Rgb(0x4d, 0x1f, 0x29),
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
    bat_theme: "ansi",
    added_bg: Color::Indexed(22),
    removed_bg: Color::Indexed(52),
};

const CATPPUCCIN_MOCHA: Theme = Theme {
    background: Color::Rgb(0x1e, 0x1e, 0x2e),
    foreground: Color::Rgb(0xcd, 0xd6, 0xf4),
    muted: Color::Rgb(0x7f, 0x84, 0x9c),
    selection: Color::Rgb(0x45, 0x47, 0x5a),
    heading: Color::Rgb(0xcb, 0xa6, 0xf7),
    accent: Color::Rgb(0x89, 0xdc, 0xeb),
    success: Color::Rgb(0xa6, 0xe3, 0xa1),
    warning: Color::Rgb(0xf9, 0xe2, 0xaf),
    danger: Color::Rgb(0xf3, 0x8b, 0xa8),
    fold: Color::Rgb(0xcb, 0xa6, 0xf7),
    bat_theme: "Catppuccin Mocha",
    added_bg: Color::Rgb(0x3c, 0x49, 0x47),
    removed_bg: Color::Rgb(0x4d, 0x36, 0x49),
};

const CATPPUCCIN_MACCHIATO: Theme = Theme {
    background: Color::Rgb(0x24, 0x27, 0x3a),
    foreground: Color::Rgb(0xca, 0xd3, 0xf5),
    muted: Color::Rgb(0x80, 0x87, 0xa2),
    selection: Color::Rgb(0x49, 0x4d, 0x64),
    heading: Color::Rgb(0xc6, 0xa0, 0xf6),
    accent: Color::Rgb(0x91, 0xd7, 0xe3),
    success: Color::Rgb(0xa6, 0xda, 0x95),
    warning: Color::Rgb(0xee, 0xd4, 0x9f),
    danger: Color::Rgb(0xed, 0x87, 0x96),
    fold: Color::Rgb(0xc6, 0xa0, 0xf6),
    bat_theme: "Catppuccin Macchiato",
    added_bg: Color::Rgb(0x41, 0x4e, 0x4e),
    removed_bg: Color::Rgb(0x50, 0x3c, 0x4e),
};

const CATPPUCCIN_FRAPPE: Theme = Theme {
    background: Color::Rgb(0x30, 0x34, 0x46),
    foreground: Color::Rgb(0xc6, 0xd0, 0xf5),
    muted: Color::Rgb(0x83, 0x8b, 0xa7),
    selection: Color::Rgb(0x51, 0x57, 0x6d),
    heading: Color::Rgb(0xca, 0x9e, 0xe6),
    accent: Color::Rgb(0x99, 0xd1, 0xdb),
    success: Color::Rgb(0xa6, 0xd1, 0x89),
    warning: Color::Rgb(0xe5, 0xc8, 0x90),
    danger: Color::Rgb(0xe7, 0x82, 0x84),
    fold: Color::Rgb(0xca, 0x9e, 0xe6),
    bat_theme: "Catppuccin Frappe",
    added_bg: Color::Rgb(0x4a, 0x57, 0x55),
    removed_bg: Color::Rgb(0x58, 0x45, 0x54),
};

const CATPPUCCIN_LATTE: Theme = Theme {
    background: Color::Rgb(0xdc, 0xe0, 0xe8),
    foreground: Color::Rgb(0x4c, 0x4f, 0x69),
    muted: Color::Rgb(0x70, 0x73, 0x85),
    selection: Color::Rgb(0xbc, 0xc0, 0xcc),
    heading: Color::Rgb(0x81, 0x36, 0xe3),
    accent: Color::Rgb(0x1b, 0x5c, 0xdd),
    success: Color::Rgb(0x2f, 0x76, 0x1f),
    warning: Color::Rgb(0x8c, 0x59, 0x14),
    danger: Color::Rgb(0xc8, 0x0e, 0x36),
    fold: Color::Rgb(0x81, 0x36, 0xe3),
    bat_theme: "Catppuccin Latte",
    added_bg: Color::Rgb(0xba, 0xd2, 0xbe),
    removed_bg: Color::Rgb(0xda, 0xb2, 0xc2),
};

const NORD: Theme = Theme {
    background: Color::Rgb(0x2e, 0x34, 0x40),
    foreground: Color::Rgb(0xd8, 0xde, 0xe9),
    muted: Color::Rgb(0x7b, 0x88, 0xa1),
    selection: Color::Rgb(0x43, 0x4c, 0x5e),
    heading: Color::Rgb(0x88, 0xc0, 0xd0),
    accent: Color::Rgb(0x81, 0xa1, 0xc1),
    success: Color::Rgb(0xa3, 0xbe, 0x8c),
    warning: Color::Rgb(0xeb, 0xcb, 0x8b),
    danger: Color::Rgb(0xbf, 0x61, 0x6a),
    fold: Color::Rgb(0xb4, 0x8e, 0xad),
    bat_theme: "Nord",
    added_bg: Color::Rgb(0x48, 0x52, 0x51),
    removed_bg: Color::Rgb(0x4e, 0x3e, 0x49),
};

const GRUVBOX_DARK: Theme = Theme {
    background: Color::Rgb(0x28, 0x28, 0x28),
    foreground: Color::Rgb(0xeb, 0xdb, 0xb2),
    muted: Color::Rgb(0x92, 0x83, 0x74),
    selection: Color::Rgb(0x50, 0x49, 0x45),
    heading: Color::Rgb(0xd3, 0x86, 0x9b),
    accent: Color::Rgb(0x83, 0xa5, 0x98),
    success: Color::Rgb(0xb8, 0xbb, 0x26),
    warning: Color::Rgb(0xfa, 0xbd, 0x2f),
    danger: Color::Rgb(0xfb, 0x49, 0x34),
    fold: Color::Rgb(0xd3, 0x86, 0x9b),
    bat_theme: "gruvbox-dark",
    added_bg: Color::Rgb(0x48, 0x48, 0x28),
    removed_bg: Color::Rgb(0x56, 0x2f, 0x2b),
};

const GRUVBOX_LIGHT: Theme = Theme {
    background: Color::Rgb(0xeb, 0xdb, 0xb2),
    foreground: Color::Rgb(0x3c, 0x38, 0x36),
    muted: Color::Rgb(0x7c, 0x6f, 0x64),
    selection: Color::Rgb(0xd5, 0xc4, 0xa1),
    heading: Color::Rgb(0x8f, 0x3f, 0x71),
    accent: Color::Rgb(0x07, 0x66, 0x78),
    success: Color::Rgb(0x73, 0x6e, 0x0d),
    warning: Color::Rgb(0x93, 0x60, 0x10),
    danger: Color::Rgb(0x9d, 0x00, 0x06),
    fold: Color::Rgb(0x8f, 0x3f, 0x71),
    bat_theme: "gruvbox-light",
    added_bg: Color::Rgb(0xd2, 0xc4, 0x8e),
    removed_bg: Color::Rgb(0xda, 0xab, 0x8c),
};

const SOLARIZED_DARK: Theme = Theme {
    background: Color::Rgb(0x00, 0x2b, 0x36),
    foreground: Color::Rgb(0x93, 0xa1, 0xa1),
    muted: Color::Rgb(0x65, 0x7b, 0x83),
    selection: Color::Rgb(0x07, 0x36, 0x42),
    heading: Color::Rgb(0x6c, 0x71, 0xc4),
    accent: Color::Rgb(0x26, 0x8b, 0xd2),
    success: Color::Rgb(0x85, 0x99, 0x00),
    warning: Color::Rgb(0xb5, 0x89, 0x00),
    danger: Color::Rgb(0xdc, 0x32, 0x2f),
    fold: Color::Rgb(0xd3, 0x36, 0x82),
    bat_theme: "Solarized (dark)",
    added_bg: Color::Rgb(0x1d, 0x43, 0x2a),
    removed_bg: Color::Rgb(0x30, 0x2d, 0x34),
};

const SOLARIZED_LIGHT: Theme = Theme {
    background: Color::Rgb(0xee, 0xe8, 0xd5),
    foreground: Color::Rgb(0x58, 0x6e, 0x75),
    muted: Color::Rgb(0x76, 0x86, 0x87),
    selection: Color::Rgb(0xdd, 0xd6, 0xc1),
    heading: Color::Rgb(0x6c, 0x71, 0xc4),
    accent: Color::Rgb(0x22, 0x7d, 0xbe),
    success: Color::Rgb(0x6c, 0x7c, 0x00),
    warning: Color::Rgb(0x93, 0x70, 0x00),
    danger: Color::Rgb(0xdc, 0x32, 0x2f),
    fold: Color::Rgb(0xd3, 0x36, 0x82),
    bat_theme: "Solarized (light)",
    added_bg: Color::Rgb(0xd7, 0xd7, 0xa6),
    removed_bg: Color::Rgb(0xea, 0xc0, 0xb0),
};

const ONE_HALF_DARK: Theme = Theme {
    background: Color::Rgb(0x28, 0x2c, 0x34),
    foreground: Color::Rgb(0xdc, 0xdf, 0xe4),
    muted: Color::Rgb(0x7f, 0x84, 0x8e),
    selection: Color::Rgb(0x3e, 0x44, 0x51),
    heading: Color::Rgb(0xc6, 0x78, 0xdd),
    accent: Color::Rgb(0x61, 0xaf, 0xef),
    success: Color::Rgb(0x98, 0xc3, 0x79),
    warning: Color::Rgb(0xe5, 0xc0, 0x7b),
    danger: Color::Rgb(0xe0, 0x6c, 0x75),
    fold: Color::Rgb(0xc6, 0x78, 0xdd),
    bat_theme: "OneHalfDark",
    added_bg: Color::Rgb(0x41, 0x4d, 0x43),
    removed_bg: Color::Rgb(0x50, 0x3a, 0x42),
};

const ONE_HALF_LIGHT: Theme = Theme {
    background: Color::Rgb(0xe5, 0xe5, 0xe6),
    foreground: Color::Rgb(0x38, 0x3a, 0x42),
    muted: Color::Rgb(0x79, 0x7a, 0x81),
    selection: Color::Rgb(0xca, 0xcb, 0xce),
    heading: Color::Rgb(0xa6, 0x26, 0xa4),
    accent: Color::Rgb(0x01, 0x77, 0xaa),
    success: Color::Rgb(0x3e, 0x7c, 0x3d),
    warning: Color::Rgb(0x95, 0x66, 0x01),
    danger: Color::Rgb(0xba, 0x46, 0x3c),
    fold: Color::Rgb(0xa6, 0x26, 0xa4),
    bat_theme: "OneHalfLight",
    added_bg: Color::Rgb(0xc4, 0xd6, 0xc5),
    removed_bg: Color::Rgb(0xe5, 0xc6, 0xc3),
};

const MONOKAI: Theme = Theme {
    background: Color::Rgb(0x27, 0x28, 0x22),
    foreground: Color::Rgb(0xf8, 0xf8, 0xf2),
    muted: Color::Rgb(0x75, 0x71, 0x5e),
    selection: Color::Rgb(0x49, 0x48, 0x3e),
    heading: Color::Rgb(0xae, 0x81, 0xff),
    accent: Color::Rgb(0x66, 0xd9, 0xef),
    success: Color::Rgb(0xa6, 0xe2, 0x2e),
    warning: Color::Rgb(0xe6, 0xdb, 0x74),
    danger: Color::Rgb(0xf9, 0x26, 0x72),
    fold: Color::Rgb(0xae, 0x81, 0xff),
    bat_theme: "Monokai Extended",
    added_bg: Color::Rgb(0x43, 0x51, 0x25),
    removed_bg: Color::Rgb(0x55, 0x28, 0x34),
};

const GITHUB: Theme = Theme {
    background: Color::Rgb(0xe4, 0xe8, 0xec),
    foreground: Color::Rgb(0x24, 0x29, 0x2f),
    muted: Color::Rgb(0x6e, 0x77, 0x81),
    selection: Color::Rgb(0xc8, 0xd1, 0xda),
    heading: Color::Rgb(0x82, 0x50, 0xdf),
    accent: Color::Rgb(0x09, 0x69, 0xda),
    success: Color::Rgb(0x1a, 0x7f, 0x37),
    warning: Color::Rgb(0x9a, 0x67, 0x00),
    danger: Color::Rgb(0xcf, 0x22, 0x2e),
    fold: Color::Rgb(0x82, 0x50, 0xdf),
    bat_theme: "GitHub",
    added_bg: Color::Rgb(0xb8, 0xd1, 0xc4),
    removed_bg: Color::Rgb(0xdf, 0xbc, 0xc2),
};

impl Theme {
    /// Bold text: the text color pushed halfway to white on a dark page, or to black on
    /// a light one, so bold stands out even in a font with no bold weight.
    pub fn strong(&self) -> Color {
        let (Color::Rgb(red, green, blue), Color::Rgb(..)) = (self.foreground, self.background)
        else {
            return self.foreground;
        };
        let target = if group_of(self) == "Light" { 0 } else { 255 };
        let toward = |value: u8| ((u16::from(value) + target) / 2) as u8;
        Color::Rgb(toward(red), toward(green), toward(blue))
    }
}

/// The menu group a theme belongs to.
pub fn group_of(theme: &Theme) -> &'static str {
    match theme.background {
        Color::Rgb(red, green, blue)
            if u32::from(red) + u32::from(green) + u32::from(blue) > 384 =>
        {
            "Light"
        }
        Color::Rgb(..) => "Dark",
        _ => "Your terminal",
    }
}

/// A color as delta and git write it: "#rrggbb" or a 256-color number.
pub fn color_name(color: Color) -> String {
    match color {
        Color::Rgb(red, green, blue) => format!("#{red:02x}{green:02x}{blue:02x}"),
        Color::Indexed(number) => number.to_string(),
        // The tints are always RGB or indexed; "normal" leaves the text untinted.
        _ => "normal".to_string(),
    }
}

/// The built-in name of a theme, for marking the current one in the menu.
pub fn name_of(theme: &Theme) -> Option<&'static str> {
    THEME_NAMES
        .into_iter()
        .find(|name| by_name(name).is_ok_and(|named| named == *theme))
}

pub fn by_name(name: &str) -> Result<Theme> {
    match THEMES.iter().find(|(known, _)| *known == name) {
        Some((_, theme)) => Ok(*theme),
        None => bail!(
            "unknown theme \"{name}\"; choose one of: {}",
            THEME_NAMES.join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_theme_has_a_name_and_a_bat_theme_bat_knows() {
        for name in THEME_NAMES {
            assert_eq!(name_of(&by_name(name).unwrap()), Some(name));
        }
        let Ok(output) = std::process::Command::new("bat")
            .arg("--list-themes")
            .output()
        else {
            return;
        };
        let known = String::from_utf8_lossy(&output.stdout);
        for (name, theme) in THEMES {
            assert!(
                known.lines().any(|line| line.trim() == theme.bat_theme),
                "{name}: bat has no theme \"{}\"",
                theme.bat_theme
            );
        }
    }

    #[test]
    fn the_menu_lists_terminal_then_dark_then_light_each_a_to_z() {
        let groups: Vec<&str> = THEMES.iter().map(|(_, theme)| group_of(theme)).collect();
        let mut sorted = groups.clone();
        sorted.sort_by_key(|group| {
            ["Your terminal", "Dark", "Light"]
                .iter()
                .position(|g| g == group)
        });
        assert_eq!(groups, sorted, "groups stay together in order");
        for group in ["Dark", "Light"] {
            let names: Vec<&str> = THEMES
                .iter()
                .filter(|(_, theme)| group_of(theme) == group)
                .map(|(name, _)| *name)
                .collect();
            assert!(names.is_sorted(), "{group}: {names:?}");
        }
    }

    #[test]
    fn tints_are_named_the_way_delta_reads_them() {
        assert_eq!(color_name(DRACULA.added_bg), "#1f3d2b");
        assert_eq!(color_name(TERMINAL.removed_bg), "52");
    }

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
        assert!(error.contains("terminal, catppuccin_frappe"));
    }
}
