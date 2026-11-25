//! The process inside the sidebar pane: draws the 2-level Directory -> Pane
//! tree, reads keys and mouse, and turns them into tmux commands. It redraws
//! when a hook signals it (SIGUSR1), re-detects the theme on SIGUSR2, and
//! handles its own resizes on SIGWINCH.

use std::cell::{Cell, RefCell};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::cli;
use crate::config::{Config, HIDE_BELOW, MAX_W, MIN_FULL, Mode, Side, Theme};
use crate::input::{Event, Key, Mouse, MouseKind, Parser};
use crate::label::parse_color;
use crate::model::{self, COLLAPSED_SEP, DirGroup, NodeKey, Snapshot, pane_number, shorten_home};
use crate::screen::{Canvas, truncate, width};
use crate::split;
use crate::strings::{Strings, wrap};
use crate::term;
use crate::theme::{self, Palette};
use crate::tip;
use crate::tmux::{self, Batch};
use crate::view::{self, Ctx, Drag, Footer, Group, Hit, Rename, Target};

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
/// How long a popup stays up after the pointer last moved over its name.
const POPUP_IDLE: Duration = Duration::from_secs(5);
/// Frame interval while a cut-off name scrolls (it moves every 70ms).
const MARQUEE_FRAME: Duration = Duration::from_millis(35);
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
    /// Directory groups in screen rows (may run off either end).
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
    hover_add: Option<String>,
    hover_rename: Option<String>,
    hover_split: Option<char>,
    hover_until: Option<Instant>,
    last_motion: Option<Instant>,
    /// The last motion was on the edge facing the work area.
    hover_edge: bool,
    /// The node whose cut-off text scrolls, and since when.
    marquee: Option<(NodeKey, Instant)>,
    /// tmux can float panes: cut-off names get a popup, not a marquee.
    floating: bool,
    /// The open popup's pane, and what it shows.
    popup: Option<(String, tip::Popup)>,
    /// The rows written last frame; only rows that differ are written, to
    /// keep a sidebar over a slow ssh link from resending the whole tree.
    last_rows: Vec<String>,
    anim_at: Option<Instant>,
    /// A node to scroll into view on the next frame.
    reveal: Option<NodeKey>,
    scroll: u16,
    press: Option<Press>,
    drag: Option<Drag>,
    /// Inline pane rename: `(editor, return_focus_on_end, started_at)`.
    rename: Option<(Rename, bool, Instant)>,
    last_click: Option<(String, Instant)>,
    cursor_shown: bool,
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
    app.floating = tip::supported();
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
            hover_add: None,
            hover_rename: None,
            hover_split: None,
            hover_until: None,
            last_motion: None,
            hover_edge: false,
            marquee: None,
            floating: false,
            popup: None,
            last_rows: Vec::new(),
            anim_at: None,
            reveal: None,
            scroll: 0,
            press: None,
            drag: None,
            rename: None,
            last_click: None,
            cursor_shown: false,
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
                self.anim_at,
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
                // tmux reflows the pane on resize: write every row again.
                self.last_rows.clear();
                self.resize_at = Some(Instant::now() + RESIZE_SETTLE);
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
        self.sync_popup(None);
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
        if due(self.anim_at) {
            self.anim_at = None;
            self.draw();
        }
        if due(self.hover_until) {
            self.hover_until = None;
            self.hover = None;
            self.hover_add = None;
            self.hover_rename = None;
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
        let rail = snap.cfg.mode == Mode::Rail || self.size.0 < MIN_FULL;
        let nodes = snap.nodes(rail);
        let focused = snap.focused();
        if focused && !self.was_focused {
            self.cursor = snap.focused_pane().map(NodeKey::Pane).or_else(|| nodes.first().cloned());
            self.reveal = self.cursor.clone();
        }
        self.was_focused = focused;
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
        let current = snap.focused_pane();
        if current != self.last_current {
            self.reveal = self.reveal.take().or_else(|| current.clone().map(NodeKey::Pane));
            self.last_current = current;
            self.hover = None;
            self.hover_add = None;
            self.hover_rename = None;
        }
        if let Some((r, ..)) = &self.rename
            && (rail || snap.find_pane(&r.pid).is_none())
        {
            self.rename = None;
        }
        if let Some(c) = &self.cursor
            && !nodes.contains(c)
        {
            self.cursor = match c {
                NodeKey::Pane(p) => snap.pane_group_dir(p).map(NodeKey::Dir).filter(|d| nodes.contains(d)),
                NodeKey::Dir(_) => None,
            }
            .or_else(|| snap.focused_pane().map(NodeKey::Pane).filter(|k| nodes.contains(k)))
            .or_else(|| nodes.first().cloned());
        }
        self.tick_at =
            (snap.cfg.interval > 0 && snap.visible()).then(|| Instant::now() + Duration::from_secs(snap.cfg.interval));
        self.snap = Some(snap);
        self.update_palette();
    }

    fn refresh(&mut self) {
        self.refresh_at = None;
        let Some(snap) = self.query() else { return };
        let focused = self.focused();
        let changed = self.snap.as_ref() != Some(&snap);
        self.apply(snap);
        if self.quit {
            return;
        }
        self.fix_geometry();
        if changed || self.focused() != focused {
            self.draw();
        }
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
        if docked && snap.dragging {
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
                if self.rename.is_some() {
                    self.rename_key(k);
                } else {
                    self.key(k);
                }
            }
            Event::Mouse(m) => self.mouse(m),
            Event::FocusIn | Event::FocusOut => {
                if ev == Event::FocusOut {
                    self.hover = None;
                    self.hover_add = None;
                    self.hover_rename = None;
                    if let Some((_, _, started_at)) = self.rename
                        && started_at.elapsed() > Duration::from_millis(150)
                    {
                        self.finish_rename(false);
                    }
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

    fn start_rename(&mut self, pid: &str, return_focus: bool) {
        if self.rail() {
            return;
        }
        let Some(initial) = self.snap.as_ref().and_then(|s| s.find_pane(pid)).map(|(w, i)| w.panes[i].cmd.clone())
        else {
            return;
        };
        if return_focus {
            let _ = tmux::run(["select-pane", "-t", &self.pane]);
        }
        self.rename = Some((Rename::new(pid.to_string(), initial), return_focus, Instant::now()));
        self.cursor = Some(NodeKey::Pane(pid.to_string()));
        self.reveal = self.cursor.clone();
        self.hover = None;
        self.hover_add = None;
        self.hover_rename = None;
        self.hover_split = None;
        self.drag = None;
        self.press = None;
        self.draw();
    }

    fn finish_rename(&mut self, commit: bool) {
        let Some((r, return_focus, _)) = self.rename.take() else { return };
        if commit {
            let val = r.sanitized();
            let res = if val.is_empty() {
                tmux::run(["set-option", "-pu", "-t", &r.pid, "@arbor-name"])
            } else {
                tmux::run(["set-option", "-p", "-t", &r.pid, "@arbor-name", &val])
            };
            if res.is_ok() {
                if let Some(p) = self.snap.as_mut().and_then(|s| s.pane_mut(&r.pid)) {
                    if val.is_empty() {
                        p.custom_name = None;
                        p.cmd = p.auto_cmd.clone();
                    } else {
                        p.custom_name = Some(val.clone());
                        p.cmd = val.clone();
                    }
                }
                let text = self.strings();
                self.say(if val.is_empty() { text.name_reset() } else { text.renamed_to(&val) });
                let _ = cli::notify(libc::SIGUSR1);
            }
        }
        if return_focus {
            self.leave();
        }
        self.draw();
    }

    fn rename_key(&mut self, k: Key) {
        match k {
            Key::Enter => {
                self.finish_rename(true);
                return;
            }
            Key::Esc | Key::Ctrl('c') => {
                self.finish_rename(false);
                return;
            }
            _ => {}
        }
        if let Some((r, ..)) = &mut self.rename {
            match k {
                Key::Char(ch) => r.insert(ch),
                Key::Backspace => r.backspace(),
                Key::Delete => r.delete(),
                Key::Left => r.move_left(),
                Key::Right => r.move_right(),
                Key::Home | Key::Ctrl('a') => r.home(),
                Key::End | Key::Ctrl('e') => r.end(),
                Key::Ctrl('u') => r.clear(),
                Key::Ctrl('k') => r.kill_to_end(),
                Key::Ctrl('w') => r.delete_word(),
                _ => return,
            }
        }
        self.draw();
    }

    fn key(&mut self, k: Key) {
        let Some(snap) = self.snap.clone() else { return };
        let rail = self.rail();
        let groups = snap.groups();
        let nodes = snap.nodes(rail);
        if nodes.is_empty() {
            return;
        }
        let current = snap.focused_pane().map(NodeKey::Pane);
        let i = self.cursor.as_ref().or(current.as_ref()).and_then(|c| nodes.iter().position(|n| n == c)).unwrap_or(0);
        let node = nodes[i].clone();
        let text = self.strings();
        let mut cfg = snap.cfg.clone();

        match k {
            Key::Char('j') | Key::Down => self.move_cursor(nodes.get(i + 1)),
            Key::Char('k') | Key::Up => self.move_cursor(i.checked_sub(1).and_then(|j| nodes.get(j))),
            Key::Char('l') | Key::Right => {
                if let NodeKey::Dir(dir) = &node {
                    if snap.collapsed_dirs.contains(dir) {
                        self.set_dir_collapsed(dir, false);
                    } else {
                        self.move_cursor(nodes.get(i + 1));
                    }
                }
            }
            Key::Char('h') | Key::Left => match &node {
                NodeKey::Dir(dir) if !snap.collapsed_dirs.contains(dir) => self.set_dir_collapsed(dir, true),
                NodeKey::Pane(pid) => {
                    if let Some(dir) = snap.pane_group_dir(pid) {
                        self.move_cursor(Some(&NodeKey::Dir(dir)));
                    }
                }
                _ => {}
            },
            Key::Char(' ') => {
                if let NodeKey::Dir(dir) = &node {
                    self.set_dir_collapsed(dir, !snap.collapsed_dirs.contains(dir));
                }
            }
            Key::Enter => match &node {
                NodeKey::Dir(dir) => self.set_dir_collapsed(dir, !snap.collapsed_dirs.contains(dir)),
                NodeKey::Pane(pid) => {
                    if let Some((w, _)) = snap.find_pane(pid) {
                        self.goto_pane(&w.id.clone(), pid);
                    }
                }
            },
            Key::Char('c') => {
                let dir = match &node {
                    NodeKey::Dir(d) => Some(d.clone()),
                    NodeKey::Pane(p) => snap.pane_group_dir(p),
                };
                if let Some(dir) = dir {
                    self.create_window_in_dir(&dir);
                }
            }
            Key::Char('1'..='9') => {
                let Key::Char(d) = k else { unreachable!() };
                let n = d as usize - '1' as usize;
                if let Some(NodeKey::Pane(pid)) = nodes.iter().filter(|k| matches!(k, NodeKey::Pane(_))).nth(n)
                    && let Some((w, _)) = snap.find_pane(pid)
                {
                    self.cursor = Some(NodeKey::Pane(pid.clone()));
                    self.goto_pane(&w.id.clone(), pid);
                }
            }
            Key::Char('R') | Key::F2 => {
                if let NodeKey::Pane(pid) = &node {
                    self.start_rename(pid, false);
                }
                return;
            }
            Key::Char('J') => self.shift(&node, &groups, 1),
            Key::Char('K') => self.shift(&node, &groups, -1),
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

    fn set_dir_collapsed(&mut self, raw_path: &str, collapsed: bool) {
        let Some(snap) = &mut self.snap else { return };
        let sid = snap.session_id.clone();
        if collapsed {
            snap.collapsed_dirs.insert(raw_path.to_string());
        } else {
            snap.collapsed_dirs.remove(raw_path);
        }
        let val: String = snap.collapsed_dirs.iter().cloned().collect::<Vec<_>>().join(&COLLAPSED_SEP.to_string());
        let r = if val.is_empty() {
            tmux::run(["set-option", "-su", "-t", &sid, "@arbor-collapsed-dirs"])
        } else {
            tmux::run(["set-option", "-s", "-t", &sid, "@arbor-collapsed-dirs", &val])
        };
        if r.is_ok() {
            let _ = cli::notify(libc::SIGUSR1);
        }
    }

    fn create_window_in_dir(&mut self, raw_path: &str) {
        let Some(snap) = self.snap.clone() else { return };
        if snap.collapsed_dirs.contains(raw_path) {
            self.set_dir_collapsed(raw_path, false);
        }
        let label = snap
            .groups()
            .into_iter()
            .find(|g| g.raw_path == raw_path)
            .map(|g| g.label)
            .unwrap_or_else(|| shorten_home(raw_path, model::home_dir()));
        let text = self.strings();
        if tmux::run(["new-window", "-t", &snap.session_id, "-c", raw_path]).is_ok() {
            self.say(text.new_window_in(&label));
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

    /// J / K: moves a directory group or pane one place down or up.
    fn shift(&mut self, node: &NodeKey, groups: &[DirGroup], d: i32) {
        let Some(snap) = self.snap.clone() else { return };
        let text = self.strings();
        match node {
            NodeKey::Dir(dir) => {
                let Some(i) = groups.iter().position(|g| &g.raw_path == dir) else { return };
                let Some(other) = (i as i32 + d).try_into().ok().and_then(|j: usize| groups.get(j)) else {
                    return;
                };
                self.reorder_dir(&snap, groups, dir, &other.raw_path, d > 0);
            }
            NodeKey::Pane(pid) => {
                let Some(dir) = snap.pane_group_dir(pid) else { return };
                let Some(g) = groups.iter().find(|g| g.raw_path == dir) else { return };
                let Some(i) = g.items.iter().position(|it| &it.pane.id == pid) else { return };
                let Some(other) = (i as i32 + d).try_into().ok().and_then(|j: usize| g.items.get(j)) else {
                    self.say(text.at_dir_edge());
                    return;
                };
                let cur = &g.items[i];
                if cur.wid == other.wid {
                    swap_panes(&cur.wid, pid, &other.pane.id);
                } else {
                    move_window(&snap.session_id, &cur.wid, &other.wid, d > 0);
                }
            }
        }
    }

    fn reorder_dir(&self, snap: &Snapshot, groups: &[DirGroup], src_dir: &str, dst_dir: &str, after: bool) {
        let Some(src_g) = groups.iter().find(|g| g.raw_path == src_dir) else { return };
        let Some(dst_g) = groups.iter().find(|g| g.raw_path == dst_dir) else { return };
        let anchor = if after { dst_g.items.last() } else { dst_g.items.first() }.map(|it| it.wid.clone());
        let Some(anchor) = anchor else { return };

        let mut src_wids: Vec<String> = Vec::new();
        for it in &src_g.items {
            if it.wid != anchor && !src_wids.contains(&it.wid) {
                src_wids.push(it.wid.clone());
            }
        }
        if src_wids.is_empty() {
            return;
        }
        let Ok(current) = tmux::run(["display-message", "-p", "-t", &snap.session_id, "#{window_id}"]) else {
            return;
        };
        let mut b = Batch::new();
        if after {
            let mut prev = anchor;
            for w in &src_wids {
                b.cmd([
                    "move-window",
                    "-d",
                    "-a",
                    "-s",
                    &format!("{}:{w}", snap.session_id),
                    "-t",
                    &format!("{}:{prev}", snap.session_id),
                ]);
                prev = w.clone();
            }
        } else {
            for w in &src_wids {
                b.cmd([
                    "move-window",
                    "-d",
                    "-b",
                    "-s",
                    &format!("{}:{w}", snap.session_id),
                    "-t",
                    &format!("{}:{anchor}", snap.session_id),
                ]);
            }
        }
        b.cmd(["select-window", "-t", current.trim()]);
        let _ = b.run();
    }

    // ---- mouse -----------------------------------------------------------

    fn hit_at(&self, x: u16, y: u16) -> Hit {
        self.frame.as_ref().and_then(|f| f.screen.hit(x, y).cloned()).unwrap_or_default()
    }

    fn mouse(&mut self, m: Mouse) {
        let hit = self.hit_at(m.x, m.y);
        if let Some((r, ..)) = &self.rename {
            if m.kind == MouseKind::Press(0) {
                let on_editing = matches!(&hit, Hit::Pane { pid, .. } | Hit::PaneRename { pid, .. } if pid == &r.pid);
                if !on_editing {
                    self.finish_rename(false);
                }
            }
            return;
        }
        match m.kind {
            MouseKind::Press(0) => {
                self.press = Some(Press { x: m.x, y: m.y, hit });
            }
            MouseKind::Drag(0) => {
                let Some(p) = &self.press else { return };
                if self.drag.is_none() {
                    if m.y == p.y && m.x.abs_diff(p.x) < 2 {
                        return;
                    }
                    let src = match &p.hit {
                        Hit::Dir(d) => (NodeKey::Dir(d.clone()), String::new(), d.clone(), 0),
                        Hit::Pane { wid, pid, dir } => {
                            let win_panes = self.snap.as_ref().and_then(|s| s.window(wid)).map_or(1, |w| w.panes.len());
                            (NodeKey::Pane(pid.clone()), wid.clone(), dir.clone(), win_panes)
                        }
                        _ => return,
                    };
                    self.drag =
                        Some(Drag { src: src.0, src_wid: src.1, src_dir: src.2, src_win_panes: src.3, target: None });
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
                let hover_add = match &hit {
                    Hit::DirAdd(d) => Some(d.clone()),
                    _ => None,
                };
                let hover_rename = match &hit {
                    Hit::PaneRename { pid, .. } => Some(pid.clone()),
                    _ => None,
                };
                let hover_split = match hit {
                    Hit::Split(d) => Some(d),
                    _ => None,
                };
                let edge = match self.cfg().position {
                    Side::Left => m.x + 1 >= self.size.0,
                    Side::Right => m.x == 0,
                };
                let now = Instant::now();
                self.hover_until = Some(now + if edge { HOVER_EDGE } else { HOVER_IDLE });
                self.last_motion = Some(now);
                self.hover_edge = edge;
                if hover != self.hover
                    || hover_add != self.hover_add
                    || hover_rename != self.hover_rename
                    || hover_split != self.hover_split
                {
                    self.hover = hover;
                    self.hover_add = hover_add;
                    self.hover_rename = hover_rename;
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
                self.hover = None;
                self.hover_add = None;
                self.hover_rename = None;
                self.hover_split = None;
                self.draw();
            }
            _ => {}
        }
    }

    fn click(&mut self, hit: &Hit) {
        match hit {
            Hit::DirAdd(dir) => self.create_window_in_dir(dir),
            Hit::Dir(dir) => {
                let collapsed = self.snap.as_ref().is_some_and(|s| s.collapsed_dirs.contains(dir));
                self.cursor = Some(NodeKey::Dir(dir.clone()));
                self.set_dir_collapsed(dir, !collapsed);
                if !self.was_focused {
                    let _ = cli::focus_work(&self.pane);
                }
                self.draw();
            }
            Hit::PaneRename { pid, .. } => self.start_rename(pid, !self.was_focused),
            Hit::Pane { wid, pid, .. } => {
                let dbl =
                    self.last_click.as_ref().is_some_and(|(p, t)| p == pid && t.elapsed() < Duration::from_millis(400));
                if dbl && !self.rail() && self.snap.as_ref().is_some_and(|s| s.window_id == *wid) {
                    self.last_click = None;
                    self.start_rename(pid, !self.was_focused);
                } else {
                    self.last_click = Some((pid.clone(), Instant::now()));
                    self.cursor = Some(NodeKey::Pane(pid.clone()));
                    self.goto_pane(&wid.clone(), &pid.clone());
                }
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
            NodeKey::Dir(src) => {
                if y < frame.list_top || y >= frame.list_top + frame.list_h {
                    return None;
                }
                let yy = y as i32;
                let g = frame
                    .groups
                    .iter()
                    .position(|g| yy >= g.1 && yy <= g.2)
                    .or_else(|| frame.groups.last().filter(|g| yy > g.2).map(|_| frame.groups.len() - 1))?;
                let (dir, y0, y1) = &frame.groups[g];
                if dir == src {
                    return None;
                }
                let groups = snap.groups();
                let src_i = groups.iter().position(|gr| &gr.raw_path == src)?;
                let before = if y1 > y0 { (yy - y0) * 2 < y1 - y0 + 1 } else { g < src_i };
                let before = before && yy <= *y1;
                Some(if before { Target::DirBefore(dir.clone()) } else { Target::DirAfter(dir.clone()) })
            }
            NodeKey::Pane(src) => match frame.screen.hit(x, y)? {
                Hit::NewWindow if drag.src_win_panes > 1 => Some(Target::Break),
                Hit::Pane { wid, pid, .. } => {
                    if pid == src {
                        None
                    } else if *wid == drag.src_wid {
                        Some(Target::Swap(pid.clone()))
                    } else {
                        Some(Target::Join { wid: wid.clone(), pid: pid.clone() })
                    }
                }
                Hit::Dir(d) | Hit::DirAdd(d) if *d != drag.src_dir => Some(Target::Invalid),
                _ => None,
            },
        }
    }

    fn drop(&mut self, d: &Drag, t: Target) {
        let Some(snap) = self.snap.clone() else { return };
        match (&d.src, &t) {
            (NodeKey::Dir(src), Target::DirBefore(dst) | Target::DirAfter(dst)) => {
                let groups = snap.groups();
                self.reorder_dir(&snap, &groups, src, dst, matches!(t, Target::DirAfter(_)));
            }
            (NodeKey::Pane(src), Target::Swap(dst)) => swap_panes(&d.src_wid, src, dst),
            (NodeKey::Pane(src), Target::Join { pid, .. }) => {
                let _ = tmux::run(["join-pane", "-s", src, "-t", pid]);
            }
            (NodeKey::Pane(src), Target::Break) => {
                let _ = tmux::run(["break-pane", "-s", src]);
            }
            (_, Target::Invalid) => {
                let text = self.strings();
                self.say(text.dir_cwd_hint());
                let _ = cli::focus_work(&self.pane);
                return;
            }
            _ => return,
        }
        self.cursor = Some(d.src.clone());
        let _ = cli::focus_work(&self.pane);
    }

    // ---- drawing ---------------------------------------------------------

    fn footer(&self, ctx: &Ctx, text: &Strings, focused: bool) -> Footer {
        if self.rename.is_some() {
            let mut lines = wrap(&text.hints_rename(), ctx.width.saturating_sub(2) as usize);
            lines.truncate(3);
            return Footer { lines, accent: true, ..Footer::default() };
        }
        if let Some(d) = &self.drag {
            let hint = match &d.target {
                Some(Target::Invalid) => (text.dir_cwd_hint(), false),
                Some(t) => (self.target_hint(t, text), true),
                None => (text.cannot_drop(), false),
            };
            let zone = (matches!(d.src, NodeKey::Pane(_)) && d.src_win_panes > 1)
                .then(|| (text.new_window_zone(), d.target == Some(Target::Break)));
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
        let groups = snap.map(Snapshot::groups).unwrap_or_default();
        let dname = |raw: &str| {
            groups.iter().find(|g| g.raw_path == raw).map(|g| g.label.clone()).unwrap_or_else(|| raw.to_string())
        };
        let pinfo = |pid: &str| {
            snap.and_then(|s| s.find_pane(pid)).map(|(w, i)| (w.panes[i].cmd.clone(), w.index)).unwrap_or_default()
        };
        match t {
            Target::DirBefore(d) => text.before(&dname(d)),
            Target::DirAfter(d) => text.after(&dname(d)),
            Target::Join { pid, .. } => {
                let (cmd, widx) = pinfo(pid);
                text.join_pane(&cmd, widx)
            }
            Target::Swap(p) => {
                let (cmd, _) = pinfo(p);
                text.swap(&cmd)
            }
            Target::Break => text.break_out(),
            Target::Invalid => text.dir_cwd_hint(),
        }
    }

    /// What scrolls: the hovered node, else the keyboard cursor; nothing
    /// while a button is held. A new target starts from the beginning.
    fn pick_marquee(&mut self, focused: bool) {
        let target = if self.press.is_some() || self.drag.is_some() {
            None
        } else {
            self.hover.clone().or_else(|| if focused { self.cursor.clone() } else { None })
        };
        if self.marquee.as_ref().map(|m| &m.0) != target.as_ref() {
            self.marquee = target.map(|k| (k, Instant::now()));
        }
    }

    /// Where the popup for cut-off text on list row `tip.y` goes: the same
    /// row, just outside the sidebar, opening towards the work area.
    fn place_popup(&self, snap: &Snapshot, tip: &view::Tip, header_h: u16, list_h: u16) -> Option<tip::Popup> {
        let row = (header_h + tip.y).checked_sub(self.scroll)?;
        if row < header_h || row >= header_h + list_h {
            return None;
        }
        let full = width(&tip.text) + 2;
        let room = match snap.cfg.position {
            Side::Left => snap.window_width.saturating_sub(snap.pane_left + snap.pane_width + 1),
            Side::Right => snap.pane_left.saturating_sub(1),
        } as usize;
        if room < 6 {
            return None;
        }
        let text = truncate(&tip.text, room.min(full) - 2);
        let x = match snap.cfg.position {
            Side::Left => snap.pane_left + snap.pane_width + 1,
            Side::Right => (room - width(&text) - 2) as u16,
        };
        Some(tip::Popup { x, y: snap.pane_top + row, text, fg: self.pal.chip_fg, bg: self.pal.chip_bg })
    }

    /// Opens, moves or closes the popup to match `want`.
    fn sync_popup(&mut self, want: Option<tip::Popup>) {
        if self.popup.as_ref().map(|p| &p.1) == want.as_ref() {
            return;
        }
        let bin = self.cfg().bin_path();
        match (self.popup.take(), want) {
            (Some((pane, _)), None) => tip::close(&pane),
            (Some((pane, _)), Some(p)) if tip::update(&pane, &bin, &p).is_ok() => self.popup = Some((pane, p)),
            (old, Some(p)) => {
                if let Some((pane, _)) = old {
                    tip::close(&pane);
                }
                if let Ok(id) = tip::open(&self.pane, &bin, &p) {
                    self.popup = Some((id, p));
                }
            }
            (None, None) => {}
        }
    }

    fn draw(&mut self) {
        let focused = self.focused();
        self.pick_marquee(focused);
        let Some(snap) = &self.snap else { return };
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            return;
        }
        let rail = self.rail();
        let editing = self.rename.is_some();
        let text = Strings::new(snap.cfg.lang);
        let groups = snap.groups();
        let ctx = Ctx {
            snap,
            groups: &groups,
            pal: &self.pal,
            text: &text,
            home: model::home_dir(),
            width: w,
            rail,
            variant: snap.cfg.style,
            label_styles: snap.cfg.label_styles,
            cursor: if focused && !editing { self.cursor.as_ref() } else { None },
            hover: if editing { None } else { self.hover.as_ref() },
            hover_add: if editing { None } else { self.hover_add.as_deref() },
            hover_rename: if editing { None } else { self.hover_rename.as_deref() },
            hover_split: if editing { None } else { self.hover_split },
            drag: if editing { None } else { self.drag.as_ref() },
            rename: self.rename.as_ref().map(|(r, ..)| r),
            edit_cursor: Cell::new(None),
            marquee: if self.floating || editing { None } else { self.marquee.as_ref().map(|(k, t)| (k, t.elapsed())) },
            overflow: Cell::new(None),
            tip_for: if self.floating && !editing { self.marquee.as_ref().map(|(k, _)| k) } else { None },
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
                NodeKey::Dir(dir) => list.groups.iter().find(|g| &g.dir == dir).map(|g| (g.y0, g.y1)),
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
        let rows = screen.encode_rows();
        let changed: Vec<&String> =
            rows.iter().enumerate().filter(|(i, r)| self.last_rows.get(*i) != Some(*r)).map(|(_, r)| r).collect();
        let edit_pos = ctx.edit_cursor.get().and_then(|(ex, ey)| {
            let sy = (header_h + ey).checked_sub(self.scroll)?;
            (sy >= header_h && sy < header_h + list_h && ex < w).then_some((ex, sy))
        });
        let mut out = String::new();
        if !changed.is_empty() {
            out.push_str("\x1b[?2026h");
            out.extend(changed.into_iter().map(String::as_str));
            out.push_str("\x1b[0m\x1b[?2026l");
        }
        if let Some((cx, cy)) = edit_pos {
            use std::fmt::Write;
            let _ = write!(out, "\x1b[{};{}H\x1b[?25h", cy + 1, cx + 1);
            self.cursor_shown = true;
        } else if self.cursor_shown {
            out.push_str("\x1b[?25l");
            self.cursor_shown = false;
        }
        if !out.is_empty() {
            let _ = term::write(&out);
        }
        self.last_rows = rows;

        let overflow = ctx.overflow.get();
        let popup = ctx.tip.borrow().as_ref().and_then(|t| self.place_popup(snap, t, header_h, list_h));
        self.anim_at = overflow.map(|_| Instant::now() + MARQUEE_FRAME);
        let linger = match (overflow, &popup) {
            (Some(dist), _) => Some(view::marquee_cycle(dist).max(HOVER_IDLE)),
            (None, Some(_)) => Some(POPUP_IDLE),
            (None, None) => None,
        };
        if let (Some(linger), Some(last)) = (linger, self.last_motion)
            && !self.hover_edge
            && self.hover.is_some()
            && self.marquee.as_ref().map(|m| &m.0) == self.hover.as_ref()
        {
            let until = last + linger;
            if self.hover_until.is_some_and(|h| h < until) {
                self.hover_until = Some(until);
            }
        }
        self.sync_popup(popup);

        let offset = header_h as i32 - self.scroll as i32;
        let groups =
            list.groups.iter().map(|Group { dir, y0, y1 }| (dir.clone(), *y0 as i32 + offset, *y1 as i32 + offset));
        self.frame =
            Some(Frame { groups: groups.collect(), list_top: header_h, list_h, list_total: list.canvas.h, screen });
    }
}

/// Moves a window before/after another window, keeping the current window active.
fn move_window(sid: &str, src_wid: &str, dst_wid: &str, after: bool) {
    let Ok(current) = tmux::run(["display-message", "-p", "-t", sid, "#{window_id}"]) else { return };
    let _ = Batch::new()
        .cmd([
            "move-window",
            "-d",
            if after { "-a" } else { "-b" },
            "-s",
            &format!("{sid}:{src_wid}"),
            "-t",
            &format!("{sid}:{dst_wid}"),
        ])
        .cmd(["select-window", "-t", current.trim()])
        .run();
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
