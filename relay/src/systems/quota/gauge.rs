//! One meter as the boards draw it: a bar in eighths of a cell with `│` at its floor, how full it
//! is and how long until it resets. `view status` writes it as text, `view watch` as a styled line.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::config::Limits;
use crate::harness::Meter;

/// The cells a bar takes when there is room, and the fewest it shrinks to.
pub const BAR: usize = 12;
pub const MIN_BAR: usize = 5;
/// What follows the bar: ` 100%* 1d17h`, the time left padded to five columns, which
/// [`crate::accounts::fmt_left`] never exceeds, so the tail always fits whole.
pub const TAIL: usize = 12;

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

    /// How far past its floor it is (or its whole window, where no floor is known): the
    /// closest to its floor is the greatest, negative while there is room.
    pub fn past_floor(&self) -> f64 {
        self.used - self.floor.unwrap_or(1.0)
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

/// A meter's name as a board shows it: claude's and puku's in words, any other as named.
pub fn heading(window: &str) -> String {
    match window {
        "five_hour" => "5 hours".into(),
        "seven_day" => "7 days".into(),
        other => other.replace('_', " "),
    }
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

    /// A monthly meter is weeks from its reset: its time left is in days alone, so the tail keeps
    /// its width and its unit, down to the narrowest gauge a board lays out (review of #42).
    #[test]
    fn a_monthly_reset_keeps_its_unit() {
        let g = Gauge { left: Some(29 * 86_400 + 23 * 3600), ..gauge(0.31, Some(0.9)) };
        assert_eq!(g.text(12), "███▊░░░░░░│░  31%  30d  ");
        let text = |l: Line<'_>| l.spans.iter().map(|s| s.content.to_string()).collect::<String>();
        assert_eq!(text(g.line(BAR + TAIL)), "███▊░░░░░░│░  31%  30d  ");
        assert_eq!(text(g.line(MIN_BAR + TAIL)), "█▌░░│  31%  30d  ", "at the narrowest, still whole");
    }

    #[test]
    fn a_meter_reads_as_words() {
        let names = ["five_hour", "seven_day", "seven_day_overage_included", "openai-codex/5 hours"];
        let words = ["5 hours", "7 days", "seven day overage included", "openai-codex/5 hours"];
        assert_eq!(names.map(heading), words);
    }

    #[test]
    fn the_closest_to_its_floor_is_the_most_past_it() {
        let (near_reset, far) = (gauge(0.95, Some(0.98)), gauge(0.9, Some(0.9)));
        assert!(far.past_floor() > near_reset.past_floor(), "at its floor, past one three points off");
        assert!(near_reset.past_floor() > gauge(0.85, Some(0.9)).past_floor(), "three points off, past five");
        assert!((gauge(0.5, None).past_floor() + 0.5).abs() < 1e-9, "machine-wide, against the whole window");
    }
}
