//! Plugin options. They live in tmux global user options so every sidebar,
//! hook and key binding reads the same state.

use anyhow::Result;

use crate::tmux::{self, SEP};

/// Width of the rail, which shows window numbers only.
pub const RAIL: u16 = 7;
/// Narrowest full tree; dragging below it snaps to the rail.
pub const MIN_FULL: u16 = 14;
pub const MAX_W: u16 = 64;
/// Dragging below this hides the sidebar.
pub const HIDE_BELOW: u16 = 4;
pub const DEFAULT_WIDTH: u16 = 28;
/// Below this width the map style falls back to branches.
pub const MAP_MIN: u16 = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Full,
    Rail,
    Hidden,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Full => "full",
            Mode::Rail => "rail",
            Mode::Hidden => "hidden",
        }
    }

    fn parse(s: &str) -> Option<Mode> {
        match s {
            "full" => Some(Mode::Full),
            "rail" => Some(Mode::Rail),
            "hidden" => Some(Mode::Hidden),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    pub fn name(self) -> &'static str {
        match self {
            Side::Left => "left",
            Side::Right => "right",
        }
    }

    pub fn other(self) -> Side {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    Branches,
    Blocks,
    Map,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Auto,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Zh,
    En,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub mode: Mode,
    pub last_mode: Mode,
    pub width: u16,
    pub default_width: u16,
    pub position: Side,
    pub style: Variant,
    pub label_styles: bool,
    pub theme: Theme,
    pub lang: Lang,
    /// Seconds between checks of pane commands while visible; 0 disables.
    pub interval: u64,
    pub hints: bool,
    /// Sidebar background as a tmux colour; empty keeps the terminal's.
    pub bg: String,
    pub hide_status_windows: bool,
    pub status_button: bool,
    pub bin: String,
    pub prefix: String,
    /// The split buttons under the header.
    pub buttons: bool,
    /// The user's keys for split-window -h and -v, read with list-keys.
    pub split_h_key: String,
    pub split_v_key: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            mode: Mode::Full,
            last_mode: Mode::Full,
            width: DEFAULT_WIDTH,
            default_width: DEFAULT_WIDTH,
            position: Side::Left,
            style: Variant::Branches,
            label_styles: true,
            theme: Theme::Auto,
            lang: Lang::Zh,
            interval: 2,
            hints: true,
            bg: String::new(),
            hide_status_windows: true,
            status_button: true,
            bin: String::new(),
            prefix: "C-b".into(),
            buttons: true,
            split_h_key: String::new(),
            split_v_key: String::new(),
        }
    }
}

const KEYS: [&str; 19] = [
    "@arbor-mode",
    "@arbor-last-mode",
    "@arbor-width",
    "@arbor-default-width",
    "@arbor-position",
    "@arbor-style",
    "@arbor-label-styles",
    "@arbor-theme",
    "@arbor-lang",
    "@arbor-interval",
    "@arbor-hints",
    "@arbor-bg",
    "@arbor-hide-status-windows",
    "@arbor-status-button",
    "@arbor-bin",
    "prefix",
    "@arbor-buttons",
    "@arbor-split-h-key",
    "@arbor-split-v-key",
];

fn flag(s: &str, default: bool) -> bool {
    match s.to_ascii_lowercase().as_str() {
        "on" | "1" | "yes" | "true" => true,
        "off" | "0" | "no" | "false" => false,
        _ => default,
    }
}

fn width(s: &str) -> Option<u16> {
    s.trim().parse::<u16>().ok().map(|w| w.clamp(MIN_FULL, MAX_W))
}

impl Config {
    /// A format that prints every option on one line.
    pub fn query_format() -> String {
        KEYS.iter().map(|k| format!("#{{{k}}}")).collect::<Vec<_>>().join(&SEP.to_string())
    }

    pub fn parse(line: &str) -> Config {
        let f: Vec<&str> = line.split(SEP).collect();
        let get = |i: usize| f.get(i).copied().unwrap_or("").trim();
        let d = Config::default();
        let width = width(get(2)).unwrap_or(d.width);
        Config {
            mode: Mode::parse(get(0)).unwrap_or(d.mode),
            last_mode: Mode::parse(get(1)).filter(|m| *m != Mode::Hidden).unwrap_or(Mode::Full),
            width,
            default_width: crate::config::width(get(3)).unwrap_or(DEFAULT_WIDTH),
            position: if get(4) == "right" { Side::Right } else { Side::Left },
            style: match get(5) {
                "blocks" => Variant::Blocks,
                "map" => Variant::Map,
                _ => Variant::Branches,
            },
            label_styles: flag(get(6), true),
            theme: match get(7) {
                "light" => Theme::Light,
                "dark" => Theme::Dark,
                _ => Theme::Auto,
            },
            lang: if get(8) == "en" { Lang::En } else { Lang::Zh },
            interval: get(9).parse().unwrap_or(d.interval),
            hints: flag(get(10), true),
            bg: get(11).to_string(),
            hide_status_windows: flag(get(12), true),
            status_button: flag(get(13), true),
            bin: get(14).to_string(),
            prefix: if get(15).is_empty() { d.prefix } else { get(15).to_string() },
            buttons: flag(get(16), true),
            split_h_key: get(17).to_string(),
            split_v_key: get(18).to_string(),
        }
    }

    pub fn load() -> Result<Config> {
        let out = tmux::run(["display-message", "-p", &Config::query_format()])?;
        Ok(Config::parse(out.lines().next().unwrap_or("")))
    }

    /// Columns the sidebar should occupy in the current mode.
    pub fn cols(&self) -> u16 {
        match self.mode {
            Mode::Full => self.width,
            Mode::Rail => RAIL,
            Mode::Hidden => 0,
        }
    }

    /// The arbor binary hooks and splits should run.
    pub fn bin_path(&self) -> String {
        if !self.bin.is_empty() {
            return self.bin.clone();
        }
        std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "arbor".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_clamps() {
        let s = SEP.to_string();
        let line =
            ["rail", "hidden", "90", "", "right", "map", "off", "dark", "en", "0", "", "", "", "", "/bin/arbor", "C-a"]
                .join(&s);
        let c = Config::parse(&line);
        assert_eq!(c.mode, Mode::Rail);
        assert_eq!(c.last_mode, Mode::Full);
        assert_eq!(c.width, MAX_W);
        assert_eq!(c.default_width, DEFAULT_WIDTH);
        assert_eq!(c.position, Side::Right);
        assert_eq!(c.style, Variant::Map);
        assert!(!c.label_styles);
        assert_eq!(c.lang, Lang::En);
        assert_eq!(c.interval, 0);
        assert_eq!(c.cols(), RAIL);
        assert_eq!(c.prefix, "C-a");
    }

    #[test]
    fn empty_line_gives_defaults() {
        assert_eq!(Config::parse(""), Config::default());
    }
}
