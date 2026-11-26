//! Color themes for the dashboard.
//!
//! Token names and values come from the popier design system ("Popier Pipeline
//! Terminal"). Every theme defines the same tokens; a custom theme file only
//! overrides some of them and inherits the rest from `noite`.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use ratatui::style::Color;
use serde::Deserialize;

use crate::status::Status;

/// Built-in theme ids, in the order `t` cycles through them.
pub const BUILTIN: [&str; 5] = ["noite", "brasa", "papel", "alto-contraste", "terminal"];

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub name: String,
    /// Screen background. `Color::Reset` keeps the terminal's own.
    pub bg: Color,
    /// Header background.
    pub surface: Color,
    /// Footer and overlay panels.
    pub surface_raised: Color,
    /// Selected row background.
    pub selection: Color,
    /// Panel borders.
    pub line: Color,
    /// Empty part of a progress bar.
    pub track: Color,
    /// Main text.
    pub ink: Color,
    /// Secondary text: branch, counts, durations, hints.
    pub muted: Color,
    /// Tree connectors and step numbers only.
    pub faint: Color,
    /// Brand mark, keys, filter cursor, active theme.
    pub accent: Color,
    /// Text on an `accent` block.
    pub on_accent: Color,
    pub running: Color,
    pub queued: Color,
    pub success: Color,
    pub failure: Color,
    pub skipped: Color,
    pub cancelled: Color,
}

impl Theme {
    pub fn status(&self, status: Status) -> Color {
        match status {
            Status::Running => self.running,
            Status::Queued => self.queued,
            Status::Success => self.success,
            Status::Failure => self.failure,
            Status::Skipped => self.skipped,
            Status::Cancelled => self.cancelled,
        }
    }

    /// A built-in theme by id.
    pub fn builtin(name: &str) -> Option<Self> {
        let hex = match name {
            "noite" => NOITE,
            "brasa" => BRASA,
            "papel" => PAPEL,
            "alto-contraste" => ALTO_CONTRASTE,
            "terminal" => return Some(Self::terminal()),
            _ => return None,
        };
        Some(Self::from_hex(name, hex))
    }

    /// The theme for `name`: built-in, or `~/.config/popier/themes/<name>.toml`.
    /// `None` picks `noite`, or `terminal` when the terminal lacks true color.
    pub fn resolve(name: Option<&str>) -> Result<Self> {
        let Some(name) = name else {
            let truecolor = std::env::var("COLORTERM")
                .is_ok_and(|v| v.contains("truecolor") || v.contains("24bit"));
            let default = if truecolor { "noite" } else { "terminal" };
            return Ok(Self::builtin(default).expect("built-in"));
        };
        if let Some(theme) = Self::builtin(name) {
            return Ok(theme);
        }
        let path = custom_path(name)
            .with_context(|| format!("unknown theme {name:?} and no config dir"))?;
        let text = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "unknown theme {name:?} (built-in: {}; custom themes go in {})",
                BUILTIN.join(", "),
                path.display()
            )
        })?;
        Self::parse_custom(name, &text).with_context(|| format!("invalid theme {}", path.display()))
    }

    /// Parses a custom theme file: `token = "#rrggbb"` lines, any subset.
    pub fn parse_custom(name: &str, text: &str) -> Result<Self> {
        let values: HashMap<String, String> = toml::from_str::<CustomFile>(text)?.0;
        let mut theme = Self::from_hex(name, NOITE);
        for (key, value) in &values {
            let color = parse_hex(value)
                .with_context(|| format!("{key} = {value:?} is not a #rrggbb color"))?;
            let Some(slot) = theme.slot(key) else {
                bail!("unknown token {key:?}");
            };
            *slot = color;
        }
        Ok(theme)
    }

    fn slot(&mut self, key: &str) -> Option<&mut Color> {
        Some(match key {
            "bg" => &mut self.bg,
            "surface" => &mut self.surface,
            "surface-raised" => &mut self.surface_raised,
            "selection" => &mut self.selection,
            "line" => &mut self.line,
            "track" => &mut self.track,
            "ink" => &mut self.ink,
            "muted" => &mut self.muted,
            "faint" => &mut self.faint,
            "accent" => &mut self.accent,
            "on-accent" => &mut self.on_accent,
            "running" => &mut self.running,
            "queued" => &mut self.queued,
            "success" => &mut self.success,
            "failure" => &mut self.failure,
            "skipped" => &mut self.skipped,
            "cancelled" => &mut self.cancelled,
            _ => return None,
        })
    }

    fn from_hex(name: &str, hex: [&str; 17]) -> Self {
        let c = |i: usize| parse_hex(hex[i]).expect("built-in colors are valid");
        Self {
            name: name.to_owned(),
            bg: c(0),
            surface: c(1),
            surface_raised: c(2),
            selection: c(3),
            line: c(4),
            track: c(5),
            ink: c(6),
            muted: c(7),
            faint: c(8),
            accent: c(9),
            on_accent: c(10),
            running: c(11),
            success: c(12),
            failure: c(13),
            queued: c(14),
            skipped: c(15),
            cancelled: c(16),
        }
    }

    /// Uses the terminal's own 16 ANSI colors, so it follows its color scheme.
    fn terminal() -> Self {
        Self {
            name: "terminal".into(),
            bg: Color::Reset,
            surface: Color::Reset,
            surface_raised: Color::Reset,
            selection: Color::DarkGray,
            line: Color::DarkGray,
            track: Color::DarkGray,
            ink: Color::Reset,
            muted: Color::Gray,
            faint: Color::DarkGray,
            accent: Color::Yellow,
            on_accent: Color::Black,
            running: Color::Yellow,
            queued: Color::Gray,
            success: Color::Green,
            failure: Color::Red,
            skipped: Color::DarkGray,
            cancelled: Color::Magenta,
        }
    }

    /// The built-in theme after this one (custom themes go back to the first).
    pub fn next(&self) -> Self {
        let i = BUILTIN.iter().position(|n| *n == self.name);
        let next = i.map_or(0, |i| (i + 1) % BUILTIN.len());
        Self::builtin(BUILTIN[next]).expect("built-in")
    }
}

#[derive(Deserialize)]
struct CustomFile(HashMap<String, String>);

fn custom_path(name: &str) -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("popier").join("themes").join(format!("{name}.toml")))
}

fn parse_hex(s: &str) -> Option<Color> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some(Color::Rgb(byte(0)?, byte(2)?, byte(4)?))
}

// Order: bg, surface, surface-raised, selection, line, track, ink, muted, faint,
// accent, on-accent, running, success, failure, queued, skipped, cancelled.
const NOITE: [&str; 17] = [
    "#0e1016", "#151821", "#1c202b", "#262d3d", "#2a3040", "#2e3546", "#e7eaf0", "#9aa3b5",
    "#5d6578", "#ffb454", "#1a1206", "#62b6ff", "#4fd18b", "#ff6b6b", "#a0a8bd", "#7a8296",
    "#b89fc0",
];
const BRASA: [&str; 17] = [
    "#15110e", "#1d1814", "#262019", "#33291f", "#3a3027", "#3b3025", "#f2e9df", "#b3a493",
    "#7a6c5e", "#ff8f4a", "#1a0d05", "#ffc65c", "#9fd36b", "#ff6f61", "#b3a493", "#8a7b6c",
    "#c49a8a",
];
const PAPEL: [&str; 17] = [
    "#f7f5f0", "#ffffff", "#efece5", "#eee9dd", "#d9d4c9", "#dcd5c6", "#1f2328", "#575e69",
    "#9a9ea6", "#a35200", "#ffffff", "#0b5cb8", "#17702f", "#c22525", "#575e69", "#6c717a",
    "#7a5487",
];
const ALTO_CONTRASTE: [&str; 17] = [
    "#000000", "#000000", "#0d0d0d", "#262626", "#9a9a9a", "#4d4d4d", "#ffffff", "#d6d6d6",
    "#a6a6a6", "#f5a6ff", "#000000", "#ffe14d", "#3fd8ff", "#ff9e3d", "#d6d6d6", "#a6a6a6",
    "#d6d6d6",
];

/// How progress bars are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BarStyle {
    /// `━━━━╸────`
    #[default]
    Line,
    /// `████▌░░░░` with eighth blocks.
    Block,
}

/// One painted piece of a bar: text and whether it is the filled part.
pub fn bar(fraction: f64, width: usize, style: BarStyle, status: Status) -> Vec<(String, bool)> {
    let fraction = fraction.clamp(0.0, 1.0);
    if status == Status::Queued {
        return vec![("╌".repeat(width), false)];
    }
    match style {
        BarStyle::Line => {
            let full = (fraction * width as f64).round() as usize;
            let mut filled = "━".repeat(full);
            if status == Status::Running && full > 0 && full < width {
                filled.pop();
                filled.push('╸');
            }
            vec![(filled, true), ("─".repeat(width - full), false)]
        }
        BarStyle::Block => {
            const EIGHTHS: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];
            let total = fraction * width as f64;
            let mut full = total.floor() as usize;
            let mut part = ((total - full as f64) * 8.0).round() as usize;
            if part == 8 {
                full += 1;
                part = 0;
            }
            let mut filled = "█".repeat(full);
            let mut rest = width - full;
            if part > 0 && rest > 0 {
                filled.push(EIGHTHS[part]);
                rest -= 1;
            }
            vec![(filled, true), ("░".repeat(rest), false)]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(parts: &[(String, bool)]) -> String {
        parts.iter().map(|(s, _)| s.as_str()).collect()
    }

    #[test]
    fn every_builtin_resolves_and_cycles() {
        let mut theme = Theme::builtin("noite").unwrap();
        for name in BUILTIN.iter().skip(1).chain(["noite"].iter()) {
            theme = theme.next();
            assert_eq!(&theme.name, name);
        }
    }

    #[test]
    fn custom_theme_overrides_some_tokens() {
        let theme = Theme::parse_custom("mine", "running = \"#00ff00\"\n").unwrap();
        assert_eq!(theme.running, Color::Rgb(0, 255, 0));
        assert_eq!(theme.bg, Theme::builtin("noite").unwrap().bg);
        assert!(Theme::parse_custom("x", "nope = \"#000000\"\n").is_err());
        assert!(Theme::parse_custom("x", "bg = \"red\"\n").is_err());
    }

    #[test]
    fn line_bar_shows_head_while_running() {
        let parts = bar(0.5, 10, BarStyle::Line, Status::Running);
        assert_eq!(text(&parts), "━━━━╸─────");
        let parts = bar(1.0, 10, BarStyle::Line, Status::Success);
        assert_eq!(text(&parts), "━━━━━━━━━━");
        let parts = bar(0.0, 4, BarStyle::Line, Status::Queued);
        assert_eq!(text(&parts), "╌╌╌╌");
    }

    #[test]
    fn block_bar_uses_eighths() {
        let parts = bar(0.3, 10, BarStyle::Block, Status::Running);
        assert_eq!(text(&parts), "███░░░░░░░");
        let parts = bar(0.35, 10, BarStyle::Block, Status::Running);
        assert_eq!(text(&parts), "███▌░░░░░░");
        assert_eq!(text(&parts).chars().count(), 10);
    }
}
