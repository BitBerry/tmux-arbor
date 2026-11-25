//! Lays a 2-level Directory -> Pane snapshot out into cells, in one of the
//! three styles (indent, tree, blocks) or as the 7-column rail, plus the
//! header and footer around the list.

use std::cell::{Cell, RefCell};
use std::time::Duration;

use crate::config::{GroupBy, Variant};
use crate::model::{DirGroup, NodeKey, PaneItem, Snapshot};
use crate::screen::{
    BOLD, Canvas, Color, DIM, OVERLINE, Style, UNDER_DASHED, UNDERLINE, skip_width, take_width, truncate, width,
};
use crate::strings::Strings;
use crate::theme::Palette;

use crate::tmux::SEP;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Hit {
    #[default]
    None,
    Dir(String),
    DirAdd(String),
    Pane {
        wid: String,
        pid: String,
        dir: String,
    },
    PaneRename {
        wid: String,
        pid: String,
    },
    NewWindow,
    /// A split button: 'h' or 'v'.
    Split(char),
}

impl Hit {
    /// The tree node a cell belongs to, if any.
    pub fn node(&self) -> Option<NodeKey> {
        match self {
            Hit::Dir(d) | Hit::DirAdd(d) => Some(NodeKey::Dir(d.clone())),
            Hit::Pane { pid, .. } | Hit::PaneRename { pid, .. } => Some(NodeKey::Pane(pid.clone())),
            _ => None,
        }
    }
}

/// Where a dragged node would land.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    DirBefore(String),
    DirAfter(String),
    Swap(String),
    Join { wid: String, pid: String },
    Break,
    Invalid,
}

#[derive(Clone, Debug)]
pub struct Drag {
    pub src: NodeKey,
    pub src_wid: String,
    pub src_dir: String,
    pub src_win_panes: usize,
    pub target: Option<Target>,
}

pub struct Ctx<'a> {
    pub snap: &'a Snapshot,
    pub groups: &'a [DirGroup],
    pub pal: &'a Palette,
    pub text: &'a Strings,
    pub home: Option<&'a str>,
    pub width: u16,
    pub rail: bool,
    pub variant: Variant,
    pub label_styles: bool,
    /// Only set while the sidebar holds the keyboard.
    pub cursor: Option<&'a NodeKey>,
    pub hover: Option<&'a NodeKey>,
    /// The `+` button on a directory row currently under the pointer.
    pub hover_add: Option<&'a str>,
    /// The `✎` rename button on a pane row currently under the pointer.
    pub hover_rename: Option<&'a str>,
    /// The split button under the pointer.
    pub hover_split: Option<char>,
    pub drag: Option<&'a Drag>,
    /// Active inline pane rename state, if editing.
    pub rename: Option<&'a Rename>,
    /// List-relative hardware cursor position `(x, y)` during inline rename.
    pub edit_cursor: Cell<Option<(u16, u16)>>,
    /// The node whose cut-off text scrolls, and for how long it has.
    pub marquee: Option<(&'a NodeKey, Duration)>,
    /// Set while drawing: how many columns the marquee text overflows by.
    pub overflow: Cell<Option<u16>>,
    /// The node whose cut-off text a popup shows whole, when tmux can
    /// float one (instead of the marquee).
    pub tip_for: Option<&'a NodeKey>,
    /// Set while drawing: where that cut-off text sits, and all of it.
    pub tip: RefCell<Option<Tip>>,
}

/// Inline editor state for renaming a pane (`@arbor-name`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rename {
    pub pid: String,
    pub buf: String,
    pub cursor: usize,
    pub select_all: bool,
}

impl Rename {
    pub fn new(pid: String, initial: String) -> Self {
        let cursor = initial.len();
        let select_all = !initial.is_empty();
        Rename { pid, buf: initial, cursor, select_all }
    }

    pub fn insert(&mut self, ch: char) {
        if ch.is_control() || ch == SEP {
            return;
        }
        if self.select_all {
            self.buf.clear();
            self.cursor = 0;
            self.select_all = false;
        }
        self.buf.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    pub fn backspace(&mut self) {
        if self.select_all {
            self.clear();
            return;
        }
        if self.cursor == 0 {
            return;
        }
        if let Some((prev, _)) = self.buf[..self.cursor].char_indices().next_back() {
            self.buf.drain(prev..self.cursor);
            self.cursor = prev;
        }
    }

    pub fn delete(&mut self) {
        if self.select_all {
            self.clear();
            return;
        }
        if self.cursor >= self.buf.len() {
            return;
        }
        if let Some(ch) = self.buf[self.cursor..].chars().next() {
            self.buf.drain(self.cursor..self.cursor + ch.len_utf8());
        }
    }

    pub fn move_left(&mut self) {
        self.select_all = false;
        if let Some((prev, _)) = self.buf[..self.cursor].char_indices().next_back() {
            self.cursor = prev;
        }
    }

    pub fn move_right(&mut self) {
        self.select_all = false;
        if let Some(ch) = self.buf[self.cursor..].chars().next() {
            self.cursor += ch.len_utf8();
        }
    }

    pub fn home(&mut self) {
        self.select_all = false;
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.select_all = false;
        self.cursor = self.buf.len();
    }

    pub fn clear(&mut self) {
        self.buf.clear();
        self.cursor = 0;
        self.select_all = false;
    }

    pub fn kill_to_end(&mut self) {
        self.select_all = false;
        self.buf.truncate(self.cursor);
    }

    pub fn delete_word(&mut self) {
        if self.select_all {
            self.clear();
            return;
        }
        let head = &self.buf[..self.cursor];
        let trimmed = head.trim_end();
        let cut = trimmed
            .rfind(char::is_whitespace)
            .map(|i| i + trimmed[i..].chars().next().map_or(0, char::len_utf8))
            .unwrap_or(0);
        self.buf.drain(cut..self.cursor);
        self.cursor = cut;
    }

    pub fn sanitized(&self) -> String {
        self.buf.chars().filter(|&c| c != '\r' && c != '\n' && c != SEP).collect::<String>().trim().to_string()
    }

    /// Returns `(pad_spaces, visible_slice, cursor_col_offset)` scrolled to keep `cursor` inside `avail`.
    pub fn visible_window(&self, avail: usize) -> (usize, &str, usize) {
        if avail == 0 {
            return (0, "", 0);
        }
        let prefix_cols = width(&self.buf[..self.cursor]);
        let total_cols = width(&self.buf);
        if total_cols < avail {
            return (0, &self.buf, prefix_cols);
        }
        let scroll_cols = prefix_cols.saturating_sub(avail.saturating_sub(1));
        let (pad, rest) = skip_width(&self.buf, scroll_cols);
        let rest_offset = self.buf.len() - rest.len();
        let caret_col = pad + width(&self.buf[rest_offset..self.cursor]);
        let slice = take_width(rest, avail.saturating_sub(pad));
        (pad, slice, caret_col)
    }
}

/// Cut-off text the popup shows, in list coordinates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tip {
    pub y: u16,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct Group {
    pub dir: String,
    pub y0: u16,
    pub y1: u16,
}

/// The tree drawn on a canvas as tall as its content.
pub struct List {
    pub canvas: Canvas<Hit>,
    pub groups: Vec<Group>,
    pub nodes: Vec<(NodeKey, u16, u16)>,
}

struct Look {
    base: Style,
    sub: Color,
    plain: bool,
}

impl Ctx<'_> {
    pub fn plain(&self) -> Style {
        Style::default().bg(self.pal.bg)
    }

    fn is_cursor(&self, k: &NodeKey) -> bool {
        self.cursor == Some(k)
    }

    fn is_hover(&self, k: &NodeKey) -> bool {
        self.drag.is_none() && self.hover == Some(k)
    }

    fn target(&self) -> Option<&Target> {
        self.drag.and_then(|d| d.target.as_ref())
    }

    fn drop_into_pane(&self, pid: &str) -> bool {
        matches!(self.target(), Some(Target::Swap(p)) if p == pid)
            || matches!(self.target(), Some(Target::Join { pid: p, .. }) if p == pid)
    }

    fn dir_look(&self, g: &DirGroup) -> Look {
        let p = self.pal;
        let key = NodeKey::Dir(g.raw_path.clone());
        let mut look = Look { base: Style::default().bg(p.dir_bg).fg(p.dir_fg).with(BOLD), sub: p.muted, plain: true };
        if g.collapsed && g.has_focused {
            look.base = Style::default().bg(p.cur_win_bg).fg(p.cur_win_fg).with(BOLD);
            look.sub = p.cur_win_sub;
        } else if self.is_hover(&key) {
            look.base = look.base.bg(p.hover_bg);
        }
        if self.is_cursor(&key) {
            look.base = Style::default().bg(p.cursor_bg).fg(p.cursor_fg).with(BOLD);
            look.sub = p.cursor_sub;
        }
        look
    }

    fn pane_look(&self, it: &PaneItem) -> Look {
        let p = self.pal;
        let key = NodeKey::Pane(it.pane.id.clone());
        let plain = !self.label_styles || it.pane.custom_name.is_some();
        let mut look = Look { base: self.plain().fg(p.text), sub: p.muted, plain };
        if it.is_focused() {
            look.base = Style::default().bg(p.cur_win_bg).fg(p.cur_win_fg).with(BOLD);
            look.sub = p.cur_win_sub;
        } else if it.is_win_sibling() {
            look.base = Style::default().bg(p.cur_pane_bg).fg(p.cur_pane_fg);
            look.sub = p.cur_pane_num;
        } else if self.is_hover(&key) {
            look.base = look.base.bg(p.hover_bg);
        }
        if self.drop_into_pane(&it.pane.id) {
            look.base = Style::default().bg(p.drop_bg).fg(p.drop_fg).underline(UNDER_DASHED).us(p.drop_line);
            look.sub = p.drop_fg;
            look.plain = true;
        }
        if self.is_cursor(&key) {
            look.base = Style::default().bg(p.cursor_bg).fg(p.cursor_fg);
            if it.is_focused() {
                look.base = look.base.with(BOLD);
            }
            look.sub = p.cursor_sub;
            look.plain = true;
        }
        look
    }

    /// Notes the popup when `key` wants one and either its text overflows or
    /// `always` is true (e.g. a directory row showing its full path).
    fn note_tip(&self, key: &NodeKey, y: u16, text: impl FnOnce() -> String, total: usize, avail: usize, always: bool) {
        if self.tip_for == Some(key) && (always || total > avail) {
            *self.tip.borrow_mut() = Some(Tip { y, text: text() });
        }
    }

    /// Columns to scroll `key`'s text by, when it is the marquee and its
    /// `total` columns do not fit in `avail`.
    fn scroll(&self, key: &NodeKey, total: usize, avail: usize) -> Option<usize> {
        let (k, elapsed) = self.marquee?;
        if k != key || total <= avail {
            return None;
        }
        let dist = total - avail;
        self.overflow.set(Some(dist as u16));
        Some(marquee_offset(dist, elapsed))
    }

    fn dir_chevron(&self, g: &DirGroup) -> &'static str {
        if g.collapsed { "▸" } else { "▾" }
    }
}

/// Marquee timing: a short pause at each end, then about 14 columns a
/// second at an even pace, so a cut-off name starts moving almost at once.
const MARQUEE_HOLD: f64 = 350.0;
const MARQUEE_COL: f64 = 70.0;

fn marquee_travel(dist: usize) -> f64 {
    (dist as f64 * MARQUEE_COL).max(1.0)
}

/// One full cycle: out, pause, back, pause.
pub fn marquee_cycle(dist: u16) -> Duration {
    Duration::from_millis((2.0 * (marquee_travel(dist as usize) + 2.0 * MARQUEE_HOLD)) as u64)
}

/// Columns scrolled `elapsed` into a back-and-forth marquee over `dist`.
pub fn marquee_offset(dist: usize, elapsed: Duration) -> usize {
    let travel = marquee_travel(dist);
    let pass = travel + 2.0 * MARQUEE_HOLD;
    let t = elapsed.as_secs_f64() * 1000.0 % (2.0 * pass);
    let t = if t > pass { 2.0 * pass - t } else { t };
    (((t - MARQUEE_HOLD) / travel).clamp(0.0, 1.0) * dist as f64).round() as usize
}

/// Writes `spans` from `x` with their first `skip` columns scrolled away,
/// clipped at `limit`.
fn put_scrolled(c: &mut Canvas<Hit>, x: u16, y: u16, spans: &[(String, Style)], skip: usize, limit: u16) -> u16 {
    let mut x = x;
    let mut skip = skip;
    for (s, st) in spans {
        let w = width(s);
        if skip >= w {
            skip -= w;
            continue;
        }
        let (pad, rest) = skip_width(s, skip);
        skip = 0;
        x = c.put(x, y, &" ".repeat(pad), *st, limit);
        x = c.put(x, y, rest, *st, limit);
    }
    x
}

fn put_spans_truncated(
    c: &mut Canvas<Hit>,
    mut x: u16,
    y: u16,
    spans: &[(String, Style)],
    limit: u16,
    fallback: Style,
) -> u16 {
    let avail = limit.saturating_sub(x) as usize;
    let total: usize = spans.iter().map(|(s, _)| width(s)).sum();
    if total <= avail {
        for (s, st) in spans {
            x = c.put(x, y, s, *st, limit);
        }
        return x;
    }
    if avail == 0 {
        return x;
    }
    let mut room = avail - 1;
    let mut last = fallback;
    for (s, st) in spans {
        let part = take_width(s, room);
        x = c.put(x, y, part, *st, limit);
        room -= width(part);
        last = *st;
        if part.len() < s.len() || room == 0 {
            break;
        }
    }
    c.put(x, y, "…", last, limit)
}

fn put_text(c: &mut Canvas<Hit>, x: u16, y: u16, s: &str, style: Style, limit: u16) -> u16 {
    c.put(x, y, &truncate(s, limit.saturating_sub(x) as usize), style, limit)
}

fn put_dir_label(c: &mut Canvas<Hit>, ctx: &Ctx, g: &DirGroup, x: u16, y: u16, limit: u16, style: Style) -> u16 {
    let key = NodeKey::Dir(g.raw_path.clone());
    let total = width(&g.label);
    let avail = limit.saturating_sub(x) as usize;
    if ctx.hover_add == Some(g.raw_path.as_str()) {
        ctx.note_tip(&key, y, || ctx.text.new_window_in(&g.label), total, avail, true);
    } else {
        let tip_text = format!("{} · {}", g.display_path, ctx.text.panes(g.items.len()));
        ctx.note_tip(&key, y, || tip_text, total, avail, true);
    }
    match ctx.scroll(&key, total, avail) {
        Some(skip) => put_scrolled(c, x, y, &[(g.label.clone(), style)], skip, limit),
        None => put_text(c, x, y, &g.label, style, limit),
    }
}

fn put_pane_label(
    c: &mut Canvas<Hit>,
    ctx: &Ctx,
    it: &PaneItem,
    (x, y, limit): (u16, u16, u16),
    look: &Look,
    has_sub: bool,
) -> u16 {
    let spans = if look.plain { vec![(it.pane.cmd.clone(), look.base)] } else { it.pane.label.spans(look.base) };
    let avail = limit.saturating_sub(x) as usize;
    let total: usize = spans.iter().map(|(s, _)| width(s)).sum();
    let key = NodeKey::Pane(it.pane.id.clone());
    if ctx.hover_rename == Some(it.pane.id.as_str()) {
        ctx.note_tip(&key, y, || ctx.text.rename_tip(), total, avail, true);
    } else {
        let always_tip = has_sub || it.win_pane_count > 1 || it.pane.custom_name.is_some();
        ctx.note_tip(&key, y, || it.tooltip(ctx.home), total, avail, always_tip);
    }
    if let Some(skip) = ctx.scroll(&key, total, avail) {
        return put_scrolled(c, x, y, &spans, skip, limit);
    }
    put_spans_truncated(c, x, y, &spans, limit, look.base)
}

fn draw_rename_field(c: &mut Canvas<Hit>, ctx: &Ctx, r: &Rename, x_start: u16, y: u16, x_end: u16) {
    let avail = x_end.saturating_sub(x_start) as usize;
    let st = if r.select_all {
        Style::default().bg(ctx.pal.cursor_bg).fg(ctx.pal.cursor_fg)
    } else {
        Style::default().bg(ctx.pal.cur_pane_bg).fg(ctx.pal.cur_pane_fg).underline(UNDERLINE).us(ctx.pal.cursor_bg)
    };
    c.fill(x_start, x_end, y, st);
    let (pad, slice, caret_col) = r.visible_window(avail);
    let mut x = x_start;
    if pad > 0 {
        x = c.put(x, y, &" ".repeat(pad), st, x_end);
    }
    c.put(x, y, slice, st, x_end);
    let caret_x = (x_start + caret_col as u16).min(x_end.saturating_sub(1));
    ctx.edit_cursor.set(Some((caret_x, y)));
}

fn dir_badges(g: &DirGroup) -> String {
    if !g.collapsed {
        return String::new();
    }
    let mut b = String::new();
    if g.has_focused {
        b.push('*');
    }
    if g.has_activity {
        b.push('#');
    }
    if g.has_bell {
        b.push('!');
    }
    b
}

fn group_height(ctx: &Ctx, g: &DirGroup) -> u16 {
    if ctx.rail {
        return g.items.len() as u16;
    }
    let panes = if g.collapsed { 0 } else { g.items.len() as u16 };
    match ctx.variant {
        Variant::Indent | Variant::Tree => 1 + panes,
        Variant::Blocks => 2 + panes,
    }
}

pub fn list(ctx: &Ctx) -> List {
    let mut total: u16 = ctx.groups.iter().map(|g| group_height(ctx, g)).sum();
    if ctx.rail && ctx.groups.len() > 1 {
        total += (ctx.groups.len() - 1) as u16;
    }
    total = total.max(1);
    let mut c = Canvas::new(ctx.width, total, ctx.plain());
    let mut groups = Vec::new();
    let mut nodes = Vec::new();
    let mut y = 0;

    for (gi, g) in ctx.groups.iter().enumerate() {
        let gh = group_height(ctx, g);
        if ctx.rail {
            if gi > 0 {
                let st = ctx.plain().fg(if g.has_focused { ctx.pal.accent } else { ctx.pal.divider });
                let sx = ctx.width.saturating_sub(2) / 2;
                c.put(sx, y, "──", st, ctx.width);
                c.set_hit(0, ctx.width, y, &Hit::Dir(g.raw_path.clone()));
                y += 1;
            }
            let y0 = y;
            draw_rail_group(&mut c, y, ctx, g, &mut nodes);
            groups.push(Group { dir: g.raw_path.clone(), y0, y1: y + gh.saturating_sub(1) });
            y += gh;
        } else {
            match ctx.variant {
                Variant::Indent | Variant::Tree => draw_line_group(&mut c, y, ctx, g, &mut nodes),
                Variant::Blocks => draw_dir_block(&mut c, y, ctx, g, &mut nodes),
            }
            groups.push(Group { dir: g.raw_path.clone(), y0: y, y1: y + gh.saturating_sub(1) });
            y += gh;
        }
    }

    decorate_drag(&mut c, ctx, &groups, &nodes, 1);
    List { canvas: c, groups, nodes }
}

/// Dims what is being dragged and draws the insertion line for a reorder.
fn decorate_drag(c: &mut Canvas<Hit>, ctx: &Ctx, groups: &[Group], nodes: &[(NodeKey, u16, u16)], indent: u16) {
    let Some(drag) = ctx.drag else { return };
    let rows = match &drag.src {
        NodeKey::Dir(dir) => groups.iter().find(|g| &g.dir == dir).map(|g| (g.y0, g.y1)),
        NodeKey::Pane(_) => nodes.iter().find(|(k, ..)| *k == drag.src).map(|(_, a, b)| (*a, *b)),
    };
    if let Some((a, b)) = rows {
        for y in a..=b {
            c.restyle(0, c.w, y, |s| s.attrs |= DIM);
        }
    }
    let line = ctx.pal.drop_line;
    let w = c.w.saturating_sub(1);
    let mark = |c: &mut Canvas<Hit>, y: u16, attr: u16| {
        c.restyle(indent, w, y, |s| {
            s.attrs |= attr;
            s.us = line;
        })
    };
    match ctx.target() {
        Some(Target::DirBefore(dir)) => {
            if let Some(i) = groups.iter().position(|g| &g.dir == dir) {
                if i > 0 { mark(c, groups[i - 1].y1, UNDERLINE) } else { mark(c, groups[i].y0, OVERLINE) }
            }
        }
        Some(Target::DirAfter(dir)) => {
            if let Some(g) = groups.iter().find(|g| &g.dir == dir) {
                mark(c, g.y1, UNDERLINE);
            }
        }
        _ => {}
    }
}

/// Neutral-width indicator for inactive panes in `Variant::Indent`.
const LEAF: &str = "∙";

fn draw_dir_row(c: &mut Canvas<Hit>, y: u16, ctx: &Ctx, g: &DirGroup, nodes: &mut Vec<(NodeKey, u16, u16)>) {
    let full = ctx.width;
    let end = full.saturating_sub(1);
    let key = NodeKey::Dir(g.raw_path.clone());
    let look = ctx.dir_look(g);
    c.fill(1, end, y, look.base);
    c.put(1, y, ctx.dir_chevron(g), look.base.fg(look.sub), end);

    let hovered = ctx.is_hover(&key);
    let right_limit = if hovered && end >= 8 {
        let add_x = end - 3;
        let add_st = if ctx.hover_add == Some(g.raw_path.as_str()) {
            Style::default().bg(ctx.pal.cursor_bg).fg(ctx.pal.cursor_fg).with(BOLD)
        } else {
            Style::default().bg(ctx.pal.cur_win_bg).fg(ctx.pal.cur_win_fg).with(BOLD)
        };
        c.put(add_x, y, " + ", add_st, end);
        add_x
    } else {
        let badges = dir_badges(g);
        let cnt = g.items.len().to_string();
        let right_text = if badges.is_empty() { cnt } else { format!("{badges} {cnt}") };
        let rw = width(&right_text) as u16;
        if end > 5 + rw {
            let rx = end - rw - 1;
            c.put(rx, y, &right_text, look.base.fg(look.sub), end);
            rx
        } else {
            end
        }
    };

    put_dir_label(c, ctx, g, 3, y, right_limit.saturating_sub(1), look.base);
    c.set_hit(0, full, y, &Hit::Dir(g.raw_path.clone()));
    if hovered && end >= 8 {
        c.set_hit(end - 3, full, y, &Hit::DirAdd(g.raw_path.clone()));
    }
    nodes.push((key, y, y));
}

fn draw_pane_row(
    c: &mut Canvas<Hit>,
    y: u16,
    ctx: &Ctx,
    g: &DirGroup,
    it: &PaneItem,
    is_last: bool,
    nodes: &mut Vec<(NodeKey, u16, u16)>,
) {
    let full = ctx.width;
    let end = full.saturating_sub(1);
    let guide_st = ctx.plain().fg(ctx.pal.guide);
    let look = ctx.pane_look(it);

    match ctx.variant {
        Variant::Tree => {
            c.put(0, y, if is_last { " └─ " } else { " ├─ " }, guide_st, full);
            c.fill(4, end, y, look.base);
        }
        _ => {
            c.fill(2, end, y, look.base);
            let ind = if it.is_focused() {
                "▪"
            } else if it.is_win_sibling() {
                "▫"
            } else {
                LEAF
            };
            let ind_color = if ctx.is_cursor(&NodeKey::Pane(it.pane.id.clone())) {
                ctx.pal.cursor_sub
            } else if it.is_focused() {
                ctx.pal.cur_win_sub
            } else if it.is_win_sibling() {
                ctx.pal.dot_active
            } else {
                ctx.pal.dot
            };
            c.put(2, y, ind, look.base.fg(ind_color), end);
        }
    }

    if let Some(r) = ctx.rename.filter(|r| r.pid == it.pane.id) {
        draw_rename_field(c, ctx, r, 4, y, end);
        c.set_hit(0, full, y, &Hit::Pane { wid: it.wid.clone(), pid: it.pane.id.clone(), dir: g.raw_path.clone() });
        nodes.push((NodeKey::Pane(it.pane.id.clone()), y, y));
        return;
    }

    let pkey = NodeKey::Pane(it.pane.id.clone());
    let hovered = ctx.rename.is_none() && ctx.is_hover(&pkey) && end > 8;
    let mut text_limit = if hovered {
        let rx = end - 3;
        let rst = if ctx.hover_rename == Some(it.pane.id.as_str()) {
            Style::default().bg(ctx.pal.cursor_bg).fg(ctx.pal.cursor_fg).with(BOLD)
        } else {
            Style::default().bg(ctx.pal.cur_win_bg).fg(ctx.pal.cur_win_fg).with(BOLD)
        };
        c.put(rx, y, " ✎ ", rst, end);
        rx.saturating_sub(1)
    } else {
        let badge = it.badge();
        let bw = width(&badge) as u16;
        if end > 6 + bw {
            let bx = end - bw - 1;
            c.put(bx, y, &badge, look.base.fg(look.sub), end);
            bx.saturating_sub(1)
        } else {
            end
        }
    };

    let sub = (ctx.snap.cfg.group_by == GroupBy::Git).then(|| it.rel_subdir(&g.raw_path, ctx.home)).flatten();
    let mut has_sub = false;
    if let Some(sub) = &sub {
        let tag = format!("({sub})");
        let tw = width(&tag) as u16;
        if text_limit > 4 + tw + 4 {
            let tx = text_limit - tw;
            c.put(tx, y, &tag, look.base.fg(look.sub), text_limit);
            text_limit = tx.saturating_sub(1);
            has_sub = true;
        }
    }

    put_pane_label(c, ctx, it, (4, y, text_limit), &look, has_sub);
    c.set_hit(0, full, y, &Hit::Pane { wid: it.wid.clone(), pid: it.pane.id.clone(), dir: g.raw_path.clone() });
    if hovered {
        c.set_hit(end - 3, full, y, &Hit::PaneRename { wid: it.wid.clone(), pid: it.pane.id.clone() });
    }
    nodes.push((pkey, y, y));
}

fn draw_line_group(c: &mut Canvas<Hit>, y0: u16, ctx: &Ctx, g: &DirGroup, nodes: &mut Vec<(NodeKey, u16, u16)>) {
    draw_dir_row(c, y0, ctx, g, nodes);
    if g.collapsed {
        return;
    }
    let n = g.items.len();
    for (i, it) in g.items.iter().enumerate() {
        draw_pane_row(c, y0 + 1 + i as u16, ctx, g, it, i + 1 == n, nodes);
    }
}

const THIN: [&str; 6] = ["┌", "┐", "└", "┘", "─", "│"];
const HEAVY: [&str; 6] = ["┏", "┓", "┗", "┛", "━", "┃"];

fn draw_dir_block(c: &mut Canvas<Hit>, y0: u16, ctx: &Ctx, g: &DirGroup, nodes: &mut Vec<(NodeKey, u16, u16)>) {
    let p = ctx.pal;
    let full = ctx.width;
    let (x0, x1) = (1, full.saturating_sub(2));
    let key = NodeKey::Dir(g.raw_path.clone());
    let cursor = ctx.is_cursor(&key);
    let (set, color) = if cursor {
        (HEAVY, p.cursor_bg)
    } else if g.has_focused {
        (THIN, p.border_cur)
    } else {
        (THIN, p.border)
    };
    let frame = ctx.plain().fg(color);
    let open = !g.collapsed;
    let last = y0 + 1 + if open { g.items.len() as u16 } else { 0 };
    for y in y0..=last {
        if y == y0 || y == last {
            c.put(x0, y, if y == y0 { set[0] } else { set[2] }, frame, full);
            for x in x0 + 1..x1 {
                c.put(x, y, set[4], frame, full);
            }
            c.put(x1, y, if y == y0 { set[1] } else { set[3] }, frame, full);
        } else {
            c.put(x0, y, set[5], frame, full);
            c.put(x1, y, set[5], frame, full);
        }
        c.set_hit(0, full, y, &Hit::Dir(g.raw_path.clone()));
    }

    let mut look = ctx.dir_look(g);
    if !cursor && !(g.collapsed && g.has_focused) {
        let bg = if ctx.is_hover(&key) { p.hover_bg } else { p.bg };
        look.base = look.base.bg(bg);
    }
    let chev = ctx.dir_chevron(g);
    let hovered = ctx.is_hover(&key);
    let title_end = if hovered && x1 >= x0 + 10 {
        let add_x = x1 - 5;
        let add_st = if ctx.hover_add == Some(g.raw_path.as_str()) {
            Style::default().bg(p.cursor_bg).fg(p.cursor_fg).with(BOLD)
        } else {
            Style::default().bg(p.cur_win_bg).fg(p.cur_win_fg).with(BOLD)
        };
        c.put(add_x, y0, " + ", add_st, x1);
        c.put(x1 - 2, y0, &format!("{chev} "), frame.fg(p.muted), x1);
        c.set_hit(add_x, x1 - 2, y0, &Hit::DirAdd(g.raw_path.clone()));
        add_x
    } else {
        let badges = dir_badges(g);
        let cnt = g.items.len();
        let right_str = if badges.is_empty() { format!(" {cnt} {chev} ") } else { format!(" {badges} {cnt} {chev} ") };
        let rw = width(&right_str) as u16;
        if x1 > x0 + rw + 4 {
            let rx = x1 - rw;
            c.put(rx, y0, &right_str, frame.fg(if g.collapsed && g.has_focused { p.cur_win_sub } else { p.muted }), x1);
            rx
        } else {
            x1
        }
    };

    let x = c.put(x0 + 1, y0, " ", look.base, title_end);
    let x = put_dir_label(c, ctx, g, x, y0, title_end.saturating_sub(1), look.base);
    c.put(x, y0, " ", look.base, title_end);
    nodes.push((key, y0, y0));

    if !open {
        return;
    }
    for (i, it) in g.items.iter().enumerate() {
        let y = y0 + 1 + i as u16;
        let look = ctx.pane_look(it);
        c.fill(x0 + 1, x1, y, look.base);
        let pkey = NodeKey::Pane(it.pane.id.clone());
        let dot_color = if ctx.is_cursor(&pkey) {
            p.cursor_fg
        } else if it.is_focused() {
            p.cur_win_sub
        } else if it.is_win_sibling() {
            p.dot_active
        } else {
            p.dot
        };
        let ind = if it.is_focused() {
            "■"
        } else if it.is_win_sibling() {
            "▫"
        } else {
            "□"
        };
        c.put(x0 + 2, y, ind, look.base.fg(dot_color), x1);

        if let Some(r) = ctx.rename.filter(|r| r.pid == it.pane.id) {
            draw_rename_field(c, ctx, r, x0 + 4, y, x1);
            c.set_hit(0, full, y, &Hit::Pane { wid: it.wid.clone(), pid: it.pane.id.clone(), dir: g.raw_path.clone() });
            nodes.push((pkey, y, y));
            continue;
        }

        let hovered = ctx.rename.is_none() && ctx.is_hover(&pkey) && x1 > x0 + 8;
        let text_limit = if hovered {
            let rx = x1 - 3;
            let rst = if ctx.hover_rename == Some(it.pane.id.as_str()) {
                Style::default().bg(p.cursor_bg).fg(p.cursor_fg).with(BOLD)
            } else {
                Style::default().bg(p.cur_win_bg).fg(p.cur_win_fg).with(BOLD)
            };
            c.put(rx, y, " ✎ ", rst, x1);
            rx.saturating_sub(1)
        } else {
            let badge = it.badge();
            let bw = width(&badge) as u16;
            if x1 > x0 + 6 + bw {
                let bx = x1 - bw - 1;
                c.put(bx, y, &badge, look.base.fg(look.sub), x1);
                bx.saturating_sub(1)
            } else {
                x1.saturating_sub(1)
            }
        };
        put_pane_label(c, ctx, it, (x0 + 4, y, text_limit), &look, false);
        c.set_hit(0, full, y, &Hit::Pane { wid: it.wid.clone(), pid: it.pane.id.clone(), dir: g.raw_path.clone() });
        if hovered {
            c.set_hit(x1 - 3, x1, y, &Hit::PaneRename { wid: it.wid.clone(), pid: it.pane.id.clone() });
        }
        nodes.push((pkey, y, y));
    }
}

fn draw_rail_group(c: &mut Canvas<Hit>, y0: u16, ctx: &Ctx, g: &DirGroup, nodes: &mut Vec<(NodeKey, u16, u16)>) {
    for (i, it) in g.items.iter().enumerate() {
        let y = y0 + i as u16;
        let look = ctx.pane_look(it);
        let text = it.rail_label();
        let tw = width(&text) as u16;
        let mut bw = (tw + 2).max(4).min(ctx.width);
        if (ctx.width - bw) % 2 == 1 && bw < ctx.width {
            bw += 1;
        }
        let x = (ctx.width - bw) / 2;
        c.fill(x, x + bw, y, look.base);
        c.put(x + (bw - tw) / 2, y, &text, look.base, x + bw);
        c.set_hit(
            0,
            ctx.width,
            y,
            &Hit::Pane { wid: it.wid.clone(), pid: it.pane.id.clone(), dir: g.raw_path.clone() },
        );
        let key = NodeKey::Pane(it.pane.id.clone());
        let tip_text = format!("{} · {} (w{})", g.label, it.pane.cmd, it.win_index);
        ctx.note_tip(&key, y, || tip_text, 100, 0, true);
        nodes.push((key, y, y));
    }
}

/// Session name and pane/dir count; on the right, with `buttons`, the two
/// split buttons; then a rule. Two rows.
pub fn header(c: &mut Canvas<Hit>, ctx: &Ctx, focused: bool, buttons: bool, text: &Strings) -> u16 {
    let p = ctx.pal;
    let full = c.w;
    let name = &ctx.snap.session_name;
    let mut tip = None;
    if ctx.rail {
        let initial: String = name.chars().next().map(|ch| ch.to_uppercase().collect()).unwrap_or_default();
        let st = if focused {
            Style::default().bg(p.cursor_bg).fg(p.cursor_fg).with(BOLD)
        } else {
            Style::default().bg(p.cur_win_bg).fg(p.cur_win_fg).with(BOLD)
        };
        let bw = (width(&initial) as u16 + 2).min(full);
        let x = full.saturating_sub(bw) / 2;
        c.fill(x, x + bw, 0, st);
        c.put(x + 1, 0, &initial, st, x + bw);
    } else {
        let mut right = full.saturating_sub(1);
        if buttons {
            let cfg = &ctx.snap.cfg;
            let items = [('h', "◫", text.split_h(), &cfg.split_h_key), ('v', "⊟", text.split_v(), &cfg.split_v_key)];
            let start = full.saturating_sub(7);
            for (i, (dir, icon, name, key)) in items.iter().enumerate() {
                let x0 = start + 3 * i as u16;
                let hot = ctx.hover_split == Some(*dir);
                let st = if hot { Style::default().bg(p.cur_win_bg).fg(p.cur_win_fg) } else { ctx.plain().fg(p.text) };
                c.put(x0, 0, &format!(" {icon} "), st, full);
                c.set_hit(x0, x0 + 3, 0, &Hit::Split(*dir));
                if hot {
                    let label = if key.is_empty() { name.clone() } else { format!("{name} · {} {key}", cfg.prefix) };
                    tip = Some((x0 + 3, label));
                }
            }
            right = start.saturating_sub(1);
        }
        let x = put_text(c, 1, 0, name, ctx.plain().fg(p.accent).with(BOLD), right);
        let panes = ctx.snap.pane_count();
        let dirs = ctx.groups.len();
        let primary = if full >= 30 { text.panes_dirs(panes, dirs) } else { text.panes(panes) };
        let fallback = text.panes(panes);
        let count = if x + 2 + width(&primary) as u16 <= right {
            Some(primary)
        } else if x + 2 + width(&fallback) as u16 <= right {
            Some(fallback)
        } else {
            None
        };
        if let Some(count) = count {
            c.put(x + 2, 0, &count, ctx.plain().fg(p.muted), right);
        }
    }
    let rule = ctx.plain().fg(p.divider);
    for x in 0..full {
        c.put(x, 1, "─", rule, full);
    }
    if let Some((end, label)) = tip {
        tooltip(c, ctx, end.saturating_sub(width(&label) as u16 + 2), 1, &label, c.h);
    }
    2
}

/// The design's floating label: the whole of something cut off, drawn
/// over whatever is there from column `x`. A sidebar is too narrow to let
/// it spill out, so it shifts left to fit, and wraps onto the rows below
/// (before `bottom`) when even that is not enough.
pub fn tooltip(c: &mut Canvas<Hit>, ctx: &Ctx, x: u16, y: u16, text: &str, bottom: u16) {
    let st = Style::default().bg(ctx.pal.chip_bg).fg(ctx.pal.chip_fg);
    let full = c.w as usize;
    let tw = width(text);
    if tw + 2 <= full {
        let x = (x as usize).min(full - tw - 2) as u16;
        c.put(x, y, &format!(" {text} "), st, c.w);
        return;
    }
    let mut rest = text;
    let mut y = y;
    while !rest.is_empty() && y < bottom {
        let part = take_width(rest, full.saturating_sub(2).max(2));
        if part.is_empty() {
            break;
        }
        c.fill(0, c.w, y, st);
        c.put(1, y, part, st, c.w);
        rest = &rest[part.len()..];
        y += 1;
    }
}

/// What the footer shows below the list.
#[derive(Default)]
pub struct Footer {
    pub lines: Vec<String>,
    pub accent: bool,
    /// A drag in progress: its hint, and whether it names a valid drop.
    pub drag: Option<(String, bool)>,
    /// The "+ new window" drop zone, and whether it is the current target.
    pub zone: Option<(String, bool)>,
}

impl Footer {
    pub fn height(&self) -> u16 {
        let body = self.lines.len() as u16 + self.drag.is_some() as u16 + if self.zone.is_some() { 3 } else { 0 };
        if body == 0 { 0 } else { 1 + body }
    }
}

pub fn footer(c: &mut Canvas<Hit>, y0: u16, ctx: &Ctx, f: &Footer) {
    let p = ctx.pal;
    let full = c.w;
    for x in 0..full {
        c.put(x, y0, "─", ctx.plain().fg(p.divider), full);
    }
    let mut y = y0 + 1;
    if let Some((label, hot)) = &f.zone {
        let (x0, x1) = (1, full.saturating_sub(2));
        let edge = ctx.plain().fg(if *hot { p.drop_line } else { p.guide });
        let inner = if *hot { Style::default().bg(p.drop_bg).fg(p.drop_fg) } else { ctx.plain().fg(p.muted) };
        for (dy, (l, m, r)) in [("┌", "╌", "┐"), ("╎", " ", "╎"), ("└", "╌", "┘")].into_iter().enumerate()
        {
            let yy = y + dy as u16;
            c.put(x0, yy, l, edge, full);
            for x in x0 + 1..x1 {
                c.put(x, yy, m, if dy == 1 { inner } else { edge }, full);
            }
            c.put(x1, yy, r, edge, full);
            c.set_hit(0, full, yy, &Hit::NewWindow);
        }
        let label = truncate(label, x1.saturating_sub(x0 + 2) as usize);
        let lx = x0 + 1 + (x1.saturating_sub(x0 + 1)).saturating_sub(width(&label) as u16) / 2;
        c.put(lx, y + 1, &label, inner, x1);
        y += 3;
    }
    if let Some((hint, ok)) = &f.drag {
        let st = if *ok { Style::default().bg(p.chip_bg).fg(p.chip_fg).with(BOLD) } else { ctx.plain().fg(p.muted) };
        let chip = format!(" {} ", truncate(hint, full.saturating_sub(4) as usize));
        c.put(1, y, &chip, st, full.saturating_sub(1));
        y += 1;
    }
    let st = ctx.plain().fg(if f.accent { p.accent } else { p.muted });
    for line in &f.lines {
        put_text(c, 1, y, line, st, full.saturating_sub(1));
        y += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::label::Label;
    use crate::model::tests::sample;
    use crate::theme;

    fn ctx<'a>(
        snap: &'a Snapshot,
        groups: &'a [DirGroup],
        pal: &'a Palette,
        text: &'a Strings,
        variant: Variant,
        width: u16,
    ) -> Ctx<'a> {
        Ctx {
            snap,
            groups,
            pal,
            text,
            home: Some("/home/u"),
            width,
            rail: false,
            variant,
            label_styles: true,
            cursor: None,
            hover: None,
            hover_add: None,
            hover_rename: None,
            hover_split: None,
            drag: None,
            rename: None,
            edit_cursor: Cell::new(None),
            marquee: None,
            overflow: Cell::new(None),
            tip_for: None,
            tip: RefCell::new(None),
        }
    }

    #[test]
    fn indent_layout() {
        let snap = sample();
        let groups = snap.groups_with_home(Some("/home/u"));
        let pal = theme::light();
        let text = Strings::new(crate::config::Lang::Zh);
        let mut cx = ctx(&snap, &groups, &pal, &text, Variant::Indent, 28);
        let l = list(&cx);
        let rows: Vec<String> = (0..l.canvas.h).map(|y| l.canvas.row_text(y).trim_end().to_string()).collect();
        assert_eq!(
            rows,
            vec![
                " ▾ tmux-arbor/           3",
                "  ▪ nvim                1*",
                "  ▫ cargo                1",
                "  ∙ lazygit              3",
                " ▾ src/                  1",
                "  ▫ zsh                  1",
                " ▾ api-server/           2",
                "  ∙ npm                 2#",
                "  ∙ tail                2#",
                " ▾ ~/                    1",
                "  ∙ psql                4!",
            ]
        );
        assert_eq!(l.canvas.hit(4, 0), Some(&Hit::Dir("/home/u/ws/tmux-arbor".into())));
        assert_eq!(
            l.canvas.hit(5, 1),
            Some(&Hit::Pane { wid: "@1".into(), pid: "%1".into(), dir: "/home/u/ws/tmux-arbor".into() })
        );
        assert_eq!(l.groups[0].y0, 0);
        assert_eq!(l.groups[0].y1, 3);

        // Hovering a directory reveals the '+' button hit target on its right.
        let hover = NodeKey::Dir("/home/u/ws/tmux-arbor".into());
        cx.hover = Some(&hover);
        let lh = list(&cx);
        assert_eq!(lh.canvas.row_text(0).trim_end(), " ▾ tmux-arbor/           +");
        assert_eq!(lh.canvas.hit(25, 0), Some(&Hit::DirAdd("/home/u/ws/tmux-arbor".into())));
    }

    #[test]
    fn tree_layout() {
        let snap = sample();
        let groups = snap.groups_with_home(Some("/home/u"));
        let pal = theme::light();
        let text = Strings::new(crate::config::Lang::Zh);
        let l = list(&ctx(&snap, &groups, &pal, &text, Variant::Tree, 28));
        let rows: Vec<String> = (0..l.canvas.h).map(|y| l.canvas.row_text(y).trim_end().to_string()).collect();
        assert_eq!(rows[0], " ▾ tmux-arbor/           3");
        assert_eq!(rows[1], " ├─ nvim                1*");
        assert_eq!(rows[2], " ├─ cargo                1");
        assert_eq!(rows[3], " └─ lazygit              3");
        assert_eq!(rows[5], " └─ zsh                  1");
    }

    #[test]
    fn blocks_layout() {
        let mut snap = sample();
        snap.collapsed_dirs.insert("/home/u/ws/api-server".into());
        let groups = snap.groups_with_home(Some("/home/u"));
        let pal = theme::light();
        let text = Strings::new(crate::config::Lang::Zh);
        let l = list(&ctx(&snap, &groups, &pal, &text, Variant::Blocks, 26));
        assert_eq!(l.canvas.row_text(0), " ┌ tmux-arbor/ ──── 3 ▾ ┐ ");
        assert_eq!(l.canvas.row_text(1), " │ ■ nvim            1* │ ");
        assert_eq!(l.canvas.row_text(4), " └──────────────────────┘ ");
        assert_eq!(l.canvas.row_text(8), " ┌ api-server/ ── # 2 ▸ ┐ ");
        assert_eq!(l.canvas.row_text(9), " └──────────────────────┘ ");
    }

    #[test]
    fn rail_layout() {
        let snap = sample();
        let groups = snap.groups_with_home(Some("/home/u"));
        let pal = theme::light();
        let text = Strings::new(crate::config::Lang::Zh);
        let mut c = ctx(&snap, &groups, &pal, &text, Variant::Indent, 7);
        c.rail = true;
        let l = list(&c);
        // 7 panes + 3 separators = 10 rows
        assert_eq!(l.canvas.h, 10);
        assert_eq!(l.canvas.row_text(0), "  1.1  ");
        assert_eq!(l.canvas.row_text(1), "  1.3  ");
        assert_eq!(l.canvas.row_text(2), "   3   ");
        assert_eq!(l.canvas.row_text(3), "  ──   ");
        assert_eq!(l.canvas.row_text(4), "  1.2  ");
    }

    #[test]
    fn long_pane_labels_are_cut() {
        let mut snap = sample();
        snap.windows[3].panes[0].cmd = "very-long-command-name-indeed".into();
        snap.windows[3].panes[0].label = Label::parse("very-long-command-name-indeed");
        let groups = snap.groups_with_home(Some("/home/u"));
        let pal = theme::light();
        let text = Strings::new(crate::config::Lang::Zh);
        let l = list(&ctx(&snap, &groups, &pal, &text, Variant::Indent, 20));
        assert_eq!(l.canvas.row_text(10).trim_end(), "  ∙ very-long-… 4!");
    }

    #[test]
    fn split_buttons_sit_right_of_the_title() {
        let mut snap = sample();
        snap.cfg.prefix = "C-x".into();
        snap.cfg.split_h_key = "\\".into();
        let groups = snap.groups_with_home(Some("/home/u"));
        let pal = theme::light();
        let text = Strings::new(crate::config::Lang::Zh);
        let mut c = Canvas::new(28, 4, Style::default());
        let mut cx = ctx(&snap, &groups, &pal, &text, Variant::Indent, 28);
        assert_eq!(header(&mut c, &cx, true, true, &text), 2);
        assert_eq!(c.row_text(0).trim_end(), " work  7 panes        ◫  ⊟");
        assert_eq!(c.hit(22, 0), Some(&Hit::Split('h')));
        assert_eq!(c.hit(25, 0), Some(&Hit::Split('v')));
        cx.hover_split = Some('h');
        let mut c = Canvas::new(28, 4, Style::default());
        header(&mut c, &cx, false, true, &text);
        assert_eq!(c.row_text(0).trim_end(), " work  7 panes        ◫  ⊟");
        assert_eq!(c.row_text(1), "────── 水平分割 · C-x \\ ────");
        cx.hover_split = Some('v');
        let mut c = Canvas::new(28, 4, Style::default());
        header(&mut c, &cx, false, true, &text);
        assert_eq!(c.row_text(1), "───────────────── 竖直分割 ─");
    }

    #[test]
    fn marquee_scrolls_a_cut_label() {
        let mut snap = sample();
        snap.windows[3].panes[0].cmd = "staging-db-migrations".into();
        snap.windows[3].panes[0].label = Label::parse("staging-db-migrations");
        let groups = snap.groups_with_home(Some("/home/u"));
        let pal = theme::light();
        let text = Strings::new(crate::config::Lang::Zh);
        let key = NodeKey::Pane("%7".into());
        let mut c = ctx(&snap, &groups, &pal, &text, Variant::Indent, 20);
        c.marquee = Some((&key, Duration::ZERO));
        let row0 = list(&c).canvas.row_text(10).trim_end().to_string();
        assert!(row0.starts_with("  ∙ staging-db"));
        assert!(c.overflow.get().is_some());
        c.marquee = Some((&key, Duration::from_millis(350 + 3 * 70)));
        assert_ne!(list(&c).canvas.row_text(10).trim_end(), row0);
    }

    #[test]
    fn cut_text_is_noted_for_the_popup() {
        let mut snap = sample();
        snap.windows[3].panes[0].cmd = "staging-db-migrations-very-long".into();
        snap.windows[3].panes[0].label = Label::parse("staging-db-migrations-very-long");
        let groups = snap.groups_with_home(Some("/home/u"));
        let pal = theme::light();
        let text = Strings::new(crate::config::Lang::Zh);
        let key = NodeKey::Pane("%7".into());
        let mut c = ctx(&snap, &groups, &pal, &text, Variant::Indent, 20);
        c.tip_for = Some(&key);
        list(&c);
        assert_eq!(
            c.tip.borrow().clone(),
            Some(Tip { y: 10, text: "staging-db-migrations-very-long · w4 · ~".into() })
        );
    }

    #[test]
    fn marquee_goes_out_and_back() {
        assert_eq!(marquee_offset(10, Duration::ZERO), 0);
        assert_eq!(marquee_offset(10, Duration::from_millis(300)), 0);
        assert_eq!(marquee_offset(10, Duration::from_millis(350 + 350)), 5);
        let pass = 350 + 700 + 350;
        assert_eq!(marquee_offset(10, Duration::from_millis(pass - 100)), 10);
        assert_eq!(marquee_offset(10, Duration::from_millis(2 * pass - 100)), 0);
        assert_eq!(marquee_cycle(10), Duration::from_millis(2 * pass));
    }

    #[test]
    fn tooltips_shift_then_wrap() {
        let snap = sample();
        let groups = snap.groups_with_home(Some("/home/u"));
        let pal = theme::light();
        let text = Strings::new(crate::config::Lang::Zh);
        let c = ctx(&snap, &groups, &pal, &text, Variant::Indent, 20);
        let mut canvas = Canvas::new(20, 3, Style::default());
        tooltip(&mut canvas, &c, 5, 0, "staging-db-mig", 3);
        assert_eq!(canvas.row_text(0), "     staging-db-mig ");
        let mut canvas = Canvas::new(20, 3, Style::default());
        tooltip(&mut canvas, &c, 5, 1, "3:staging-db-migrations", 3);
        assert_eq!(canvas.row_text(1), " 3:staging-db-migra ");
        assert_eq!(canvas.row_text(2), " tions              ");
    }

    #[test]
    fn rename_editor_handles_select_all_and_wide_chars() {
        let mut r = Rename::new("%1".into(), "agy".into());
        assert!(r.select_all);
        r.insert('防');
        assert!(!r.select_all);
        assert_eq!(r.buf, "防");
        r.insert('火');
        r.insert('墙');
        assert_eq!(r.buf, "防火墙");
        r.move_left();
        r.insert('!');
        assert_eq!(r.buf, "防火!墙");
        r.backspace();
        assert_eq!(r.buf, "防火墙");
        r.delete();
        assert_eq!(r.buf, "防火");

        let long = Rename::new("%1".into(), "防火墙排查任务".into());
        let (pad, slice, caret) = long.visible_window(6);
        assert!(caret < 6);
        assert!(pad + width(slice) <= 6);

        let mut r2 = Rename::new("%1".into(), "hello world".into());
        r2.move_right();
        r2.delete_word();
        assert_eq!(r2.buf, "hello ");
        r2.clear();
        assert_eq!(r2.buf, "");
    }

    #[test]
    fn pane_hover_shows_rename_button_and_inline_edit_sets_cursor() {
        let snap = sample();
        let groups = snap.groups_with_home(Some("/home/u"));
        let pal = theme::light();
        let text = Strings::new(crate::config::Lang::Zh);
        let mut cx = ctx(&snap, &groups, &pal, &text, Variant::Indent, 28);
        let hover = NodeKey::Pane("%1".into());
        cx.hover = Some(&hover);
        let lh = list(&cx);
        assert_eq!(lh.canvas.row_text(1).trim_end(), "  ▪ nvim                 ✎");
        assert_eq!(lh.canvas.hit(25, 1), Some(&Hit::PaneRename { wid: "@1".into(), pid: "%1".into() }));

        cx.hover = None;
        let ren = Rename::new("%1".into(), "排查".into());
        cx.rename = Some(&ren);
        let le = list(&cx);
        assert_eq!(le.canvas.row_text(1).trim_end(), "  ▪ 排查");
        assert_eq!(cx.edit_cursor.get(), Some((8, 1)));
    }
}
