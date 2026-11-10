//! The board's panes as one construct, and the pure layout they need; no terminal here.
//!
//! A pane is drawn somewhere, shows a viewport of its rows and remembers how far
//! it is scrolled. Text is counted after wrapping, so the last row of any answer
//! can be scrolled to; every pane scrolls the same way.

use ratatui::layout::Rect;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// How far a key or the wheel moves a pane (or the selection in the jobs pane).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Rows, down when positive.
    Rows(isize),
    /// Viewports less one row, down when positive.
    Pages(isize),
    Top,
    Bottom,
}

/// One pane, as the last frame drew it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Pane {
    /// Where it was drawn, its border included.
    pub area: Rect,
    /// The rows it shows at once.
    pub view: usize,
    /// The rows its content takes, wrapped.
    pub rows: usize,
    /// The rows scrolled off its top.
    pub offset: usize,
}

impl Pane {
    /// Records where a frame puts the pane and what it holds, keeping the offset in range.
    pub fn place(&mut self, area: Rect, view: usize, rows: usize) {
        (self.area, self.view, self.rows) = (area, view, rows);
        self.offset = self.offset.min(self.last_offset());
    }

    /// As [`Pane::place`], keeping row `keep` in view when it was in view before: a smaller
    /// window does not hide the selection, while one scrolled away by the wheel stays away.
    pub fn place_keeping(&mut self, area: Rect, view: usize, rows: usize, keep: Option<usize>) {
        let shown = keep.filter(|&i| i >= self.offset && i < self.offset + self.view);
        self.place(area, view, rows);
        if let Some(i) = shown {
            self.reveal(i);
        }
    }

    /// The furthest it scrolls: its last row at the bottom.
    pub fn last_offset(&self) -> usize {
        self.rows.saturating_sub(self.view)
    }

    /// Where `step` takes position `at` in `0..len`, a page being this pane's viewport less one row.
    pub fn stepped(&self, at: usize, step: Step, len: usize) -> usize {
        let page = isize::try_from(self.view.saturating_sub(1).max(1)).unwrap_or(isize::MAX);
        let to = match step {
            Step::Rows(n) => at.saturating_add_signed(n),
            Step::Pages(n) => at.saturating_add_signed(n.saturating_mul(page)),
            Step::Top => 0,
            Step::Bottom => usize::MAX,
        };
        to.min(len.saturating_sub(1))
    }

    pub fn scroll(&mut self, step: Step) {
        self.offset = self.stepped(self.offset, step, self.last_offset() + 1);
    }

    /// Scrolls just enough that row `i` is in view.
    pub fn reveal(&mut self, i: usize) {
        if i < self.offset {
            self.offset = i;
        } else if self.view > 0 && i >= self.offset + self.view {
            self.offset = i + 1 - self.view;
        }
    }

    /// `12-30 of 164` when it does not all fit.
    pub fn position(&self) -> Option<String> {
        (self.view > 0 && self.rows > self.view)
            .then(|| format!("{}-{} of {}", self.offset + 1, (self.offset + self.view).min(self.rows), self.rows))
    }
}

/// Display columns of `s`, as a terminal draws it.
pub fn width(s: &str) -> usize {
    s.chars().filter_map(UnicodeWidthChar::width).sum()
}

/// `line` in rows at most `cols` wide, broken at the last space that fits or inside a word
/// longer than a row; tabs are four spaces and other control characters are dropped.
pub fn wrap(line: &str, cols: usize) -> Vec<String> {
    let cols = cols.max(1);
    let (mut rows, mut row, mut w) = (Vec::new(), String::new(), 0);
    // Where the row's last space is, as a byte index.
    let mut space: Option<usize> = None;
    // Trailing spaces show nothing, and would only wrap into an empty row.
    for c in line.trim_end().replace('\t', "    ").chars() {
        let Some(cw) = c.width() else { continue };
        if w + cw > cols && !row.is_empty() {
            let rest = match space.filter(|&s| s > 0 && c != ' ') {
                Some(s) => {
                    let rest = row[s + 1..].to_owned();
                    row.truncate(s);
                    rest
                }
                None => String::new(),
            };
            rows.push(std::mem::replace(&mut row, rest));
            (w, space) = (row.width(), None);
            if c == ' ' {
                continue;
            }
        }
        if c == ' ' {
            space = Some(row.len());
        }
        row.push(c);
        w += cw;
    }
    rows.push(row);
    rows
}

/// `s` on one line in at most `cols` columns, its end replaced by `…` when it does not fit;
/// line breaks and other control characters become spaces.
pub fn cut(s: &str, cols: usize) -> String {
    let flat: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    if width(&flat) <= cols {
        return flat;
    }
    let mut out = String::new();
    let mut w = 0;
    for c in flat.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw + 1 > cols {
            break;
        }
        out.push(c);
        w += cw;
    }
    if cols > 0 {
        out.push('…');
    }
    out
}

/// A table column: its heading and the most it is ever widened to; the last column takes the
/// rest, and is left at least its cap first when it has one (else [`LAST_MIN`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub head: std::borrow::Cow<'static, str>,
    pub cap: usize,
    /// The width it gives way to before any other column is cut (a gauge's, its bar at its
    /// shortest); 0 when it gives way only as the others do.
    pub yields: usize,
}

/// The narrowest a column but the last is squeezed to, and the room the last one is left first.
const FLOOR: usize = 4;
const LAST_MIN: usize = 12;

/// Each column's width in `avail` columns with `gap` between them: as wide as its widest cell or
/// heading up to its cap, the last taking the rest. When they do not fit, the widest give way
/// first, down to a floor, so the last keeps some room: its cap, when it has one. A column that
/// [`Column::yields`] gives way before those, down to that width.
pub fn widths<R: AsRef<[String]>>(cols: &[Column], rows: &[R], avail: usize, gap: usize) -> Vec<usize> {
    let Some(last) = cols.len().checked_sub(1) else { return Vec::new() };
    let widest = |c: usize| rows.iter().filter_map(|r| r.as_ref().get(c)).map(|s| width(s)).max().unwrap_or(0);
    let content: Vec<usize> = (0..last).map(widest).collect();
    let mut w: Vec<usize> =
        cols[..last].iter().zip(&content).map(|(col, &c)| c.max(col.head.width()).min(col.cap)).collect();
    let used = |w: &[usize]| w.iter().sum::<usize>() + gap * last;
    let last_min = if cols[last].cap > 0 { cols[last].cap } else { LAST_MIN };
    while used(&w) + last_min > avail {
        // A heading wider than its cells is cut first, then a column that yields, then the widest.
        let slack =
            (0..last).filter(|&i| w[i] > content[i].max(2)).max_by_key(|&i| (w[i] - content[i], std::cmp::Reverse(i)));
        let yields = || {
            let room = |i: usize| w[i].saturating_sub(cols[i].yields);
            (0..last).filter(|&i| cols[i].yields > 0 && room(i) > 0).max_by_key(|&i| (room(i), std::cmp::Reverse(i)))
        };
        let widest = || (0..last).filter(|&i| w[i] > FLOOR).max_by_key(|&i| (w[i], std::cmp::Reverse(i)));
        let Some(i) = slack.or_else(yields).or_else(widest) else { break };
        w[i] -= 1;
    }
    w.push(avail.saturating_sub(used(&w)));
    w
}

/// How an answer's line is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Heading,
    Code,
}

/// An answer's lines as the board shows them: a heading without its `#`s, a fenced block's lines
/// as they are (the fences dropped), and elsewhere `**` and backticks dropped.
pub fn markdown(text: &str) -> Vec<(String, Tone)> {
    let mut code = false;
    let mut out = Vec::new();
    for line in text.lines() {
        let bare = line.trim_start();
        if bare.starts_with("```") || bare.starts_with("~~~") {
            code = !code;
            continue;
        }
        if code {
            out.push((line.to_owned(), Tone::Code));
            continue;
        }
        let plain = line.replace("**", "").replace('`', "");
        let hashes = bare.chars().take_while(|&c| c == '#').count();
        let heading = (1..=6).contains(&hashes) && bare[hashes..].chars().next().is_none_or(|c| c == ' ');
        if heading {
            out.push((plain.trim_start().trim_start_matches('#').trim().to_owned(), Tone::Heading));
        } else {
            out.push((plain, Tone::Plain));
        }
    }
    out
}

/// The rows each pane gets in a window `height` tall, the key line taking one: given the rows the
/// jobs, the decisions waiting and the accounts would fill (borders and headings included), each
/// capped (two fifths, a sixth, a quarter of the window), the job pane taking the rest and at
/// least five rows when the window has them. Returns `[jobs, decisions, job, accounts]`.
pub fn heights(height: u16, jobs: u16, decisions: u16, accounts: u16) -> [u16; 4] {
    let avail = height.saturating_sub(1);
    let cap = |want: u16, share: u16, least: u16| want.min((height / share).max(least));
    let mut h = [jobs.min((height.saturating_mul(2) / 5).max(4)), cap(decisions, 6, 3), 0, cap(accounts, 4, 4)];
    // Room for the job pane: the accounts give way first, then the jobs, then the decisions.
    for (i, floor) in [(3, 3), (0, 4), (1, 0), (3, 0), (0, 0)] {
        while h[0] + h[1] + h[3] + 5 > avail && h[i] > floor {
            h[i] -= 1;
        }
    }
    h[2] = avail.saturating_sub(h[0] + h[1] + h[3]);
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(view: usize, rows: usize) -> Pane {
        let mut p = Pane::default();
        p.place(Rect::default(), view, rows);
        p
    }

    #[test]
    fn a_pane_scrolls_to_its_last_row_and_no_further() {
        let mut p = pane(10, 164);
        p.scroll(Step::Bottom);
        assert_eq!(p.offset, 154, "the last row at the bottom");
        p.scroll(Step::Pages(3));
        assert_eq!(p.offset, 154);
        p.scroll(Step::Top);
        p.scroll(Step::Pages(1));
        assert_eq!((p.offset, p.position().as_deref()), (9, Some("10-19 of 164")), "a page keeps one row");
        p.scroll(Step::Rows(-20));
        assert_eq!(p.offset, 0);
        p.place(Rect::default(), 10, 12);
        assert_eq!(p.offset, 0, "less content pulls the offset back");
        p.scroll(Step::Rows(5));
        assert_eq!(p.offset, 2);
        assert_eq!(pane(10, 4).position(), None, "it all fits: no position");
    }

    #[test]
    fn a_selection_steps_within_its_list_and_is_revealed() {
        let mut p = pane(5, 47);
        assert_eq!(p.stepped(0, Step::Rows(-1), 47), 0);
        assert_eq!(p.stepped(3, Step::Pages(1), 47), 7);
        assert_eq!(p.stepped(3, Step::Bottom, 47), 46);
        assert_eq!(p.stepped(0, Step::Rows(1), 0), 0, "an empty list stays at 0");
        p.reveal(13);
        assert_eq!(p.offset, 9, "the row is the last in view");
        p.reveal(2);
        assert_eq!(p.offset, 2);
    }

    #[test]
    fn a_shrinking_pane_keeps_a_row_that_was_in_view() {
        let mut p = pane(13, 60);
        p.reveal(59);
        p.place_keeping(Rect::default(), 6, 60, Some(59));
        assert_eq!(p.offset, 54, "the selection is still the last row in view");
        p.scroll(Step::Top);
        p.place_keeping(Rect::default(), 3, 60, Some(59));
        assert_eq!(p.offset, 0, "a row scrolled away stays away");
    }

    #[test]
    fn wrapping_breaks_at_spaces_by_display_width() {
        assert_eq!(wrap("the quick brown fox", 9), ["the quick", "brown fox"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"], "a long word is broken");
        assert_eq!(wrap("", 4), [""], "an empty line is one row");
        assert_eq!(wrap("日本語の文", 4), ["日本", "語の", "文"], "wide characters take two columns");
        assert_eq!(wrap("a\tb\r", 10), ["a    b"]);
        assert_eq!(wrap("full row ", 8), ["full row"], "a trailing space adds no row");
        assert!(wrap(&"word ".repeat(100), 37).iter().all(|r| width(r) <= 37));
    }

    #[test]
    fn a_cut_cell_ends_in_an_ellipsis_within_its_width() {
        assert_eq!(cut("claude:tanjim.thepenguin", 12), "claude:tanj…");
        assert_eq!(cut("short", 12), "short");
        assert_eq!(cut("日本語の文字", 7), "日本語…");
        assert_eq!(cut("two\nlines", 20), "two lines");
    }

    #[test]
    fn columns_fit_their_content_up_to_a_cap_and_give_way_to_the_last() {
        let cols = [
            Column { head: "job".into(), cap: 10, yields: 0 },
            Column { head: "account".into(), cap: 20, yields: 0 },
            Column { head: "task".into(), cap: 0, yields: 0 },
        ];
        let rows = vec![vec!["build".into(), "usage unreadable (sign it in, or list it)".into(), "Build it".into()]];
        assert_eq!(widths(&cols, &rows, 80, 2), [5, 20, 51]);
        assert_eq!(widths(&cols, &rows, 30, 2), [5, 9, 12], "the widest gives way first");
        assert_eq!(widths::<Vec<String>>(&cols, &[], 80, 2), [3, 7, 66], "as wide as the headings");
        assert_eq!(widths(&cols, &rows, 6, 2), [4, 4, 0], "never below the floor, never a panic");
        let rows = vec![vec!["a".into(), "claude:solo".into(), "t".into()]];
        let cols = [
            Column { head: "project".into(), cap: 16, yields: 0 },
            Column { head: "account".into(), cap: 20, yields: 0 },
            Column { head: "task".into(), cap: 0, yields: 0 },
        ];
        assert_eq!(widths(&cols, &rows, 32, 2), [5, 11, 12], "a heading wider than its cells gives way first");
        let cols =
            [Column { head: "account".into(), cap: 20, yields: 0 }, Column { head: "run".into(), cap: 3, yields: 0 }];
        let rows = vec![vec!["claude:tanjim.thepenguins".into(), "1".into()]];
        assert_eq!(widths(&cols, &rows, 26, 2), [20, 4], "a last column with a cap is left that much first");
        let gauge = Column { head: "7 days".into(), cap: 24, yields: 17 };
        let cols = [Column { head: "account".into(), cap: 24, yields: 0 }, gauge.clone(), gauge, cols[1].clone()];
        let rows = vec![vec!["a".repeat(24), "g".repeat(24), "g".repeat(24), "1".into()]];
        assert_eq!(widths(&cols, &rows, 75, 2), [24, 21, 21, 3], "the gauges give way first");
        assert_eq!(widths(&cols, &rows, 60, 2), [17, 17, 17, 3], "then, at their shortest, as the others do");
    }

    #[test]
    fn markdown_markers_are_dropped_and_headings_marked() {
        let text = "## What survives\nsome **bold** and `code`\n```rust\nlet x = **y;\n```\n#hashtag";
        assert_eq!(
            markdown(text),
            [
                ("What survives".to_owned(), Tone::Heading),
                ("some bold and code".to_owned(), Tone::Plain),
                ("let x = **y;".to_owned(), Tone::Code),
                ("#hashtag".to_owned(), Tone::Plain),
            ]
        );
    }

    #[test]
    fn the_job_pane_keeps_five_rows_and_the_accounts_a_quarter() {
        assert_eq!(heights(24, 50, 0, 15), [9, 0, 8, 6]);
        assert_eq!(heights(40, 50, 4, 15), [16, 4, 9, 10]);
        assert_eq!(heights(30, 4, 0, 15), [4, 0, 18, 7]);
        let [jobs, _, job, accounts] = heights(12, 20, 0, 9);
        assert_eq!((job, jobs + job + accounts), (5, 11), "a small window still has the job pane");
        assert_eq!(heights(3, 20, 3, 9).iter().sum::<u16>(), 2, "a tiny window: no panic, no overflow");
    }
}
