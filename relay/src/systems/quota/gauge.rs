//! One meter as the boards draw it: a bar in eighths of a cell with `│` at its floor, how full it
//! is and how long until it resets. `view status` writes it as text, `view watch` as a styled line.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::config::Limits;
use crate::harness::Meter;

/// The cells a bar takes when there is room, and the fewest it shrinks to.
pub const BAR: usize = 12;
pub const MIN_BAR: usize = 5;
/// What follows the bar: ` 100%* 1d17h`, the time left padded to five columns. A longer time
/// left (a monthly meter's `29d23h`) makes it wider.
pub const TAIL: usize = 12;

/// What a board shows where an account has no such meter.
pub const MISSING: &str = "·";

/// A cell partly filled, by eighths.
const EIGHTHS: [char; 8] = ['░', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];

#[derive(Debug, Clone, PartialEq)]
pub struct Gauge {
    /// How much of the window is spent, 0 to 1.
    pub used: f64,
    /// Where dispatch stops on this meter now (`Limits::floor_for`), when a project's rules judge it.
    pub floor: Option<f64>,
    /// Seconds until it resets, when it says.
    pub left: Option<i64>,
    /// Whether it is the meter that decides (`accounts::at_floor`, else `accounts::tightest`).
    pub governing: bool,
}

/// How close a meter is to its floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Room,
    /// Within ten points of it.
    Near,
    /// At or over it.
    Over,
}

impl Gauge {
    /// `m` at `now_s`, its floor by `limits` when a project's rules apply.
    pub fn new(m: &Meter, limits: Option<&Limits>, governing: bool, now_s: i64) -> Self {
        Self {
            used: m.utilization,
            floor: limits.map(|l| l.floor_for(m.resets_at, now_s).0),
            left: m.resets_at.map(|r| r - now_s),
            governing,
        }
    }

    /// Against its floor, or against the whole window where no floor is known.
    pub fn tone(&self) -> Tone {
        let floor = self.floor.unwrap_or(1.0);
        if self.used >= floor {
            Tone::Over
        } else if self.used >= floor - 0.1 {
            Tone::Near
        } else {
            Tone::Room
        }
    }

    /// The bar alone, `cells` wide: `███▍░░░░░░│░`.
    pub fn bar(&self, cells: usize) -> String {
        // The cell the floor falls in.
        let mark = self.floor.map(|f| (share(f, cells * 8) / 8).min(cells.saturating_sub(1)));
        let filled = share(self.used, cells * 8);
        let cell = |i: usize| match filled.saturating_sub(i * 8) {
            n if n >= 8 => '█',
            n => EIGHTHS[n],
        };
        (0..cells).map(|i| if Some(i) == mark { '│' } else { cell(i) }).collect()
    }

    /// The bar `cells` wide then the tail, the meter that decides marked with `*`: for text
    /// that may be piped, where no bold shows.
    pub fn text(&self, cells: usize) -> String {
        format!("{}{}", self.bar(cells), self.tail(self.governing))
    }

    /// The gauge in `width` columns, its bar shrunk first (to [`MIN_BAR`]) so the whole tail
    /// fits, coloured by its [`Tone`] and bold when it decides.
    pub fn line(&self, width: usize) -> Line<'static> {
        let tail = self.tail(false);
        let cells = width.saturating_sub(tail.chars().count()).clamp(MIN_BAR, BAR);
        let colour = match self.tone() {
            Tone::Room => Color::Green,
            Tone::Over => Color::Red,
            Tone::Near => Color::Yellow,
        };
        let style = if self.governing { Style::new().fg(colour).bold() } else { Style::new().fg(colour) };
        let tail: String = tail.chars().take(width.saturating_sub(cells)).collect();
        Line::from(vec![
            Span::styled(self.bar(cells).chars().take(width).collect::<String>(), style),
            Span::styled(tail, style),
        ])
    }

    /// ` 31%* 5d4h `: how full, the mark (or a space), and the time left.
    fn tail(&self, marked: bool) -> String {
        let left =
            self.left.map_or(String::new(), |s| if s > 0 { crate::accounts::fmt_left(s) } else { "reset".into() });
        let pct = format!("{:.0}%", self.used * 100.0);
        format!(" {pct:>4}{} {left:<5}", if marked { "*" } else { " " })
    }
}

/// `fraction` of `n`, rounded, kept within `0..=n`.
fn share(fraction: f64, n: usize) -> usize {
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a bar is a few dozen eighths"
    )]
    let k = (fraction.clamp(0.0, 1.0) * n as f64).round() as usize;
    k.min(n)
}

/// A meter window's column heading: claude's and puku's names in words, any other as named.
pub fn heading(window: &str) -> String {
    match window {
        "five_hour" => "5 hours".into(),
        "seven_day" => "7 days".into(),
        other => other.replace('_', " "),
    }
}

/// Where a window's column goes: the short windows first, the others by name.
pub fn order(window: &str) -> (u8, &str) {
    let rank = match window {
        "five_hour" => 0,
        "seven_day" => 1,
        "monthly" => 2,
        _ => 3,
    };
    (rank, window)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gauge(used: f64, floor: Option<f64>) -> Gauge {
        Gauge { used, floor, left: Some(5 * 86_400 + 4 * 3600), governing: false }
    }

    #[test]
    fn a_bar_fills_by_eighths_and_marks_its_floor() {
        assert_eq!(gauge(0.31, Some(0.9)).bar(12), "███▊░░░░░░│░");
        assert_eq!(gauge(0.0, Some(0.9)).bar(12), "░░░░░░░░░░│░");
        assert_eq!(gauge(1.0, None).bar(5), "█████", "no floor, no mark");
        assert_eq!(gauge(0.99, Some(0.98)).bar(12), "███████████│", "a floor near the end marks the last cell");
        assert_eq!(gauge(1.4, Some(0.9)).bar(5), "████│", "full is full");
    }

    #[test]
    fn the_meter_that_decides_is_marked_in_text() {
        let g = Gauge { governing: true, ..gauge(0.31, Some(0.9)) };
        assert_eq!(g.text(12), "███▊░░░░░░│░  31%* 5d4h ");
        assert_eq!(gauge(0.31, Some(0.9)).text(5), "█▌░░│  31%  5d4h ");
        let soon = Gauge { left: Some(3_420), ..gauge(0.09, Some(0.98)) };
        assert_eq!(soon.text(12), "█▏░░░░░░░░░│   9%  57m  ");
    }

    #[test]
    fn its_tone_is_its_distance_to_the_floor() {
        assert_eq!(gauge(0.79, Some(0.9)).tone(), Tone::Room);
        assert_eq!(gauge(0.8, Some(0.9)).tone(), Tone::Near);
        assert_eq!(gauge(0.9, Some(0.9)).tone(), Tone::Over);
        assert_eq!(gauge(0.95, None).tone(), Tone::Near, "machine-wide, against the whole window");
    }

    #[test]
    fn a_narrow_line_shrinks_the_bar_first() {
        let text = |l: Line<'_>| l.spans.iter().map(|s| s.content.to_string()).collect::<String>();
        assert_eq!(text(gauge(0.31, Some(0.9)).line(24)), "███▊░░░░░░│░  31%  5d4h ");
        assert_eq!(text(gauge(0.31, Some(0.9)).line(20)), "██▌░░░░│  31%  5d4h ");
        assert_eq!(text(gauge(0.31, Some(0.9)).line(9)), "█▌░░│  31", "never fewer than five cells");
        let bold = Gauge { governing: true, ..gauge(0.31, Some(0.9)) }.line(24);
        assert!(bold.spans.iter().all(|s| s.style.add_modifier.contains(ratatui::style::Modifier::BOLD)));
    }

    /// A monthly meter can be `29d23h` from its reset: its tail is wider, and the bar gives way.
    #[test]
    fn a_monthly_reset_keeps_its_unit() {
        let g = Gauge { left: Some(29 * 86_400 + 23 * 3600), ..gauge(0.31, Some(0.9)) };
        assert_eq!(g.text(12), "███▊░░░░░░│░  31%  29d23h");
        let text = |l: Line<'_>| l.spans.iter().map(|s| s.content.to_string()).collect::<String>();
        assert_eq!(text(g.line(24)), "███▍░░░░░│░  31%  29d23h", "a bar a cell shorter, the whole time left");
        assert_eq!(text(g.line(18)), "█▌░░│  31%  29d23h", "at five cells, still whole");
    }

    #[test]
    fn windows_read_as_words_shortest_first() {
        let mut w = ["seven_day", "monthly", "five_hour", "openai-codex/5 hours"];
        w.sort_by_key(|w| order(w));
        assert_eq!(
            w.iter().map(|w| heading(w)).collect::<Vec<_>>(),
            ["5 hours", "7 days", "monthly", "openai-codex/5 hours"]
        );
    }
}
