//! The process inside the sidebar pane: draws the tree, reads keys and
//! mouse, and turns them into tmux commands. It redraws when a hook signals
//! it (SIGUSR1), re-detects the theme on SIGUSR2, and handles its own
//! resizes on SIGWINCH.

use std::cell::RefCell;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::cli;
use crate::config::{Config, HIDE_BELOW, MAX_W, MIN_FULL, Mode, Side, Theme};
use crate::input::{Event, Key, Mouse, MouseKind, Parser};
use crate::label::parse_color;
use crate::model::{NodeKey, Snapshot, expanded, pane_number};
use crate::screen::Canvas;
use crate::split;
use crate::strings::{Strings, wrap};
use crate::term;
use crate::theme::{self, Palette};
use crate::tmux::{self, Batch};
use crate::view::{self, Ctx, Drag, Footer, Group, Hit, Target};

/// How long a burst of SIGWINCH must settle before a resize is judged.
const RESIZE_SETTLE: Duration = Duration::from_millis(150);
/// Hooks often fire in bursts; coalesce the redraws they ask for.
const REFRESH_COALESCE: Duration = Duration::from_millis(15);
const ESC_WAIT: Duration = Duration::from_millis(25);
const MESSAGE_TIME: Duration = Duration::from_millis(1600);
/// tmux never says the pointer left the pane, so hover (and its tooltip)
/// ends this long after the last motion; sooner when that motion was on
/// the edge facing the work area.
const HOVER_IDLE: Duration = Duration::from_secs(3);
const HOVER_EDGE: Duration = Duration::from_millis(300);
/// A drag whose end tmux never reported (released off any border or pane)
/// is finished after this long without movement.
const DRAG_STALE: Duration = Duration::from_secs(2);

struct Signals {
    winch: UnixStream,
    usr1: UnixStream,
    usr2: UnixStream,
    term: UnixStream,
}

impl Signals {
    fn new() -> io::Result<Signals> {
        fn pipe(sigs: &[i32]) -> io::Result<UnixStream> {
            let (r, w) = UnixStream::pair()?;
            r.set_nonblocking(true)?;
            w.set_nonblocking(true)?;
            for &sig in sigs {
                signal_hook::low_level::pipe::register(sig, w.try_clone()?)?;
            }
            Ok(r)
        }
        use signal_hook::consts::*;
        Ok(Signals {
            winch: pipe(&[SIGWINCH])?,
            usr1: pipe(&[SIGUSR1])?,
            usr2: pipe(&[SIGUSR2])?,
            term: pipe(&[SIGTERM, SIGHUP, SIGINT])?,
        })
    }
}

/// Empties a signal pipe; returns whether anything was in it.
fn drain(mut s: &UnixStream) -> bool {
    let mut buf = [0u8; 64];
    let mut any = false;
    while let Ok(n) = s.read(&mut buf) {
        if n == 0 {
            break;
        }
        any = true;
    }
    any
}

struct Press {
    x: u16,
    y: u16,
    hit: Hit,
}

/// What the last frame put where, for mouse hit-testing.
struct Frame {
    screen: Canvas<Hit>,
    /// Window groups in screen rows (may run off either end).
    groups: Vec<(String, i32, i32)>,
    list_top: u16,
    list_h: u16,
    list_total: u16,
}

struct App {
    pane: String,
    snap: Option<Snapshot>,
    size: (u16, u16),
    pal: Palette,
    /// Whether the terminal background looked light, once tmux answered.
    light_bg: Option<bool>,
    parser: Parser,
    cursor: Option<NodeKey>,
    hover: Option<NodeKey>,
    hover_split: Option<char>,
    hover_until: Option<Instant>,
    /// A node to scroll into view on the next frame.
    reveal: Option<NodeKey>,
    scroll: u16,
    press: Option<Press>,
    drag: Option<Drag>,
    frame: Option<Frame>,
    msg: Option<(String, Instant)>,
    was_focused: bool,
    was_dragging: bool,
    /// Keys only reach the active pane, so a key means we have focus even
    /// before the snapshot says so.
    key_focus: bool,
    last_current: Option<String>,
    last_fix: Option<(Instant, (u16, u16))>,
    resize_at: Option<Instant>,
    refresh_at: Option<Instant>,
    esc_at: Option<Instant>,
    tick_at: Option<Instant>,
    drag_check_at: Option<Instant>,
    quit: bool,
}

pub fn run() -> Result<()> {
    let pane = std::env::var("TMUX_PANE").context("arbor sidebar runs inside a tmux pane")?;
    // Handlers first: once @arbor-pid is published, hooks start signalling,
    // and an unhandled SIGUSR1 would kill the process.
    let signals = Signals::new()?;
    Batch::new()
        .cmd(["set-option", "-p", "-t", &pane, "@arbor-sidebar", "1"])
        .cmd(["set-option", "-p", "-t", &pane, "@arbor-pid", &std::process::id().to_string()])
        .run()?;
    let _ = tmux::run(["set-option", "-p", "-t", &pane, "remain-on-exit", "off"]);
    let _raw = term::Raw::enter()?;
    let mut app = App::new(pane);
    // Give tmux a moment to report the background before the first frame.
    term::write(term::QUERY_BG)?;
    app.read_input(Some(Duration::from_millis(60)))?;
    app.refresh();
    app.run(&signals)
}

impl App {
    fn new(pane: String) -> App {
        App {
            pane,
            snap: None,
            size: term::size(),
            pal: theme::dark(),
            light_bg: None,
            parser: Parser::default(),
            cursor: None,
            hover: None,
            hover_split: None,
            hover_until: None,
            reveal: None,
            scroll: 0,
            press: None,
            drag: None,
            frame: None,
            msg: None,
            was_focused: false,
            was_dragging: false,
            key_focus: false,
            last_current: None,
            last_fix: None,
            resize_at: None,
            refresh_at: None,
            esc_at: None,
            tick_at: None,
            drag_check_at: None,
            quit: false,
        }
    }

    fn run(&mut self, sig: &Signals) -> Result<()> {
        while !self.quit {
            let now = Instant::now();
            let next = [
                self.resize_at,
                self.refresh_at,
                self.esc_at,
                self.tick_at,
                self.hover_until,
                self.msg.as_ref().map(|m| m.1),
            ]
            .into_iter()
            .flatten()
            .min();
            let timeout = next.map(|t| t.saturating_duration_since(now));
            let mut fds = [
                pollfd(0),
                pollfd(sig.winch.as_raw_fd()),
                pollfd(sig.usr1.as_raw_fd()),
                pollfd(sig.usr2.as_raw_fd()),
                pollfd(sig.term.as_raw_fd()),
            ];
            poll(&mut fds, timeout)?;
            if fds[0].revents & (libc::POLLHUP | libc::POLLERR) != 0 && fds[0].revents & libc::POLLIN == 0 {
                break;
            }
            if fds[0].revents & libc::POLLIN != 0 {
                self.read_input(None)?;
            }
            if drain(&sig.term) {
                break;
            }
            if drain(&sig.winch) {
                self.size = term::size();
                self.resize_at = Some(Instant::now() + RESIZE_SETTLE);
                // Still moving: not a drag tmux forgot to end.
                if self.drag_check_at.is_some() {
                    self.drag_check_at = Some(Instant::now() + DRAG_STALE);
                }
                self.draw();
            }
            if drain(&sig.usr1) {
                self.refresh_at.get_or_insert(Instant::now() + REFRESH_COALESCE);
            }
            if drain(&sig.usr2) {
                term::write(term::QUERY_BG)?;
                self.refresh_at.get_or_insert(Instant::now() + REFRESH_COALESCE);
            }
            self.run_timers();
        }
        Ok(())
    }

    fn run_timers(&mut self) {
        let now = Instant::now();
        let due = |t: Option<Instant>| t.is_some_and(|t| t <= now);
        if due(self.esc_at) {
            self.esc_at = None;
            if let Some(ev) = self.parser.flush_esc() {
                self.handle(ev);
            }
        }
        if due(self.resize_at) {
            self.settle_resize();
        }
        if due(self.refresh_at) {
            self.refresh();
        }
        if due(self.drag_check_at) {
            self.drag_check_at = None;
            if self.snap.as_ref().is_some_and(|s| s.dragging) {
                let window = self.snap.as_ref().map(|s| s.window_id.clone()).unwrap_or_default();
                let _ = cli::drag_end(Some(&window));
            }
        }
        if due(self.tick_at) {
            self.tick();
        }
        if due(self.msg.as_ref().map(|m| m.1)) {
            self.msg = None;
            self.draw();
        }
        if due(self.hover_until) {
            self.hover_until = None;
            self.hover = None;
            self.hover_split = None;
            self.draw();
        }
    }

    /// Reads whatever stdin has (waiting up to `wait` for the first bytes)
    /// and handles the events in it.
    fn read_input(&mut self, wait: Option<Duration>) -> Result<()> {
        if let Some(wait) = wait {
            let mut fds = [pollfd(0)];
            poll(&mut fds, Some(wait))?;
            if fds[0].revents & libc::POLLIN == 0 {
                return Ok(());
            }
        }
        let mut buf = [0u8; 4096];
        // SAFETY: read(2) into a stack buffer of the given length.
        let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
        if n == 0 {
            self.quit = true;
            return Ok(());
        }
        if n < 0 {
            let e = io::Error::last_os_error();
            return if e.kind() == io::ErrorKind::Interrupted || e.kind() == io::ErrorKind::WouldBlock {
                Ok(())
            } else {
                Err(e.into())
            };
        }
        self.parser.feed(&buf[..n as usize]);
        while let Some(ev) = self.parser.next() {
            self.handle(ev);
        }
        self.esc_at = self.parser.pending_esc().then(|| Instant::now() + ESC_WAIT);
        Ok(())
    }

    fn cfg(&self) -> Config {
        self.snap.as_ref().map(|s| s.cfg.clone()).unwrap_or_default()
    }

    fn rail(&self) -> bool {
        self.cfg().mode == Mode::Rail || self.size.0 < MIN_FULL
    }

    fn focused(&self) -> bool {
        self.key_focus || self.snap.as_ref().is_some_and(Snapshot::focused)
    }

    fn say(&mut self, text: String) {
        self.msg = Some((text, Instant::now() + MESSAGE_TIME));
    }

    fn strings(&self) -> Strings {
        Strings::new(self.cfg().lang)
    }

    fn update_palette(&mut self) {
        let cfg = self.cfg();
        let light = match cfg.theme {
            Theme::Light => true,
            Theme::Dark => false,
            Theme::Auto => self.light_bg.unwrap_or(false),
        };
        let mut pal = if light { theme::light() } else { theme::dark() };
        if let Some(bg) = parse_color(&cfg.bg) {
            pal.bg = bg;
        }
        self.pal = pal;
    }

    // ---- tmux state ------------------------------------------------------

    fn query(&mut self) -> Option<Snapshot> {
        match Snapshot::query(&self.pane) {
            Ok(s) => Some(s),
            Err(e) => {
                let e = format!("{e:#}");
                if e.contains("can't find") || e.contains("no server") || e.contains("error connecting") {
                    self.quit = true;
                }
                None
            }
        }
    }

    /// Takes in a fresh snapshot: exits when the sidebar should not exist,
    /// tracks focus and keeps the cursor on a real node.
    fn apply(&mut self, snap: Snapshot) {
        if snap.cfg.mode == Mode::Hidden {
            self.quit = true;
            return;
        }
        let Some(mine) = snap.my_window() else {
            self.quit = true;
            return;
        };
        // Alone in the window (its last shell exited or moved away), or a
        // duplicate from racing hooks: close the pane.
        if mine.panes.is_empty() || mine.sidebars.iter().any(|s| pane_number(s) < pane_number(&self.pane)) {
            self.quit = true;
            return;
        }
        let focused = snap.focused();
        if focused && !self.was_focused {
            self.cursor = snap.current().map(|w| NodeKey::Window(w.id.clone()));
            self.reveal = self.cursor.clone();
        }
        self.was_focused = focused;
        // A drag just ended and settled the width: say what it became.
        if self.was_dragging && !snap.dragging {
            let before = self.snap.as_ref().map(|s| (s.cfg.mode, s.cfg.width));
            if before != Some((snap.cfg.mode, snap.cfg.width)) {
                let text = Strings::new(snap.cfg.lang);
                self.say(if snap.cfg.mode == Mode::Rail { text.rail() } else { text.cols_saved(snap.cfg.width) });
            }
        }
        self.was_dragging = snap.dragging;
        if !snap.dragging {
            self.drag_check_at = None;
        }
        self.key_focus = false;
        let current = snap.current().map(|w| w.id.clone());
        if current != self.last_current {
            self.reveal = self.reveal.take().or_else(|| current.clone().map(NodeKey::Window));
            self.last_current = current;
            self.hover = None;
        }
        let rail = snap.cfg.mode == Mode::Rail || self.size.0 < MIN_FULL;
        if let Some(c) = &self.cursor
            && !snap.nodes(rail).contains(c)
        {
            self.cursor = match c {
                NodeKey::Pane(p) => snap.find_pane(p).map(|(w, _)| NodeKey::Window(w.id.clone())),
                NodeKey::Window(_) => None,
            }
            .or_else(|| snap.current().map(|w| NodeKey::Window(w.id.clone())));
        }
        self.tick_at =
            (snap.cfg.interval > 0 && snap.visible()).then(|| Instant::now() + Duration::from_secs(snap.cfg.interval));
        self.snap = Some(snap);
        self.update_palette();
    }

    fn refresh(&mut self) {
        self.refresh_at = None;
        let Some(snap) = self.query() else { return };
        self.apply(snap);
        if self.quit {
            return;
        }
        self.fix_geometry();
        self.draw();
    }

    /// The periodic check for changed pane commands; redraws only on change.
    fn tick(&mut self) {
        self.tick_at = None;
        let Some(snap) = self.query() else { return };
        let changed = self.snap.as_ref() != Some(&snap);
        self.apply(snap);
        if changed && !self.quit {
            self.draw();
        }
    }

    /// Re-docks or re-sizes this sidebar when something other than the user
    /// moved it. Skipped while a resize is settling: that one is judged
    /// separately, since it may be a drag.
    fn fix_geometry(&mut self) {
        let Some(snap) = &self.snap else { return };
        if snap.zoomed || snap.dragging || self.resize_at.is_some() {
            return;
        }
        let cfg = &snap.cfg;
        if snap.docked(cfg.position) && snap.pane_width == cfg.cols() {
            return;
        }
        // A window too narrow for the width would otherwise be retried on
        // every redraw.
        let key = (cfg.cols(), snap.window_width);
        if self.last_fix.is_some_and(|(t, k)| k == key && t.elapsed() < Duration::from_secs(1)) {
            return;
        }
        self.last_fix = Some((Instant::now(), key));
        let _ = cli::fix_sidebar(&self.pane, cfg);
    }

    fn settle_resize(&mut self) {
        self.resize_at = None;
        let Some(snap) = self.query() else { return };
        self.apply(snap);
        if self.quit {
            return;
        }
        let Some(snap) = &self.snap else { return };
        if snap.zoomed {
            self.draw();
            return;
        }
        let cfg = snap.cfg.clone();
        let docked = snap.docked(cfg.position);
        let w = snap.pane_width;
        if w == cfg.cols() && docked {
            self.draw();
            return;
        }
        // Only dragging the divider changes the width on purpose. Layout
        // commands, splits and terminal resizes are put back.
        if docked && snap.dragging {
            // tmux moves the divider with the pointer, and resizing under it
            // would lose the drag: show where it would land, and settle on
            // release (arbor drag-end).
            let text = self.strings();
            self.say(match w {
                w if w < HIDE_BELOW => text.release_hides(),
                w if w < MIN_FULL => text.rail(),
                w => text.cols(w.min(MAX_W)),
            });
            self.drag_check_at = Some(Instant::now() + DRAG_STALE);
        } else {
            let _ = cli::fix_sidebar(&self.pane, &cfg);
        }
        self.draw();
    }

    fn commit(&mut self, cfg: Config) {
        let _ = cli::commit(&cfg);
        if let Some(s) = &mut self.snap {
            s.cfg = cfg;
        }
    }

    // ---- input -----------------------------------------------------------

    fn handle(&mut self, ev: Event) {
        match ev {
            Event::Key(k) => {
                self.key_focus = true;
                self.key(k);
            }
            Event::Mouse(m) => self.mouse(m),
            Event::FocusIn | Event::FocusOut => {
                if ev == Event::FocusOut {
                    self.hover = None;
                }
                self.refresh_at.get_or_insert(Instant::now());
            }
            Event::Background(r, g, b) => {
                self.light_bg = Some(theme::is_light(r, g, b));
                self.update_palette();
                self.draw();
            }
        }
    }

    fn key(&mut self, k: Key) {
        let Some(snap) = self.snap.clone() else { return };
        let rail = self.rail();
        let nodes = snap.nodes(rail);
        if nodes.is_empty() {
            return;
        }
        let current = snap.current().map(|w| NodeKey::Window(w.id.clone()));
        let i = self.cursor.as_ref().or(current.as_ref()).and_then(|c| nodes.iter().position(|n| n == c)).unwrap_or(0);
        let node = nodes[i].clone();
        let window = match &node {
            NodeKey::Window(w) => snap.window(w),
            NodeKey::Pane(_) => None,
        };
        let foldable = window.is_some_and(|w| !rail && w.panes.len() > 1);
        let text = self.strings();
        let mut cfg = snap.cfg.clone();
        match k {
            Key::Char('j') | Key::Down => self.move_cursor(nodes.get(i + 1)),
            Key::Char('k') | Key::Up => self.move_cursor(i.checked_sub(1).and_then(|j| nodes.get(j))),
            Key::Char('l') | Key::Right => {
                if let Some(w) = window.filter(|_| foldable) {
                    if w.collapsed {
                        self.set_collapsed(&w.id, false);
                    } else {
                        self.move_cursor(nodes.get(i + 1));
                    }
                }
            }
            Key::Char('h') | Key::Left => match (&node, window) {
                (_, Some(w)) if foldable && expanded(w, rail) => self.set_collapsed(&w.id, true),
                (NodeKey::Pane(p), _) => {
                    let parent = snap.find_pane(p).map(|(w, _)| NodeKey::Window(w.id.clone()));
                    self.move_cursor(parent.as_ref());
                }
                _ => {}
            },
            Key::Char(' ') => {
                if let Some(w) = window.filter(|_| foldable) {
                    self.set_collapsed(&w.id, !w.collapsed);
                }
            }
            Key::Enter => self.activate(&node),
            Key::Char(d @ '1'..='9') => {
                let n = d as usize - '1' as usize;
                if let Some(w) = snap.windows.get(n) {
                    self.cursor = Some(NodeKey::Window(w.id.clone()));
                    self.goto_window(&w.id.clone(), true);
                }
            }
            Key::Char('J') => self.shift(&node, 1),
            Key::Char('K') => self.shift(&node, -1),
            Key::Char(c @ ('<' | '>')) => {
                if cli::step_width(&mut cfg, if c == '<' { -2 } else { 2 }) {
                    self.say(if cfg.mode == Mode::Rail { text.rail() } else { text.cols_saved(cfg.width) });
                    self.commit(cfg);
                }
            }
            Key::Char('=') => {
                cfg.mode = Mode::Full;
                cfg.width = cfg.default_width;
                self.say(text.reset(cfg.width));
                self.commit(cfg);
            }
            Key::Char('r') => {
                cfg.mode = if cfg.mode == Mode::Rail { Mode::Full } else { Mode::Rail };
                self.say(if cfg.mode == Mode::Rail { text.rail() } else { text.cols_saved(cfg.width) });
                self.commit(cfg);
            }
            Key::Char('p') => {
                cfg.position = cfg.position.other();
                self.say(text.side(cfg.position == Side::Right));
                let _ = cli::flip(&cfg);
                if let Some(s) = &mut self.snap {
                    s.cfg = cfg;
                }
            }
            Key::Char('q') | Key::Esc => self.leave(),
            _ => return,
        }
        self.draw();
    }

    fn move_cursor(&mut self, to: Option<&NodeKey>) {
        if let Some(n) = to {
            self.cursor = Some(n.clone());
            self.reveal = Some(n.clone());
        }
    }

    fn set_collapsed(&mut self, wid: &str, collapsed: bool) {
        let r = if collapsed {
            tmux::run(["set-option", "-w", "-t", wid, "@arbor-collapsed", "1"])
        } else {
            tmux::run(["set-option", "-wu", "-t", wid, "@arbor-collapsed"])
        };
        if r.is_ok() {
            if let Some(w) = self.snap.as_mut().and_then(|s| s.window_mut(wid)) {
                w.collapsed = collapsed;
            }
            let _ = cli::notify(libc::SIGUSR1);
        }
    }

    /// Selects a work pane in this sidebar's window, so coming back to it
    /// later does not land in the tree.
    fn leave_cmd(&self, b: &mut Batch) {
        if let Some(t) = self.snap.as_ref().and_then(|s| s.my_window()).and_then(|w| w.work_target()) {
            b.cmd(["select-pane", "-t", t]);
        }
    }

    fn leave(&mut self) {
        let mut b = Batch::new();
        self.leave_cmd(&mut b);
        let _ = b.run();
    }

    fn activate(&mut self, node: &NodeKey) {
        match node {
            NodeKey::Window(w) => self.goto_window(&w.clone(), false),
            NodeKey::Pane(p) => {
                if let Some((w, _)) = self.snap.as_ref().and_then(|s| s.find_pane(p)) {
                    let wid = w.id.clone();
                    self.goto_pane(&wid, &p.clone());
                }
            }
        }
    }

    /// Switches to a window. With `keep_tree` the keyboard stays in the
    /// tree, in that window's sidebar.
    fn goto_window(&mut self, wid: &str, keep_tree: bool) {
        let Some(snap) = &self.snap else { return };
        let mut b = Batch::new();
        if wid == snap.window_id {
            if !keep_tree {
                self.leave_cmd(&mut b);
            }
            let _ = b.run();
            return;
        }
        self.leave_cmd(&mut b);
        b.cmd(["select-window", "-t", wid]);
        if b.run().is_err() || !keep_tree {
            return;
        }
        let _ = cli::sync(&snap.cfg, Some(wid));
        if let Some(sb) = cli::window_rows(wid).ok().and_then(|rows| rows.into_iter().find(|r| r.sidebar)) {
            let _ = tmux::run(["select-pane", "-t", &sb.pane]);
        }
    }

    fn goto_pane(&mut self, wid: &str, pid: &str) {
        let Some(snap) = &self.snap else { return };
        let mut b = Batch::new();
        if wid != snap.window_id {
            self.leave_cmd(&mut b);
            b.cmd(["select-window", "-t", wid]);
        }
        b.cmd(["select-pane", "-t", pid]);
        let _ = b.run();
    }

    /// J / K: moves a window or pane one place down or up.
    fn shift(&mut self, node: &NodeKey, d: i32) {
        let Some(snap) = self.snap.clone() else { return };
        match node {
            NodeKey::Window(wid) => {
                let Some(i) = snap.windows.iter().position(|w| &w.id == wid) else { return };
                let Some(other) = (i as i32 + d).try_into().ok().and_then(|j: usize| snap.windows.get(j)) else {
                    return;
                };
                // Which window stays current after swap-window differs
                // between tmux versions; put back the one that was.
                let Ok(current) = tmux::run(["display-message", "-p", "-t", &snap.session_id, "#{window_id}"]) else {
                    return;
                };
                let _ = Batch::new()
                    .cmd(["swap-window", "-s", wid, "-t", &other.id])
                    .cmd(["select-window", "-t", current.trim()])
                    .run();
            }
            NodeKey::Pane(pid) => {
                let Some((w, i)) = snap.find_pane(pid) else { return };
                let Some(other) = (i as i32 + d).try_into().ok().and_then(|j: usize| w.panes.get(j)) else { return };
                swap_panes(&w.id, pid, &other.id);
            }
        }
    }

    // ---- mouse -----------------------------------------------------------

    fn hit_at(&self, x: u16, y: u16) -> Hit {
        self.frame.as_ref().and_then(|f| f.screen.hit(x, y).cloned()).unwrap_or_default()
    }

    fn mouse(&mut self, m: Mouse) {
        let hit = self.hit_at(m.x, m.y);
        match m.kind {
            MouseKind::Press(0) => {
                if let Hit::Chevron(wid) = &hit
                    && let Some(w) = self.snap.as_ref().and_then(|s| s.window(wid))
                {
                    let collapsed = !w.collapsed;
                    self.cursor = Some(NodeKey::Window(wid.clone()));
                    self.set_collapsed(&wid.clone(), collapsed);
                    self.draw();
                    return;
                }
                self.press = Some(Press { x: m.x, y: m.y, hit });
            }
            MouseKind::Drag(0) => {
                let Some(p) = &self.press else { return };
                if self.drag.is_none() {
                    if m.y == p.y && m.x.abs_diff(p.x) < 2 {
                        return;
                    }
                    let src = match &p.hit {
                        Hit::Window(w) => (NodeKey::Window(w.clone()), w.clone()),
                        Hit::Pane { wid, pid } => (NodeKey::Pane(pid.clone()), wid.clone()),
                        _ => return,
                    };
                    self.drag = Some(Drag { src: src.0, src_wid: src.1, target: None });
                }
                let target = self.drop_target(m.x, m.y);
                if let Some(d) = &mut self.drag {
                    d.target = target;
                }
                self.draw();
            }
            MouseKind::Release => {
                if let Some(d) = self.drag.take() {
                    self.press = None;
                    if let Some(t) = d.target.clone() {
                        self.drop(&d, t);
                    }
                    self.draw();
                } else if let Some(p) = self.press.take()
                    && p.hit == hit
                {
                    self.click(&hit);
                }
            }
            MouseKind::Move => {
                let hover = hit.node();
                let hover_split = match hit {
                    Hit::Split(d) => Some(d),
                    _ => None,
                };
                // The sidebar is full height: the pointer leaves it towards
                // the work area.
                let edge = match self.cfg().position {
                    Side::Left => m.x + 1 >= self.size.0,
                    Side::Right => m.x == 0,
                };
                self.hover_until = Some(Instant::now() + if edge { HOVER_EDGE } else { HOVER_IDLE });
                if hover != self.hover || hover_split != self.hover_split {
                    self.hover = hover;
                    self.hover_split = hover_split;
                    self.draw();
                }
            }
            MouseKind::WheelUp | MouseKind::WheelDown => {
                let max = self.frame.as_ref().map(|f| f.list_total.saturating_sub(f.list_h)).unwrap_or(0);
                self.scroll = if m.kind == MouseKind::WheelUp {
                    self.scroll.saturating_sub(3)
                } else {
                    (self.scroll + 3).min(max)
                };
                // Scrolling moves the rows from under the pointer; the
                // design drops tooltips on the wheel too.
                self.hover = None;
                self.hover_split = None;
                self.draw();
            }
            _ => {}
        }
    }

    fn click(&mut self, hit: &Hit) {
        match hit {
            Hit::Window(wid) => {
                self.cursor = Some(NodeKey::Window(wid.clone()));
                self.goto_window(&wid.clone(), false);
            }
            Hit::Pane { wid, pid } => {
                self.cursor = Some(NodeKey::Pane(pid.clone()));
                self.goto_pane(&wid.clone(), &pid.clone());
            }
            Hit::Split(dir) => self.split(*dir),
            _ => {}
        }
    }

    /// A split button: splits the work pane with the user's own binding.
    fn split(&mut self, dir: char) {
        if let Err(e) = split::split(&dir.to_string(), &self.pane) {
            let e = format!("{e:#}");
            let text = self.strings();
            self.say(if e.contains("no space") {
                text.no_space()
            } else {
                e.rsplit(": ").next().unwrap_or(&e).to_string()
            });
            self.draw();
        }
    }

    /// What a drag released at (x, y) would do.
    fn drop_target(&self, x: u16, y: u16) -> Option<Target> {
        let frame = self.frame.as_ref()?;
        let drag = self.drag.as_ref()?;
        let snap = self.snap.as_ref()?;
        match &drag.src {
            NodeKey::Window(src) => {
                if y < frame.list_top || y >= frame.list_top + frame.list_h {
                    return None;
                }
                let yy = y as i32;
                let g = frame.groups.iter().position(|g| yy >= g.1 && yy <= g.2).or_else(|| {
                    // Below the last window: after it.
                    frame.groups.last().filter(|g| yy > g.2).map(|_| frame.groups.len() - 1)
                })?;
                let (wid, y0, y1) = &frame.groups[g];
                if wid == src {
                    return None;
                }
                let src_i = snap.windows.iter().position(|w| &w.id == src)?;
                // Upper half goes before; a one-row window goes the way the
                // drag is heading.
                let before = if y1 > y0 { (yy - y0) * 2 < y1 - y0 + 1 } else { g < src_i };
                let before = before && yy <= *y1;
                Some(if before { Target::Before(wid.clone()) } else { Target::After(wid.clone()) })
            }
            NodeKey::Pane(src) => match frame.screen.hit(x, y)? {
                Hit::NewWindow => Some(Target::Break),
                Hit::Pane { wid, pid } => {
                    if pid == src {
                        None
                    } else if *wid == drag.src_wid {
                        Some(Target::Swap(pid.clone()))
                    } else {
                        Some(Target::Join { wid: wid.clone(), pane: Some(pid.clone()) })
                    }
                }
                Hit::Window(wid) | Hit::Chevron(wid) if *wid != drag.src_wid => {
                    Some(Target::Join { wid: wid.clone(), pane: None })
                }
                _ => None,
            },
        }
    }

    fn drop(&mut self, d: &Drag, t: Target) {
        let Some(snap) = self.snap.clone() else { return };
        let sid = &snap.session_id;
        match (&d.src, &t) {
            (NodeKey::Window(src), Target::Before(dst) | Target::After(dst)) => {
                let (Some(si), Some(di)) =
                    (snap.windows.iter().position(|w| &w.id == src), snap.windows.iter().position(|w| &w.id == dst))
                else {
                    return;
                };
                let after = matches!(t, Target::After(_));
                // Already there.
                if (after && di + 1 == si) || (!after && si + 1 == di) {
                    return;
                }
                let _ = tmux::run([
                    "move-window",
                    "-d",
                    if after { "-a" } else { "-b" },
                    "-s",
                    &format!("{sid}:{src}"),
                    "-t",
                    &format!("{sid}:{dst}"),
                ]);
            }
            (NodeKey::Pane(src), Target::Swap(dst)) => swap_panes(&d.src_wid, src, dst),
            (NodeKey::Pane(src), Target::Join { wid, pane }) => {
                let dst = pane.clone().or_else(|| snap.window(wid).and_then(|w| w.work_target()).map(str::to_string));
                if let Some(dst) = dst {
                    let _ = tmux::run(["join-pane", "-s", src, "-t", &dst]);
                }
            }
            (NodeKey::Pane(src), Target::Break) => {
                let _ = tmux::run(["break-pane", "-s", src]);
            }
            _ => return,
        }
        self.cursor = Some(d.src.clone());
        // The mouse press made the sidebar active; give the keyboard back.
        let _ = cli::focus_work(&self.pane);
    }

    // ---- drawing ---------------------------------------------------------

    fn footer(&self, ctx: &Ctx, text: &Strings, focused: bool) -> Footer {
        if let Some(d) = &self.drag {
            let hint = match &d.target {
                Some(t) => (self.target_hint(t, text), true),
                None => (text.cannot_drop(), false),
            };
            let zone =
                matches!(d.src, NodeKey::Pane(_)).then(|| (text.new_window_zone(), d.target == Some(Target::Break)));
            return Footer { drag: Some(hint), zone, ..Footer::default() };
        }
        if let Some((m, _)) = &self.msg {
            return Footer { lines: vec![m.clone()], accent: true, ..Footer::default() };
        }
        let cfg = &ctx.snap.cfg;
        if !cfg.hints {
            return Footer::default();
        }
        let items = if focused { text.hints_focused() } else { text.hints_idle(&cfg.prefix) };
        let mut lines = wrap(&items, ctx.width.saturating_sub(2) as usize);
        lines.truncate(3);
        Footer { lines, ..Footer::default() }
    }

    fn target_hint(&self, t: &Target, text: &Strings) -> String {
        let snap = self.snap.as_ref();
        let wname =
            |id: &str| snap.and_then(|s| s.window(id)).map(|w| w.label.plain().trim().to_string()).unwrap_or_default();
        match t {
            Target::Before(w) => text.before(&wname(w)),
            Target::After(w) => text.after(&wname(w)),
            Target::Join { wid, .. } => {
                text.join(&snap.and_then(|s| s.window(wid)).map(|w| w.name.clone()).unwrap_or_default())
            }
            Target::Swap(p) => {
                text.swap(&snap.and_then(|s| s.find_pane(p)).map(|(w, i)| w.panes[i].cmd.clone()).unwrap_or_default())
            }
            Target::Break => text.break_out(),
        }
    }

    fn draw(&mut self) {
        let Some(snap) = &self.snap else { return };
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            return;
        }
        let rail = self.rail();
        let focused = self.focused();
        let text = Strings::new(snap.cfg.lang);
        let ctx = Ctx {
            snap,
            pal: &self.pal,
            width: w,
            rail,
            variant: snap.cfg.style,
            label_styles: snap.cfg.label_styles,
            cursor: if focused { self.cursor.as_ref() } else { None },
            hover: self.hover.as_ref(),
            hover_split: self.hover_split,
            drag: self.drag.as_ref(),
            tip_for: self.hover.as_ref().filter(|_| self.press.is_none() && self.drag.is_none()),
            tip: RefCell::new(None),
        };
        let list = view::list(&ctx);
        let mut screen = Canvas::new(w, h, ctx.plain());
        let header_h = if h >= 4 { view::header(&mut screen, &ctx, focused, snap.cfg.buttons, &text) } else { 0 };
        let footer = if rail { Footer::default() } else { self.footer(&ctx, &text, focused) };
        let mut footer_h = footer.height();
        if h < header_h + footer_h + 3 {
            footer_h = 0;
        }
        let list_h = h - header_h - footer_h;

        let max_scroll = list.canvas.h.saturating_sub(list_h);
        if let Some(k) = self.reveal.take() {
            let rows = match &k {
                NodeKey::Window(wid) => list.groups.iter().find(|g| &g.wid == wid).map(|g| (g.y0, g.y1)),
                NodeKey::Pane(_) => list.nodes.iter().find(|n| n.0 == k).map(|n| (n.1, n.2)),
            };
            if let Some((a, b)) = rows {
                let b = b.min(a + list_h.saturating_sub(1));
                if a < self.scroll {
                    self.scroll = a;
                } else if b >= self.scroll + list_h {
                    self.scroll = b + 1 - list_h;
                }
            }
        }
        self.scroll = self.scroll.min(max_scroll);

        screen.blit(&list.canvas, self.scroll, header_h, list_h);
        if footer_h > 0 {
            view::footer(&mut screen, h - footer_h, &ctx, &footer);
        }
        // The hovered node's cut-off text, whole, floating over the list.
        if let Some(tip) = ctx.tip.borrow().as_ref() {
            let y = (header_h + tip.y).checked_sub(self.scroll).filter(|y| (header_h..header_h + list_h).contains(y));
            if let Some(y) = y {
                view::tooltip(&mut screen, &ctx, tip.x.saturating_sub(1), y, &tip.text, header_h + list_h);
            }
        }
        let mut out = String::new();
        screen.encode(&mut out);
        let _ = term::write(&out);

        let offset = header_h as i32 - self.scroll as i32;
        let groups =
            list.groups.iter().map(|Group { wid, y0, y1 }| (wid.clone(), *y0 as i32 + offset, *y1 as i32 + offset));
        self.frame =
            Some(Frame { groups: groups.collect(), list_top: header_h, list_h, list_total: list.canvas.h, screen });
    }
}

/// Swaps two panes of `window`, keeping its active pane (which may be the
/// sidebar) active: swap-pane's effect on it differs between tmux versions.
fn swap_panes(window: &str, a: &str, b: &str) {
    let Ok(active) = tmux::run(["display-message", "-p", "-t", window, "#{pane_id}"]) else { return };
    let _ = Batch::new().cmd(["swap-pane", "-d", "-s", a, "-t", b]).cmd(["select-pane", "-t", active.trim()]).run();
}

fn pollfd(fd: i32) -> libc::pollfd {
    libc::pollfd { fd, events: libc::POLLIN, revents: 0 }
}

fn poll(fds: &mut [libc::pollfd], timeout: Option<Duration>) -> io::Result<()> {
    let ms = timeout.map(|d| d.as_millis().min(i32::MAX as u128) as i32).unwrap_or(-1);
    // SAFETY: poll(2) over a live slice of pollfd structs.
    let r = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) };
    if r < 0 {
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
        for f in fds.iter_mut() {
            f.revents = 0;
        }
    }
    Ok(())
}
