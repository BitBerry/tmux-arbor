//! Lays a snapshot out into cells, in one of the design's three styles
//! (branches, blocks, map) or as the 7-column rail, plus the header and
//! footer around the list.

use std::cell::RefCell;

use crate::config::{MAP_MIN, Variant};
use crate::model::{NodeKey, PaneInfo, Snapshot, Win, expanded};
use crate::screen::{BOLD, Canvas, Color, DIM, OVERLINE, Style, UNDER_DASHED, UNDERLINE, take_width, truncate, width};
use crate::strings::Strings;
use crate::theme::Palette;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Hit {
    #[default]
    None,
    Window(String),
    Chevron(String),
    Pane {
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
            Hit::Window(w) | Hit::Chevron(w) => Some(NodeKey::Window(w.clone())),
            Hit::Pane { pid, .. } => Some(NodeKey::Pane(pid.clone())),
            _ => None,
        }
    }
}

/// Where a dragged node would land.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Before(String),
    After(String),
    Join { wid: String, pane: Option<String> },
    Swap(String),
    Break,
}

#[derive(Clone, Debug)]
pub struct Drag {
    pub src: NodeKey,
    pub src_wid: String,
    pub target: Option<Target>,
}

pub struct Ctx<'a> {
    pub snap: &'a Snapshot,
    pub pal: &'a Palette,
    pub width: u16,
    pub rail: bool,
    pub variant: Variant,
    pub label_styles: bool,
    /// Only set while the sidebar holds the keyboard.
    pub cursor: Option<&'a NodeKey>,
    pub hover: Option<&'a NodeKey>,
    /// The split button under the pointer.
    pub hover_split: Option<char>,
    pub drag: Option<&'a Drag>,
    /// The hovered node: if its text is cut off, a tooltip shows all of it.
    pub tip_for: Option<&'a NodeKey>,
    /// Set while drawing: where that cut-off text sits, and all of it.
    pub tip: RefCell<Option<Tip>>,
}

/// Cut-off text under the pointer, in list coordinates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tip {
    pub x: u16,
    pub y: u16,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct Group {
    pub wid: String,
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
    num: Color,
    plain: bool,
}

impl Ctx<'_> {
    pub fn plain(&self) -> Style {
        Style::default().bg(self.pal.bg)
    }

    /// Map labels need room; narrower sidebars fall back to branches.
    fn variant(&self) -> Variant {
        if self.variant == Variant::Map && self.width < MAP_MIN { Variant::Branches } else { self.variant }
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

    fn join_into(&self, wid: &str) -> bool {
        matches!(self.target(), Some(Target::Join { wid: w, .. }) if w == wid)
    }

    fn swap_into(&self, pid: &str) -> bool {
        matches!(self.target(), Some(Target::Swap(p)) if p == pid)
    }

    fn win_look(&self, w: &Win) -> Look {
        let p = self.pal;
        let key = NodeKey::Window(w.id.clone());
        let mut look = Look { base: self.plain().fg(p.text), sub: p.muted, num: p.muted, plain: !self.label_styles };
        if w.active {
            look.base = Style::default().bg(p.cur_win_bg).fg(p.cur_win_fg).with(BOLD);
            look.sub = p.cur_win_sub;
        } else if self.is_hover(&key) {
            look.base = look.base.bg(p.hover_bg);
        }
        if self.join_into(&w.id) {
            look.base = Style::default().bg(p.drop_bg).fg(p.drop_fg).underline(UNDER_DASHED).us(p.drop_line);
            look.sub = p.drop_fg;
            look.plain = true;
        }
        if self.is_cursor(&key) {
            look.base = Style::default().bg(p.cursor_bg).fg(p.cursor_fg);
            if w.active {
                look.base = look.base.with(BOLD);
            }
            look.sub = p.cursor_sub;
            look.plain = true;
        }
        look
    }

    fn pane_look(&self, w: &Win, pane: &PaneInfo) -> Look {
        let p = self.pal;
        let key = NodeKey::Pane(pane.id.clone());
        let mut look = Look { base: self.plain().fg(p.text), sub: p.muted, num: p.muted, plain: true };
        if w.active && pane.active {
            look.base = Style::default().bg(p.cur_pane_bg).fg(p.cur_pane_fg);
            look.num = p.cur_pane_num;
        } else if self.is_hover(&key) {
            look.base = look.base.bg(p.hover_bg);
        }
        if self.swap_into(&pane.id) {
            look.base = Style::default().bg(p.drop_bg).fg(p.drop_fg).underline(UNDER_DASHED).us(p.drop_line);
            look.num = p.drop_fg;
        }
        if self.is_cursor(&key) {
            look.base = Style::default().bg(p.cursor_bg).fg(p.cursor_fg);
            look.num = p.cursor_sub;
        }
        look
    }

    /// Records the tooltip when `key` is hovered and its text, drawn at
    /// (x, y), does not fit in `avail` columns.
    fn note_cut(&self, key: &NodeKey, x: u16, y: u16, text: impl FnOnce() -> String, total: usize, avail: usize) {
        if self.tip_for == Some(key) && total > avail {
            *self.tip.borrow_mut() = Some(Tip { x, y, text: text() });
        }
    }

    /// Small triangles: unlike ▶ ▼ (and the box-drawing guides) they are
    /// not East Asian ambiguous width, so they take one column everywhere.
    fn chevron(&self, w: &Win) -> &'static str {
        if self.rail || w.panes.is_empty() {
            " "
        } else if w.collapsed {
            "▸"
        } else {
            "▾"
        }
    }
}

/// Writes a window's label from `x`, cut with "…" so it ends before
/// `limit`.
fn put_label(c: &mut Canvas<Hit>, ctx: &Ctx, w: &Win, x: u16, y: u16, limit: u16, look: &Look) -> u16 {
    let spans = if look.plain { vec![(w.label.plain(), look.base)] } else { w.label.spans(look.base) };
    let avail = limit.saturating_sub(x) as usize;
    let total: usize = spans.iter().map(|(s, _)| width(s)).sum();
    ctx.note_cut(&NodeKey::Window(w.id.clone()), x, y, || w.label.plain(), total, avail);
    let mut x = x;
    if total <= avail {
        for (s, st) in &spans {
            x = c.put(x, y, s, *st, limit);
        }
        return x;
    }
    if avail == 0 {
        return x;
    }
    let mut room = avail - 1;
    let mut last = look.base;
    for (s, st) in &spans {
        let part = crate::screen::take_width(s, room);
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

/// A pane's command, cut with "…".
fn put_cmd(c: &mut Canvas<Hit>, ctx: &Ctx, pane: &PaneInfo, x: u16, y: u16, style: Style, limit: u16) -> u16 {
    let key = NodeKey::Pane(pane.id.clone());
    ctx.note_cut(&key, x, y, || pane.cmd.clone(), width(&pane.cmd), limit.saturating_sub(x) as usize);
    put_text(c, x, y, &pane.cmd, style, limit)
}

fn win_height(ctx: &Ctx, variant: Variant, w: &Win) -> u16 {
    if ctx.rail {
        return 1;
    }
    let panes = if expanded(w, false) { w.panes.len() as u16 } else { 0 };
    match variant {
        Variant::Branches => 1 + panes,
        Variant::Blocks => 2 + panes,
        Variant::Map => {
            let map = if expanded(w, false) { map_geom(w, ctx.width.saturating_sub(3)).map(|g| g.h + 1) } else { None };
            1 + map.unwrap_or(0)
        }
    }
}

pub fn list(ctx: &Ctx) -> List {
    let variant = ctx.variant();
    let heights: Vec<u16> = ctx.snap.windows.iter().map(|w| win_height(ctx, variant, w)).collect();
    let total = heights.iter().sum::<u16>().max(1);
    let mut c = Canvas::new(ctx.width, total, ctx.plain());
    let mut groups = Vec::new();
    let mut nodes = Vec::new();
    let mut y = 0;
    for (i, w) in ctx.snap.windows.iter().enumerate() {
        if ctx.rail {
            draw_rail(&mut c, y, ctx, w, &mut nodes);
        } else {
            match variant {
                Variant::Branches => draw_branches(&mut c, y, ctx, w, &mut nodes),
                Variant::Blocks => draw_blocks(&mut c, y, ctx, w, &mut nodes),
                Variant::Map => draw_map(&mut c, y, ctx, w, &mut nodes),
            }
        }
        groups.push(Group { wid: w.id.clone(), y0: y, y1: y + heights[i] - 1 });
        y += heights[i];
    }
    decorate_drag(&mut c, ctx, &groups, &nodes, 1);
    List { canvas: c, groups, nodes }
}

/// Dims what is being dragged and draws the insertion line for a reorder.
fn decorate_drag(c: &mut Canvas<Hit>, ctx: &Ctx, groups: &[Group], nodes: &[(NodeKey, u16, u16)], indent: u16) {
    let Some(drag) = ctx.drag else { return };
    let rows = match &drag.src {
        NodeKey::Window(wid) => groups.iter().find(|g| &g.wid == wid).map(|g| (g.y0, g.y1)),
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
        Some(Target::Before(wid)) => {
            if let Some(i) = groups.iter().position(|g| &g.wid == wid) {
                if i > 0 { mark(c, groups[i - 1].y1, UNDERLINE) } else { mark(c, groups[i].y0, OVERLINE) }
            }
        }
        Some(Target::After(wid)) => {
            if let Some(g) = groups.iter().find(|g| &g.wid == wid) {
                mark(c, g.y1, UNDERLINE);
            }
        }
        _ => {}
    }
}

/// 1a: one row per node. A window row starts with its fold arrow and its
/// panes hang from below it on ├─ guides. The arrow never touches a guide
/// sideways: terminals draw box lines at the exact middle of the cell,
/// fonts put ▸ a little lower, and side by side the difference shows.
fn draw_branches(c: &mut Canvas<Hit>, y0: u16, ctx: &Ctx, w: &Win, nodes: &mut Vec<(NodeKey, u16, u16)>) {
    let full = ctx.width;
    let end = full.saturating_sub(1);
    let guide = ctx.plain().fg(ctx.pal.guide);
    let look = ctx.win_look(w);
    c.fill(1, end, y0, look.base);
    c.put(1, y0, ctx.chevron(w), look.base.fg(look.sub), end);
    put_label(c, ctx, w, 3, y0, end.saturating_sub(1), &look);
    c.set_hit(0, full, y0, &Hit::Window(w.id.clone()));
    if !w.panes.is_empty() {
        c.set_hit(0, 3, y0, &Hit::Chevron(w.id.clone()));
    }
    nodes.push((NodeKey::Window(w.id.clone()), y0, y0));
    if !expanded(w, ctx.rail) {
        return;
    }
    let n = w.panes.len();
    for (i, pane) in w.panes.iter().enumerate() {
        let y = y0 + 1 + i as u16;
        c.put(0, y, if i + 1 == n { " └─" } else { " ├─" }, guide, full);
        let look = ctx.pane_look(w, pane);
        c.fill(3, end, y, look.base);
        let num = (i + 1).to_string();
        let x = c.put(4, y, &num, look.base.fg(look.num), end);
        put_cmd(c, ctx, pane, x.max(5) + 1, y, look.base, end.saturating_sub(1));
        c.set_hit(0, full, y, &Hit::Pane { wid: w.id.clone(), pid: pane.id.clone() });
        nodes.push((NodeKey::Pane(pane.id.clone()), y, y));
    }
}

const THIN: [&str; 6] = ["┌", "┐", "└", "┘", "─", "│"];
const HEAVY: [&str; 6] = ["┏", "┓", "┗", "┛", "━", "┃"];
const DASHED: [&str; 6] = ["┌", "┐", "└", "┘", "╌", "╎"];

/// 1b: each window in a thin frame with its label in the top edge.
fn draw_blocks(c: &mut Canvas<Hit>, y0: u16, ctx: &Ctx, w: &Win, nodes: &mut Vec<(NodeKey, u16, u16)>) {
    let p = ctx.pal;
    let full = ctx.width;
    let (x0, x1) = (1, full.saturating_sub(2));
    let key = NodeKey::Window(w.id.clone());
    let cursor = ctx.is_cursor(&key);
    let into = ctx.join_into(&w.id);
    let (set, color) = if cursor {
        (HEAVY, p.cursor_bg)
    } else if into {
        (DASHED, p.drop_line)
    } else if w.active {
        (THIN, p.border_cur)
    } else {
        (THIN, p.border)
    };
    let frame = ctx.plain().fg(color);
    let open = expanded(w, false);
    let last = y0 + 1 + if open { w.panes.len() as u16 } else { 0 };
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
        c.set_hit(0, full, y, &Hit::Window(w.id.clone()));
    }

    // The frame carries the state; the title keeps the plain ground unless
    // it is the cursor or the drop target.
    let mut look = ctx.win_look(w);
    if !cursor && !into {
        let bg = if ctx.is_hover(&key) { p.hover_bg } else { p.bg };
        look.base = look.base.bg(bg);
        if w.active {
            look.base = look.base.fg(p.cur_win_fg);
        }
    }
    let title_end = x1.saturating_sub(3);
    let x = c.put(x0 + 1, y0, " ", look.base, title_end);
    let x = put_label(c, ctx, w, x, y0, title_end.saturating_sub(1), &look);
    c.put(x, y0, " ", look.base, title_end);
    if !w.panes.is_empty() {
        c.put(
            x1 - 3,
            y0,
            &format!(" {} ", ctx.chevron(w)),
            frame.fg(if w.active { p.cur_win_sub } else { p.muted }),
            x1,
        );
        c.set_hit(x1 - 3, x1, y0, &Hit::Chevron(w.id.clone()));
    }
    nodes.push((key, y0, y0));
    if !open {
        return;
    }
    for (i, pane) in w.panes.iter().enumerate() {
        let y = y0 + 1 + i as u16;
        let look = ctx.pane_look(w, pane);
        c.fill(x0 + 1, x1, y, look.base);
        let pkey = NodeKey::Pane(pane.id.clone());
        let dot_color = if ctx.is_cursor(&pkey) {
            p.cursor_fg
        } else if pane.active {
            p.dot_active
        } else {
            p.dot
        };
        c.put(x0 + 2, y, if pane.active { "■" } else { "□" }, look.base.fg(dot_color), x1);
        let num = (i + 1).to_string();
        let x = c.put(x0 + 4, y, &num, look.base.fg(look.num), x1);
        put_cmd(c, ctx, pane, x + 1, y, look.base, x1.saturating_sub(1));
        c.set_hit(0, full, y, &Hit::Pane { wid: w.id.clone(), pid: pane.id.clone() });
        nodes.push((pkey, y, y));
    }
}

/// Pane rectangles scaled into a small grid; corners are shared borders.
struct MapGeom {
    w: u16,
    h: u16,
    /// Per work pane, in order: x0, y0, x1, y1 (border lines, inclusive).
    rects: Vec<[u16; 4]>,
}

/// Places sorted edge values on `0..=span`, keeping neighbours at least
/// `gap` apart. With `fit`, the result is squeezed back inside `span`.
fn scale_edges(edges: &[u32], total: u32, span: u16, gap: u16, fit: bool) -> Vec<u16> {
    let total = total.max(1) as f64;
    let mut pos: Vec<i32> = edges.iter().map(|&e| (e as f64 * span as f64 / total).round() as i32).collect();
    let gap = gap as i32;
    for i in 1..pos.len() {
        pos[i] = pos[i].max(pos[i - 1] + gap);
    }
    if fit && let Some(last) = pos.last_mut() {
        *last = (*last).min(span as i32);
        for i in (0..pos.len().saturating_sub(1)).rev() {
            pos[i] = pos[i].min(pos[i + 1] - gap);
        }
    }
    pos.into_iter().map(|p| p.max(0) as u16).collect()
}

fn map_geom(w: &Win, cw: u16) -> Option<MapGeom> {
    if cw < 5 || w.panes.is_empty() {
        return None;
    }
    let min_l = w.panes.iter().map(|p| p.left).min()? as u32;
    let min_t = w.panes.iter().map(|p| p.top).min()? as u32;
    // A pane spans from its own left edge to the border after it.
    let xe: Vec<(u32, u32)> =
        w.panes.iter().map(|p| (p.left as u32 - min_l, p.left as u32 + p.width as u32 + 1 - min_l)).collect();
    let ye: Vec<(u32, u32)> =
        w.panes.iter().map(|p| (p.top as u32 - min_t, p.top as u32 + p.height as u32 + 1 - min_t)).collect();
    let tw = xe.iter().map(|e| e.1).max()?;
    let th = ye.iter().map(|e| e.1).max()?;
    let distinct = |es: &[(u32, u32)]| {
        let mut v: Vec<u32> = es.iter().flat_map(|e| [e.0, e.1]).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    let (xs, ys) = (distinct(&xe), distinct(&ye));
    let span_x = cw - 1;
    let px = scale_edges(&xs, tw, span_x, 2, true);
    // As short as the design's map: the smallest pane gets one text row,
    // the rest in proportion, six rows at most. tmux never splits exactly in
    // half (a border takes a row), hence the slack.
    let min_h = w.panes.iter().map(|p| p.height).min()?.max(1) as f64;
    let rows = (th.saturating_sub(1) as f64 / min_h - 0.15).ceil().clamp(1.0, 6.0) as u16;
    let span_y = rows + ys.len() as u16 - 1;
    let py = scale_edges(&ys, th, span_y, 2, false);
    let at = |edges: &[u32], pos: &[u16], e: u32| edges.binary_search(&e).map(|i| pos[i]).unwrap_or(0);
    let rects = xe
        .iter()
        .zip(&ye)
        .map(|(x, y)| [at(&xs, &px, x.0), at(&ys, &py, y.0), at(&xs, &px, x.1), at(&ys, &py, y.1)])
        .collect();
    Some(MapGeom { w: cw, h: py.last().copied().unwrap_or(0) + 1, rects })
}

fn box_char(bits: u8) -> &'static str {
    const U: u8 = 1;
    const D: u8 = 2;
    const L: u8 = 4;
    const R: u8 = 8;
    match bits {
        0 => " ",
        b if b == U | D | L | R => "┼",
        b if b == U | D | R => "├",
        b if b == U | D | L => "┤",
        b if b == D | L | R => "┬",
        b if b == U | L | R => "┴",
        b if b == D | R => "┌",
        b if b == D | L => "┐",
        b if b == U | R => "└",
        b if b == U | L => "┘",
        b if b & (U | D) != 0 && b & (L | R) == 0 => "│",
        _ => "─",
    }
}

/// 1c: the window's real split layout as a grid of cells.
fn draw_map(c: &mut Canvas<Hit>, y0: u16, ctx: &Ctx, w: &Win, nodes: &mut Vec<(NodeKey, u16, u16)>) {
    let p = ctx.pal;
    let full = ctx.width;
    let end = full.saturating_sub(1);
    let look = ctx.win_look(w);
    c.fill(1, end, y0, look.base);
    let chev = end.saturating_sub(2);
    put_label(c, ctx, w, 2, y0, chev.saturating_sub(1), &look);
    c.set_hit(0, full, y0, &Hit::Window(w.id.clone()));
    if !w.panes.is_empty() {
        c.put(chev, y0, ctx.chevron(w), look.base.fg(look.sub), end);
        c.set_hit(chev.saturating_sub(1), end, y0, &Hit::Chevron(w.id.clone()));
    }
    nodes.push((NodeKey::Window(w.id.clone()), y0, y0));
    if !expanded(w, false) {
        return;
    }
    let Some(g) = map_geom(w, full.saturating_sub(3)) else { return };
    let (ox, oy) = (2u16, y0 + 1);

    let mut bits = vec![0u8; g.w as usize * g.h as usize];
    let mut add = |x: u16, y: u16, b: u8| {
        if x < g.w && y < g.h {
            bits[y as usize * g.w as usize + x as usize] |= b;
        }
    };
    for r in &g.rects {
        let [x0, y0r, x1, y1] = *r;
        for x in x0..x1 {
            for y in [y0r, y1] {
                add(x, y, 8);
                add(x + 1, y, 4);
            }
        }
        for y in y0r..y1 {
            for x in [x0, x1] {
                add(x, y, 2);
                add(x, y + 1, 1);
            }
        }
    }
    let border = ctx.plain().fg(p.border);
    for y in 0..g.h {
        for x in 0..g.w {
            let b = bits[y as usize * g.w as usize + x as usize];
            if b != 0 {
                c.put(ox + x, oy + y, box_char(b), border, full);
            }
        }
    }

    for (i, (pane, r)) in w.panes.iter().zip(&g.rects).enumerate() {
        let [x0, y0r, x1, y1] = *r;
        let hit = Hit::Pane { wid: w.id.clone(), pid: pane.id.clone() };
        for y in y0r..=y1 {
            c.set_hit(ox + x0, ox + x1 + 1, oy + y, &hit);
        }
        let mut look = ctx.pane_look(w, pane);
        if !(w.active && pane.active) && !ctx.is_cursor(&NodeKey::Pane(pane.id.clone())) && !ctx.swap_into(&pane.id) {
            look.base = look.base.without(BOLD);
        }
        for y in y0r + 1..y1 {
            c.fill(ox + x0 + 1, ox + x1, oy + y, look.base);
        }
        if y1 > y0r + 1 && x1 > x0 + 1 {
            let limit = ox + x1;
            let x = c.put(ox + x0 + 1, oy + y0r + 1, &(i + 1).to_string(), look.base.fg(look.num).with(BOLD), limit);
            put_cmd(c, ctx, pane, x + 1, oy + y0r + 1, look.base, limit);
        }
        nodes.push((NodeKey::Pane(pane.id.clone()), oy + y0r, oy + y1));
    }

    // Recolour the borders of the panes that matter, most important last.
    let mut outline = |r: &[u16; 4], style: Style| {
        let [x0, y0r, x1, y1] = *r;
        for x in x0..=x1 {
            for y in [y0r, y1] {
                c.restyle(ox + x, ox + x + 1, oy + y, |s| *s = s.fg(style.fg).with(style.attrs));
            }
        }
        for y in y0r..=y1 {
            for x in [x0, x1] {
                c.restyle(ox + x, ox + x + 1, oy + y, |s| *s = s.fg(style.fg).with(style.attrs));
            }
        }
    };
    let pairs: Vec<(&PaneInfo, &[u16; 4])> = w.panes.iter().zip(&g.rects).collect();
    for (pane, r) in &pairs {
        if pane.active {
            outline(r, Style::default().fg(if w.active { p.border_cur } else { p.guide }));
        }
    }
    for (pane, r) in &pairs {
        if ctx.swap_into(&pane.id) {
            outline(r, Style::default().fg(p.drop_line));
        }
    }
    for (pane, r) in &pairs {
        if ctx.is_cursor(&NodeKey::Pane(pane.id.clone())) {
            outline(r, Style::default().fg(p.cursor_bg).with(BOLD));
        }
    }
}

/// The rail: window numbers only.
fn draw_rail(c: &mut Canvas<Hit>, y: u16, ctx: &Ctx, w: &Win, nodes: &mut Vec<(NodeKey, u16, u16)>) {
    let look = ctx.win_look(w);
    let text = w.index.to_string();
    let mut bw = (width(&text) as u16 + 2).max(4).min(ctx.width);
    // Match the sidebar's parity so the number sits dead centre.
    if (ctx.width - bw) % 2 == 1 && bw < ctx.width {
        bw += 1;
    }
    let x = (ctx.width - bw) / 2;
    c.fill(x, x + bw, y, look.base);
    c.put(x + (bw - width(&text) as u16) / 2, y, &text, look.base, x + bw);
    c.set_hit(0, ctx.width, y, &Hit::Window(w.id.clone()));
    nodes.push((NodeKey::Window(w.id.clone()), y, y));
}

/// Session name and window count; on the right the TREE badge and, with
/// `buttons`, the two split buttons; then a rule. Two rows.
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
            // Icon buttons, as in the design; their tooltip names the
            // split and the user's own key for it.
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
        if focused {
            let badge = " TREE ";
            let bx = right.saturating_sub(width(badge) as u16);
            c.put(bx, 0, badge, Style::default().bg(p.cursor_bg).fg(p.cursor_fg).with(BOLD), right);
            right = bx.saturating_sub(1);
        }
        let x = put_text(c, 1, 0, name, ctx.plain().fg(p.accent).with(BOLD), right);
        let count = text.windows(ctx.snap.windows.len());
        if x + 2 + width(&count) as u16 <= right {
            c.put(x + 2, 0, &count, ctx.plain().fg(p.muted), right);
        }
    }
    let rule = ctx.plain().fg(p.divider);
    for x in 0..full {
        c.put(x, 1, "─", rule, full);
    }
    // A button's tooltip hangs below it, right-aligned to it.
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

    fn ctx<'a>(snap: &'a Snapshot, pal: &'a Palette, variant: Variant, width: u16) -> Ctx<'a> {
        Ctx {
            snap,
            pal,
            width,
            rail: false,
            variant,
            label_styles: true,
            cursor: None,
            hover: None,
            hover_split: None,
            drag: None,
            tip_for: None,
            tip: RefCell::new(None),
        }
    }

    #[test]
    fn branches_layout() {
        let snap = sample();
        let pal = theme::light();
        let l = list(&ctx(&snap, &pal, Variant::Branches, 28));
        let rows: Vec<String> = (0..l.canvas.h).map(|y| l.canvas.row_text(y).trim_end().to_string()).collect();
        assert_eq!(
            rows,
            vec![" ▾ 1:editor*", " ├─ 1 nvim", " ├─ 2 zsh", " └─ 3 zsh", " ▸ 2:server#", " ▸ 3:git-", " ▸ 4:db!",]
        );
        assert_eq!(l.canvas.hit(1, 0), Some(&Hit::Chevron("@1".into())));
        assert_eq!(l.canvas.hit(4, 0), Some(&Hit::Window("@1".into())));
        assert_eq!(l.canvas.hit(10, 2), Some(&Hit::Pane { wid: "@1".into(), pid: "%2".into() }));
        assert_eq!(l.groups[0].y1, 3);
    }

    #[test]
    fn single_pane_windows_unfold_too() {
        let mut snap = sample();
        snap.windows[2].collapsed = false;
        let pal = theme::light();
        let l = list(&ctx(&snap, &pal, Variant::Branches, 28));
        assert_eq!(l.canvas.row_text(5).trim_end(), " ▾ 3:git-");
        assert_eq!(l.canvas.row_text(6).trim_end(), " └─ 1 lazygit");
        assert_eq!(l.canvas.hit(1, 5), Some(&Hit::Chevron("@3".into())));
    }

    #[test]
    fn long_labels_are_cut() {
        let mut snap = sample();
        snap.windows[2].label = Label::parse("3:a-very-long-window-name-indeed");
        let pal = theme::light();
        let l = list(&ctx(&snap, &pal, Variant::Branches, 20));
        assert_eq!(l.canvas.row_text(5).trim_end(), " ▸ 3:a-very-long-…");
    }

    #[test]
    fn blocks_layout() {
        let snap = sample();
        let pal = theme::light();
        let l = list(&ctx(&snap, &pal, Variant::Blocks, 24));
        assert_eq!(l.canvas.row_text(0), " ┌ 1:editor* ────── ▾ ┐ ");
        assert_eq!(l.canvas.row_text(1), " │ ■ 1 nvim           │ ");
        assert_eq!(l.canvas.row_text(4), " └────────────────────┘ ");
        // server is collapsed: frame only.
        assert_eq!(l.canvas.row_text(5), " ┌ 2:server# ────── ▸ ┐ ");
        assert_eq!(l.canvas.row_text(6), " └────────────────────┘ ");
    }

    #[test]
    fn map_follows_real_splits() {
        let snap = sample();
        let pal = theme::light();
        let l = list(&ctx(&snap, &pal, Variant::Map, 28));
        let rows: Vec<String> = (0..l.canvas.h).map(|y| l.canvas.row_text(y)).collect();
        // nvim on the left, two zsh stacked on the right.
        assert!(rows[1].contains('┬'), "{rows:#?}");
        assert!(rows.iter().any(|r| r.contains('├') || r.contains('┤')), "{rows:#?}");
        assert!(rows.iter().any(|r| r.contains("1 nvim")), "{rows:#?}");
        // Narrow sidebars fall back to branches.
        let narrow = list(&ctx(&snap, &pal, Variant::Map, 20));
        assert_eq!(narrow.canvas.row_text(1).trim_end(), " ├─ 1 nvim");
    }

    #[test]
    fn rail_shows_numbers() {
        let snap = sample();
        let pal = theme::light();
        let mut c = ctx(&snap, &pal, Variant::Branches, 7);
        c.rail = true;
        let l = list(&c);
        assert_eq!(l.canvas.h, 4);
        assert_eq!(l.canvas.row_text(0), "   1   ");
    }

    #[test]
    fn map_is_as_short_as_its_smallest_pane_allows() {
        let snap = sample();
        let pal = theme::light();
        let l = list(&ctx(&snap, &pal, Variant::Map, 28));
        let rows: Vec<String> = (0..l.canvas.h).map(|y| l.canvas.row_text(y).trim_end().to_string()).collect();
        // Window row, then five lines: the two right panes get a row each.
        assert_eq!(
            rows[1..6],
            [
                "  ┌───────────┬───────────┐",
                "  │1 nvim     │2 zsh      │",
                "  │           ├───────────┤",
                "  │           │3 zsh      │",
                "  └───────────┴───────────┘",
            ]
        );
    }

    #[test]
    fn split_buttons_sit_right_of_the_title() {
        let mut snap = sample();
        snap.cfg.prefix = "C-x".into();
        snap.cfg.split_h_key = "\\".into();
        let pal = theme::light();
        let text = Strings::new(crate::config::Lang::Zh);
        let mut c = Canvas::new(28, 4, Style::default());
        let mut cx = ctx(&snap, &pal, Variant::Branches, 28);
        assert_eq!(header(&mut c, &cx, true, true, &text), 2);
        assert_eq!(c.row_text(0).trim_end(), " work          TREE   ◫  ⊟");
        assert_eq!(c.hit(22, 0), Some(&Hit::Split('h')));
        assert_eq!(c.hit(25, 0), Some(&Hit::Split('v')));
        // Hovering one shows what it does and the user's key.
        cx.hover_split = Some('h');
        let mut c = Canvas::new(28, 4, Style::default());
        header(&mut c, &cx, false, true, &text);
        assert_eq!(c.row_text(0).trim_end(), " work  4 windows      ◫  ⊟");
        assert_eq!(c.row_text(1), "────── 水平分割 · C-x \\ ────");
        // No key bound: the name alone.
        cx.hover_split = Some('v');
        let mut c = Canvas::new(28, 4, Style::default());
        header(&mut c, &cx, false, true, &text);
        assert_eq!(c.row_text(1), "───────────────── 竖直分割 ─");
    }

    #[test]
    fn hovered_cut_text_gets_a_tooltip() {
        let mut snap = sample();
        snap.windows[2].label = Label::parse("3:staging-db-migrations");
        let pal = theme::light();
        let key = NodeKey::Window("@3".into());
        let mut c = ctx(&snap, &pal, Variant::Branches, 20);
        c.tip_for = Some(&key);
        list(&c);
        assert_eq!(c.tip.borrow().clone(), Some(Tip { x: 3, y: 5, text: "3:staging-db-migrations".into() }));
        // Text that fits needs none.
        let fits = NodeKey::Window("@4".into());
        c.tip_for = Some(&fits);
        *c.tip.borrow_mut() = None;
        list(&c);
        assert_eq!(*c.tip.borrow(), None);
    }

    #[test]
    fn tooltips_shift_then_wrap() {
        let snap = sample();
        let pal = theme::light();
        let c = ctx(&snap, &pal, Variant::Branches, 20);
        let mut canvas = Canvas::new(20, 3, Style::default());
        tooltip(&mut canvas, &c, 5, 0, "staging-db-mig", 3);
        assert_eq!(canvas.row_text(0), "     staging-db-mig ");
        let mut canvas = Canvas::new(20, 3, Style::default());
        tooltip(&mut canvas, &c, 5, 1, "3:staging-db-migrations", 3);
        assert_eq!(canvas.row_text(1), " 3:staging-db-migra ");
        assert_eq!(canvas.row_text(2), " tions              ");
    }

    #[test]
    fn edges_keep_their_gaps() {
        assert_eq!(scale_edges(&[0, 46, 92], 92, 24, 2, true), vec![0, 12, 24]);
        assert_eq!(scale_edges(&[0, 1, 2, 3], 3, 4, 2, true), vec![0, 0, 2, 4]);
        assert_eq!(scale_edges(&[0, 1, 41], 41, 4, 2, false), vec![0, 2, 4]);
    }
}
