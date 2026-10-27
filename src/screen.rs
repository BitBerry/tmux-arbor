//! The cell grid a frame is drawn into. Each frame is rebuilt from scratch
//! and written out whole; tmux does the diffing towards the real terminal.

use std::fmt::Write as _;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Color {
    #[default]
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
}

pub const BOLD: u16 = 1;
pub const DIM: u16 = 1 << 1;
pub const ITALIC: u16 = 1 << 2;
pub const UNDERLINE: u16 = 1 << 3;
pub const UNDER_DOUBLE: u16 = 1 << 4;
pub const UNDER_CURLY: u16 = 1 << 5;
pub const UNDER_DOTTED: u16 = 1 << 6;
pub const UNDER_DASHED: u16 = 1 << 7;
pub const BLINK: u16 = 1 << 8;
pub const REVERSE: u16 = 1 << 9;
pub const HIDDEN: u16 = 1 << 10;
pub const STRIKE: u16 = 1 << 11;
pub const OVERLINE: u16 = 1 << 12;
pub const UNDER_ANY: u16 = UNDERLINE | UNDER_DOUBLE | UNDER_CURLY | UNDER_DOTTED | UNDER_DASHED;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    /// Underline colour.
    pub us: Color,
    pub attrs: u16,
}

impl Style {
    pub fn fg(mut self, c: Color) -> Self {
        self.fg = c;
        self
    }

    pub fn bg(mut self, c: Color) -> Self {
        self.bg = c;
        self
    }

    pub fn us(mut self, c: Color) -> Self {
        self.us = c;
        self
    }

    pub fn with(mut self, a: u16) -> Self {
        self.attrs |= a;
        self
    }

    pub fn without(mut self, a: u16) -> Self {
        self.attrs &= !a;
        self
    }

    /// Sets one underline kind, replacing any other.
    pub fn underline(mut self, kind: u16) -> Self {
        self.attrs = (self.attrs & !UNDER_ANY) | kind;
        self
    }

    fn write_sgr(&self, out: &mut String) {
        out.push_str("\x1b[0");
        for (bit, code) in
            [(BOLD, 1), (DIM, 2), (ITALIC, 3), (BLINK, 5), (REVERSE, 7), (HIDDEN, 8), (STRIKE, 9), (OVERLINE, 53)]
        {
            if self.attrs & bit != 0 {
                let _ = write!(out, ";{code}");
            }
        }
        push_color(out, self.fg, 30, 90, 38);
        push_color(out, self.bg, 40, 100, 48);
        match self.us {
            Color::Default => {}
            Color::Idx(n) => {
                let _ = write!(out, ";58;5;{n}");
            }
            Color::Rgb(r, g, b) => {
                let _ = write!(out, ";58;2;{r};{g};{b}");
            }
        }
        out.push('m');
        let kind = [
            (UNDER_DOUBLE, "4:2"),
            (UNDER_CURLY, "4:3"),
            (UNDER_DOTTED, "4:4"),
            (UNDER_DASHED, "4:5"),
            (UNDERLINE, "4"),
        ]
        .into_iter()
        .find(|(bit, _)| self.attrs & bit != 0);
        if let Some((_, k)) = kind {
            let _ = write!(out, "\x1b[{k}m");
        }
    }
}

fn push_color(out: &mut String, c: Color, base: u8, bright: u8, ext: u8) {
    match c {
        Color::Default => {}
        Color::Idx(n) if n < 8 => {
            let _ = write!(out, ";{}", base + n);
        }
        Color::Idx(n) if n < 16 => {
            let _ = write!(out, ";{}", bright + n - 8);
        }
        Color::Idx(n) => {
            let _ = write!(out, ";{ext};5;{n}");
        }
        Color::Rgb(r, g, b) => {
            let _ = write!(out, ";{ext};2;{r};{g};{b}");
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    /// Empty for the second column of a wide character.
    pub text: String,
    pub style: Style,
}

impl Cell {
    fn blank(style: Style) -> Self {
        Cell { text: " ".into(), style }
    }
}

/// A grid of cells plus, for each cell, what a mouse event there refers to.
#[derive(Clone, Debug)]
pub struct Canvas<H> {
    pub w: u16,
    pub h: u16,
    cells: Vec<Cell>,
    hits: Vec<H>,
}

impl<H: Clone + Default> Canvas<H> {
    pub fn new(w: u16, h: u16, style: Style) -> Self {
        let n = w as usize * h as usize;
        Canvas { w, h, cells: vec![Cell::blank(style); n], hits: vec![H::default(); n] }
    }

    fn idx(&self, x: u16, y: u16) -> Option<usize> {
        (x < self.w && y < self.h).then(|| y as usize * self.w as usize + x as usize)
    }

    pub fn hit(&self, x: u16, y: u16) -> Option<&H> {
        self.idx(x, y).map(|i| &self.hits[i])
    }

    /// Replaces one cell, blanking whatever half of a wide character the
    /// write would leave behind. `cont` marks the second half of a new one.
    fn set(&mut self, x: u16, y: u16, text: String, style: Style, cont: bool) {
        let Some(i) = self.idx(x, y) else { return };
        if !cont && self.cells[i].text.is_empty() && x > 0 {
            self.cells[i - 1].text = " ".into();
        }
        if self.cells[i].text.width() == 2 && x + 1 < self.w {
            self.cells[i + 1].text = " ".into();
        }
        self.cells[i] = Cell { text, style };
    }

    /// Writes `s` from column `x`, stopping before `limit` or the edge.
    /// Returns the column after the last character written.
    pub fn put(&mut self, x: u16, y: u16, s: &str, style: Style, limit: u16) -> u16 {
        let limit = limit.min(self.w);
        let mut x = x;
        if y >= self.h {
            return x;
        }
        for ch in s.chars() {
            let ch = if ch.is_control() { '?' } else { ch };
            let cw = ch.width().unwrap_or(0) as u16;
            if cw == 0 {
                if x > 0
                    && let Some(i) = self.idx(x - 1, y)
                {
                    self.cells[i].text.push(ch);
                }
                continue;
            }
            if x + cw > limit {
                break;
            }
            self.set(x, y, ch.to_string(), style, false);
            if cw == 2 {
                self.set(x + 1, y, String::new(), style, true);
            }
            x += cw;
        }
        x
    }

    pub fn fill(&mut self, x0: u16, x1: u16, y: u16, style: Style) {
        for x in x0..x1.min(self.w) {
            self.set(x, y, " ".into(), style, false);
        }
    }

    pub fn restyle(&mut self, x0: u16, x1: u16, y: u16, f: impl Fn(&mut Style)) {
        for x in x0..x1.min(self.w) {
            if let Some(i) = self.idx(x, y) {
                f(&mut self.cells[i].style);
            }
        }
    }

    pub fn set_hit(&mut self, x0: u16, x1: u16, y: u16, hit: &H) {
        for x in x0..x1.min(self.w) {
            if let Some(i) = self.idx(x, y) {
                self.hits[i] = hit.clone();
            }
        }
    }

    /// Copies `rows` rows of `src` starting at `src_y` to `dst_y`.
    pub fn blit(&mut self, src: &Canvas<H>, src_y: u16, dst_y: u16, rows: u16) {
        let w = self.w.min(src.w) as usize;
        for r in 0..rows {
            let (sy, dy) = (src_y + r, dst_y + r);
            if sy >= src.h || dy >= self.h {
                break;
            }
            for x in 0..w {
                let s = sy as usize * src.w as usize + x;
                let d = dy as usize * self.w as usize + x;
                self.cells[d] = src.cells[s].clone();
                self.hits[d] = src.hits[s].clone();
            }
        }
    }

    /// The whole frame as one synchronized-update write.
    pub fn encode(&self, out: &mut String) {
        out.push_str("\x1b[?2026h");
        let mut cur: Option<Style> = None;
        for y in 0..self.h {
            let _ = write!(out, "\x1b[{};1H", y + 1);
            for x in 0..self.w {
                let c = &self.cells[y as usize * self.w as usize + x as usize];
                if c.text.is_empty() {
                    continue;
                }
                if cur != Some(c.style) {
                    c.style.write_sgr(out);
                    cur = Some(c.style);
                }
                out.push_str(&c.text);
            }
        }
        out.push_str("\x1b[0m\x1b[?2026l");
    }

    /// One row as plain text, for tests and debugging.
    #[cfg(test)]
    pub fn row_text(&self, y: u16) -> String {
        (0..self.w).filter_map(|x| self.idx(x, y)).map(|i| self.cells[i].text.as_str()).collect::<String>()
    }
}

pub fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Cuts `s` to at most `max` columns, ending in "…" when anything was cut.
pub fn truncate(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = take_width(s, max - 1).to_string();
    out.push('…');
    out
}

/// Drops the first `n` columns of `s`. Returns the rest and how many blank
/// columns stand in for a wide character cut in half.
pub fn skip_width(s: &str, n: usize) -> (usize, &str) {
    let mut w = 0;
    for (i, ch) in s.char_indices() {
        if w >= n {
            return (w - n, &s[i..]);
        }
        w += ch.width().unwrap_or(0);
    }
    (w.saturating_sub(n), "")
}

/// The longest prefix of `s` that fits in `max` columns.
pub fn take_width(s: &str, max: usize) -> &str {
    let mut w = 0;
    for (i, ch) in s.char_indices() {
        let cw = ch.width().unwrap_or(0);
        if w + cw > max {
            return &s[..i];
        }
        w += cw;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_characters_take_two_cells() {
        let mut c: Canvas<()> = Canvas::new(6, 1, Style::default());
        let end = c.put(0, 0, "窗口x", Style::default(), 6);
        assert_eq!(end, 5);
        assert_eq!(c.row_text(0), "窗口x ");
        // Overwriting the second half of a wide character blanks the first.
        c.put(1, 0, "a", Style::default(), 6);
        assert_eq!(c.row_text(0), " a口x ");
    }

    #[test]
    fn put_stops_at_limit() {
        let mut c: Canvas<()> = Canvas::new(8, 1, Style::default());
        assert_eq!(c.put(0, 0, "abcdef", Style::default(), 4), 4);
        assert_eq!(c.row_text(0), "abcd    ");
        assert_eq!(c.put(3, 0, "窗", Style::default(), 4), 3);
    }

    #[test]
    fn skipping_columns() {
        assert_eq!(skip_width("abcdef", 2), (0, "cdef"));
        assert_eq!(skip_width("窗口ab", 1), (1, "口ab"));
        assert_eq!(skip_width("窗口ab", 4), (0, "ab"));
        assert_eq!(skip_width("ab", 5), (0, ""));
    }

    #[test]
    fn truncation() {
        assert_eq!(truncate("editor", 10), "editor");
        assert_eq!(truncate("editor", 4), "edi…");
        assert_eq!(truncate("窗口名字", 5), "窗口…");
        assert_eq!(truncate("x", 0), "");
    }
}
