//! Paints dashboard rows, the header and key hints with the active theme.
//!
//! Layout follows the design system: fixed columns for name, bar, percent,
//! count and time on every row, so the eye can run down a column.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::status::Status;
use crate::theme::{self, BarStyle, Theme};
use crate::tree::{Elapsed, Gauge, Row, RowKey};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Everything that decides how a frame looks.
pub struct Look<'a> {
    pub theme: &'a Theme,
    pub bar: BarStyle,
    /// Animation frame, advancing every 100 ms.
    pub tick: usize,
}

impl Look<'_> {
    fn fg(&self, color: ratatui::style::Color) -> Style {
        Style::new().fg(color)
    }
    fn status(&self, status: Status) -> Style {
        self.fg(self.theme.status(status))
    }
    fn spinner(&self) -> &'static str {
        SPINNER[self.tick % SPINNER.len()]
    }
    /// Status glyph; running ones spin unless `still`.
    fn glyph(&self, status: Status, still: bool) -> Span<'static> {
        let g = if status == Status::Running && !still {
            self.spinner()
        } else {
            status.icon()
        };
        Span::styled(format!("{g} "), self.status(status))
    }
}

/// Column widths for a terminal `width` cells wide: (name, bar).
pub fn columns(width: u16) -> (usize, usize) {
    match width {
        140.. => (44, 20),
        100.. => (36, 20),
        80.. => (32, 10),
        w => ((w as usize).saturating_sub(22).max(16), 0),
    }
}

/// One line per row, with tree connectors, bars and times.
pub fn rows(rows: &[Row], look: &Look, width: u16) -> Vec<Line<'static>> {
    let (name_w, bar_w) = columns(width);
    let prefixes = tree_prefixes(rows);
    rows.iter()
        .zip(prefixes)
        .map(|(row, prefix)| row_line(row, &prefix, look, name_w, bar_w))
        .collect()
}

/// `├─ │ └─` prefixes, from which rows have a later sibling.
fn tree_prefixes(rows: &[Row]) -> Vec<String> {
    const MAX: usize = 8;
    let mut next_sibling = vec![false; rows.len()];
    let mut seen = [false; MAX];
    for (i, row) in rows.iter().enumerate().rev() {
        let d = (row.depth as usize).min(MAX - 1);
        next_sibling[i] = seen[d];
        seen[d] = true;
        seen[d + 1..].iter_mut().for_each(|s| *s = false);
    }
    let mut open = [false; MAX];
    rows.iter()
        .enumerate()
        .map(|(i, row)| {
            let d = (row.depth as usize).min(MAX - 1);
            open[d] = next_sibling[i];
            if d == 0 {
                return String::new();
            }
            let mut p = String::from("  ");
            for &o in &open[1..d] {
                p.push_str(if o { "│   " } else { "    " });
            }
            if matches!(row.key, RowKey::Step(..)) {
                p.push_str("    ");
            } else {
                p.push_str(if next_sibling[i] {
                    "├─ "
                } else {
                    "└─ "
                });
            }
            p
        })
        .collect()
}

fn row_line(row: &Row, prefix: &str, look: &Look, name_w: usize, bar_w: usize) -> Line<'static> {
    let t = look.theme;
    let mut left: Vec<Span<'static>> = Vec::new();
    let mut name_style = Style::new().fg(t.ink);
    match &row.key {
        RowKey::Group(_) | RowKey::Repo(_) => {
            left.push(Span::styled(prefix.to_owned(), look.fg(t.faint)));
            left.push(Span::styled(fold(row), look.fg(t.faint)));
            match row.status {
                Some(s) => left.push(look.glyph(s, true)),
                None => left.push(Span::raw("  ")),
            }
            name_style = name_style.add_modifier(Modifier::BOLD);
        }
        RowKey::Workflow(_) => {
            left.push(Span::styled(prefix.to_owned(), look.fg(t.faint)));
            left.push(Span::styled(fold(row), look.fg(t.faint)));
            if let Some(s) = row.status {
                left.push(look.glyph(s, false));
            }
        }
        RowKey::Job(..) => {
            left.push(Span::styled(prefix.to_owned(), look.fg(t.faint)));
            if let Some(s) = row.status {
                left.push(look.glyph(s, false));
            }
        }
        RowKey::Step(..) => {
            left.push(Span::styled(prefix.to_owned(), look.fg(t.faint)));
            if let Some(s) = row.status {
                left.push(look.glyph(s, false));
                name_style = match s {
                    Status::Failure => look.status(s),
                    Status::Queued | Status::Skipped => look.fg(t.skipped),
                    _ => name_style,
                };
            }
            if let Some(d) = &row.detail {
                left.push(Span::styled(format!("{d} "), look.fg(t.faint)));
            }
        }
        RowKey::Note(..) => {
            left.push(Span::styled(prefix.to_owned(), look.fg(t.faint)));
            name_style = if row.is_error {
                look.fg(t.failure)
            } else {
                look.fg(t.muted)
            };
        }
    }

    // Name, then the secondary text that may be cut first.
    let used: usize = left.iter().map(|s| s.width()).sum();
    let room = name_w.saturating_sub(used + 1);
    let name = cut(&row.text, room.max(4));
    let mut rest = room.saturating_sub(name.chars().count());
    left.push(Span::styled(name, name_style));
    let mut extra = |text: String, style: Style, left: &mut Vec<Span<'static>>| {
        if rest > 3 && !text.is_empty() {
            let text = cut(&text, rest);
            rest = rest.saturating_sub(text.chars().count());
            left.push(Span::styled(text, style));
        }
    };
    if let Some(lead) = &row.lead {
        extra(format!("  {lead}"), look.fg(t.muted), &mut left);
        if let Some(b) = &row.branch {
            extra(format!(" · {b}"), look.fg(t.faint), &mut left);
        }
    } else if let Some(b) = &row.branch {
        extra(format!("  {b}"), look.fg(t.muted), &mut left);
    }

    let used: usize = left.iter().map(|s| s.width()).sum();
    if used < name_w {
        left.push(Span::raw(" ".repeat(name_w - used)));
    }

    if let Some(gauge) = &row.gauge {
        if matches!(row.key, RowKey::Step(..)) {
            let gap = if bar_w > 0 { bar_w + 2 } else { 1 } + 5 + 8;
            left.push(Span::raw(" ".repeat(gap)));
            left.push(Span::styled(elapsed(gauge.elapsed), look.fg(t.muted)));
        } else if matches!(row.key, RowKey::Group(_) | RowKey::Repo(_)) && row.expanded {
            // The open tree below already says why; keep the repo line calm.
            let calm = Gauge {
                hint: None,
                ..gauge.clone()
            };
            left.extend(gauge_spans(&calm, look, bar_w));
        } else {
            left.extend(gauge_spans(gauge, look, bar_w));
        }
    }
    Line::from(left)
}

fn fold(row: &Row) -> &'static str {
    match (row.foldable, row.expanded) {
        (true, true) => "▾ ",
        (true, false) => "▸ ",
        _ => "",
    }
}

/// Bar, percent, count, time and hint.
fn gauge_spans(g: &Gauge, look: &Look, bar_w: usize) -> Vec<Span<'static>> {
    let t = look.theme;
    let mut out = vec![Span::raw(" ")];
    let fraction = g.fraction();
    if bar_w > 0 {
        match fraction {
            Some(f) => {
                for (text, filled) in theme::bar(f, bar_w, look.bar, g.status) {
                    let color = if filled { t.status(g.status) } else { t.track };
                    out.push(Span::styled(text, look.fg(color)));
                }
            }
            None => out.push(Span::raw(" ".repeat(bar_w))),
        }
        out.push(Span::raw(" "));
    }
    let pct = |f: f64| format!("{:>4}%", (f * 100.0).floor() as u32);
    out.push(match (g.status, fraction) {
        (Status::Queued, _) => Span::styled(" wait", look.status(g.status)),
        (Status::Skipped, _) => Span::styled(" skip", look.status(g.status)),
        (Status::Success, Some(f)) if f >= 1.0 => Span::styled("  ok ", look.status(g.status)),
        (Status::Failure, Some(f)) => Span::styled(pct(f), look.status(g.status)),
        (_, Some(f)) => Span::styled(pct(f), look.fg(t.ink)),
        (_, None) => Span::styled("   … ", look.fg(t.muted)),
    });
    let count = if g.total > 0 {
        format!("  {}/{}", g.done, g.total)
    } else {
        String::new()
    };
    out.push(Span::styled(format!("{count:<8}"), look.fg(t.muted)));
    out.push(Span::styled(elapsed(g.elapsed), look.fg(t.muted)));
    if let Some(hint) = &g.hint {
        out.push(Span::raw("   "));
        match (g.status, hint.as_str()) {
            (Status::Failure, "l log") => {
                out.push(Span::styled(
                    "l",
                    look.fg(t.accent).add_modifier(Modifier::BOLD),
                ));
                out.push(Span::styled(" log", look.fg(t.muted)));
            }
            (Status::Running, _) => {
                out.push(Span::styled(
                    format!("{} ", look.spinner()),
                    look.fg(t.running),
                ));
                out.push(Span::styled(hint.clone(), look.fg(t.ink)));
            }
            _ => out.push(Span::styled(hint.clone(), look.status(g.status))),
        }
    }
    out
}

/// `24s`, `1m02s`, `1h05m`, or `12m ago`, right-aligned in 6+ cells.
pub fn elapsed(e: Option<Elapsed>) -> String {
    let short = |s: u64| match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m{:02}s", s / 60, s % 60),
        _ => format!("{}h{:02}m", s / 3600, s % 3600 / 60),
    };
    let text = match e {
        None => String::new(),
        Some(Elapsed::Took(s)) => short(s),
        Some(Elapsed::Ago(s)) => match s {
            0..60 => "just now".into(),
            60..3600 => format!("{}m ago", s / 60),
            3600..86_400 => format!("{}h ago", s / 3600),
            _ => format!("{}d ago", s / 86_400),
        },
    };
    format!("{text:>6}")
}

fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// What the header shows besides the rows.
pub struct HeaderInfo {
    pub loading: bool,
    pub updated_secs: Option<u64>,
    pub rate: Option<(u32, u32)>,
}

/// Two lines: totals per state, then a pulse strip and live mini bars.
/// `rows` should hold every repo row, also those inside folded groups
/// (see [`crate::tree::repo_rows`]).
pub fn header(rows: &[Row], info: &HeaderInfo, look: &Look, width: u16) -> Vec<Line<'static>> {
    let t = look.theme;
    let repos: Vec<&Row> = rows
        .iter()
        .filter(|r| matches!(r.key, RowKey::Repo(_)))
        .collect();
    let bold = |c| Style::new().fg(c).add_modifier(Modifier::BOLD);

    let mut left = vec![
        Span::styled(" ◆ ", bold(t.accent)),
        Span::styled("popier", bold(t.ink)),
        Span::styled(format!("   {} repos   ", repos.len()), look.fg(t.muted)),
    ];
    let order = [
        (Status::Running, "running"),
        (Status::Failure, "failed"),
        (Status::Queued, "queued"),
        (Status::Success, "ok"),
        (Status::Cancelled, "cancelled"),
    ];
    for (status, word) in order {
        let n = repos.iter().filter(|r| r.status == Some(status)).count();
        if n > 0 {
            left.push(Span::styled(
                format!("{} {n} ", status.icon()),
                bold(t.status(status)),
            ));
            left.push(Span::styled(format!("{word}   "), look.fg(t.muted)));
        }
    }

    let mut right = Vec::new();
    let refresh = if info.loading { look.spinner() } else { "⟳" };
    let ago = info
        .updated_secs
        .map_or("…".to_owned(), |s| format!("{s}s"));
    right.push(Span::styled(format!("{refresh} {ago}"), look.fg(t.muted)));
    if let Some((rem, lim)) = info.rate {
        right.push(Span::styled(
            format!("  ·  api {rem}/{lim}"),
            look.fg(t.faint),
        ));
    }
    right.push(Span::styled("  ·  theme ", look.fg(t.faint)));
    right.push(Span::styled(t.name.clone(), look.fg(t.accent)));
    right.push(Span::raw(" "));

    let used: usize = left.iter().chain(&right).map(|s| s.width()).sum();
    let mut line1 = left;
    if used < width as usize {
        line1.push(Span::raw(" ".repeat(width as usize - used)));
        line1.extend(right);
    }

    let mut line2 = vec![Span::raw(" ")];
    for repo in &repos {
        let color = repo.status.map_or(t.faint, |s| t.status(s));
        line2.push(Span::styled("▮", look.fg(color)));
    }
    line2.push(Span::raw("  "));
    for repo in repos
        .iter()
        .filter(|r| r.status == Some(Status::Running))
        .take(4)
    {
        let Some(f) = repo.gauge.as_ref().and_then(Gauge::fraction) else {
            continue;
        };
        line2.push(Span::styled(
            format!(" {} ", look.spinner()),
            look.fg(t.running),
        ));
        line2.push(Span::styled(format!("{} ", repo.text), look.fg(t.ink)));
        for (text, filled) in theme::bar(f, 10, look.bar, Status::Running) {
            line2.push(Span::styled(
                text,
                look.fg(if filled { t.running } else { t.track }),
            ));
        }
        line2.push(Span::styled(
            format!(" {}%  ", (f * 100.0).floor() as u32),
            look.fg(t.muted),
        ));
    }
    vec![Line::from(line1), Line::from(line2)]
}

/// `key action` pairs: key in accent, action in muted.
pub fn hints(keys: &[(&str, &str)], look: &Look) -> Vec<Span<'static>> {
    let t = look.theme;
    let mut out = vec![Span::raw(" ")];
    for (key, action) in keys {
        out.push(Span::styled(
            (*key).to_owned(),
            Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
        ));
        out.push(Span::styled(format!(" {action}   "), look.fg(t.muted)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{self, Folds};

    #[test]
    fn formats_elapsed() {
        assert_eq!(elapsed(Some(Elapsed::Took(24))), "   24s");
        assert_eq!(elapsed(Some(Elapsed::Took(62))), " 1m02s");
        assert_eq!(elapsed(Some(Elapsed::Took(3900))), " 1h05m");
        assert_eq!(elapsed(Some(Elapsed::Ago(720))), "12m ago");
        assert_eq!(elapsed(None), "      ");
    }

    #[test]
    fn columns_shrink_with_the_terminal() {
        assert_eq!(columns(160), (44, 20));
        assert_eq!(columns(100), (36, 20));
        assert_eq!(columns(90), (32, 10));
        assert_eq!(columns(60).1, 0);
    }

    fn row(depth: u8, key: RowKey) -> Row {
        Row {
            key,
            depth,
            status: None,
            text: String::new(),
            detail: None,
            is_error: false,
            url: None,
            foldable: false,
            expanded: false,
            gauge: None,
            branch: None,
            lead: None,
        }
    }

    #[test]
    fn draws_tree_connectors() {
        let rows = vec![
            row(0, RowKey::Repo("a".into())),
            row(1, RowKey::Workflow(1)),
            row(2, RowKey::Job(1, 0)),
            row(3, RowKey::Step(1, 0, 1)),
            row(2, RowKey::Job(1, 1)),
            row(1, RowKey::Workflow(2)),
            row(0, RowKey::Repo("b".into())),
            row(1, RowKey::Workflow(3)),
        ];
        let p = tree_prefixes(&rows);
        assert_eq!(p[1], "  ├─ ");
        assert_eq!(p[2], "  │   ├─ ");
        assert_eq!(p[3], "  │   │       ");
        assert_eq!(p[4], "  │   └─ ");
        assert_eq!(p[5], "  └─ ");
        assert_eq!(p[7], "  └─ ");
    }

    #[test]
    fn every_row_fits_its_columns() {
        let snap = crate::snapshot::Snapshot { repos: vec![] };
        let rows = tree::build(&snap, &Folds::default());
        assert!(rows.is_empty());
        let theme = Theme::builtin("noite").unwrap();
        let look = Look {
            theme: &theme,
            bar: BarStyle::Line,
            tick: 0,
        };
        let mut repo = row(0, RowKey::Repo("a".into()));
        repo.text = "a-very-long-repository-name-that-will-not-fit".into();
        repo.lead = Some("CI".into());
        repo.status = Some(Status::Running);
        repo.foldable = true;
        repo.gauge = Some(Gauge {
            status: Status::Running,
            done: 5,
            total: 8,
            elapsed: Some(Elapsed::Took(62)),
            hint: None,
        });
        let lines = rows_for(&[repo], &look, 100);
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        // name column (36) + " " + bar (20) + " " + pct (5) + count (8) + time (6)
        assert_eq!(text.chars().count(), 36 + 1 + 20 + 1 + 5 + 8 + 6);
        assert!(text.contains("…"));
        assert!(text.contains("  62%  5/8"));
    }

    fn rows_for(r: &[Row], look: &Look, w: u16) -> Vec<Line<'static>> {
        rows(r, look, w)
    }
}
