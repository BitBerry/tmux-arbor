//! What a sidebar knows about its session, read from tmux in one invocation.

use anyhow::{Result, bail};

use crate::config::{Config, Side};
use crate::label::Label;
use crate::tmux::{self, SEP};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum NodeKey {
    Window(String),
    Pane(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PaneInfo {
    pub id: String,
    pub active: bool,
    pub last: bool,
    pub cmd: String,
    pub left: u16,
    pub top: u16,
    pub width: u16,
    pub height: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Win {
    pub id: String,
    pub index: u32,
    pub name: String,
    /// The session's current window.
    pub active: bool,
    pub collapsed: bool,
    pub label: Label,
    /// Work panes in index order; sidebars are listed apart.
    pub panes: Vec<PaneInfo>,
    pub sidebars: Vec<String>,
}

impl Win {
    /// Where focus goes when it leaves the sidebar: the active work pane,
    /// else the one used last, else the first.
    pub fn work_target(&self) -> Option<&str> {
        self.panes
            .iter()
            .find(|p| p.active)
            .or_else(|| self.panes.iter().find(|p| p.last))
            .or_else(|| self.panes.first())
            .map(|p| p.id.as_str())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub session_id: String,
    pub session_name: String,
    /// The window this sidebar lives in.
    pub window_id: String,
    pub pane_active: bool,
    pub window_active: bool,
    pub attached: bool,
    pub pane_width: u16,
    pub window_width: u16,
    pub at_left: bool,
    pub at_right: bool,
    pub at_top: bool,
    pub at_bottom: bool,
    pub zoomed: bool,
    pub windows: Vec<Win>,
    pub cfg: Config,
}

fn join(fields: &[&str]) -> String {
    fields.join(&SEP.to_string())
}

/// Labels follow tmux: the current window uses window-status-current-format,
/// the rest window-status-format, unless @arbor-window-format overrides both.
const LABEL: &str = "#{?#{@arbor-window-format},#{E:@arbor-window-format},#{?window_active,#{E:window-status-current-format},#{E:window-status-format}}}";

impl Snapshot {
    pub fn query(pane: &str) -> Result<Snapshot> {
        let s = join(&[
            "S",
            "#{session_id}",
            "#{session_name}",
            "#{window_id}",
            "#{pane_active}",
            "#{window_active}",
            "#{session_attached}",
            "#{pane_width}",
            "#{window_width}",
            "#{pane_at_left}",
            "#{pane_at_right}",
            "#{pane_at_top}",
            "#{pane_at_bottom}",
            "#{window_zoomed_flag}",
        ]);
        let o = format!("O{SEP}{}", Config::query_format());
        let w = join(&[
            "W",
            "#{window_id}",
            "#{window_index}",
            "#{window_active}",
            "#{@arbor-collapsed}",
            "#{window_name}",
            LABEL,
        ]);
        let p = join(&[
            "P",
            "#{window_id}",
            "#{pane_id}",
            "#{pane_active}",
            "#{pane_last}",
            "#{@arbor-sidebar}",
            "#{pane_left}",
            "#{pane_top}",
            "#{pane_width}",
            "#{pane_height}",
            "#{pane_current_command}",
        ]);
        let out = tmux::Batch::new()
            .cmd(["display-message", "-p", "-t", pane, &s])
            .cmd(["display-message", "-p", "-t", pane, &o])
            .cmd(["list-windows", "-t", pane, "-F", &w])
            .cmd(["list-panes", "-s", "-t", pane, "-F", &p])
            .run()?;
        Snapshot::parse(&out)
    }

    pub fn parse(out: &str) -> Result<Snapshot> {
        let mut snap: Option<Snapshot> = None;
        let mut cfg = Config::default();
        let mut windows: Vec<Win> = Vec::new();
        let mut panes: Vec<(String, PaneInfo, bool)> = Vec::new();
        let on = |s: &str| s == "1";
        let num = |s: &str| s.parse::<u16>().unwrap_or(0);
        for line in out.lines() {
            let (tag, rest) = line.split_once(SEP).unwrap_or((line, ""));
            match tag {
                "S" => {
                    let f: Vec<&str> = rest.split(SEP).collect();
                    if f.len() < 13 {
                        bail!("short session line");
                    }
                    snap = Some(Snapshot {
                        session_id: f[0].into(),
                        session_name: f[1].into(),
                        window_id: f[2].into(),
                        pane_active: on(f[3]),
                        window_active: on(f[4]),
                        attached: f[5].parse::<u32>().unwrap_or(0) > 0,
                        pane_width: num(f[6]),
                        window_width: num(f[7]),
                        at_left: on(f[8]),
                        at_right: on(f[9]),
                        at_top: on(f[10]),
                        at_bottom: on(f[11]),
                        zoomed: on(f[12]),
                        windows: Vec::new(),
                        cfg: Config::default(),
                    });
                }
                "O" => cfg = Config::parse(rest),
                "W" => {
                    let f: Vec<&str> = rest.splitn(6, SEP).collect();
                    if f.len() < 6 {
                        continue;
                    }
                    windows.push(Win {
                        id: f[0].into(),
                        index: f[1].parse().unwrap_or(0),
                        active: on(f[2]),
                        collapsed: on(f[3]),
                        name: f[4].into(),
                        label: Label::parse(f[5]),
                        panes: Vec::new(),
                        sidebars: Vec::new(),
                    });
                }
                "P" => {
                    let f: Vec<&str> = rest.splitn(10, SEP).collect();
                    if f.len() < 10 {
                        continue;
                    }
                    let info = PaneInfo {
                        id: f[1].into(),
                        active: on(f[2]),
                        last: on(f[3]),
                        left: num(f[5]),
                        top: num(f[6]),
                        width: num(f[7]),
                        height: num(f[8]),
                        cmd: f[9].into(),
                    };
                    panes.push((f[0].into(), info, on(f[4])));
                }
                _ => {}
            }
        }
        let Some(mut snap) = snap else { bail!("no session line") };
        for (wid, info, sidebar) in panes {
            if let Some(w) = windows.iter_mut().find(|w| w.id == wid) {
                if sidebar { w.sidebars.push(info.id) } else { w.panes.push(info) }
            }
        }
        snap.windows = windows;
        snap.cfg = cfg;
        Ok(snap)
    }

    /// The sidebar holds the keyboard: it is the active pane of the window a
    /// client is looking at.
    pub fn focused(&self) -> bool {
        self.pane_active && self.window_active && self.attached
    }

    pub fn visible(&self) -> bool {
        self.window_active && self.attached
    }

    /// Full height against the configured edge.
    pub fn docked(&self, side: Side) -> bool {
        self.at_top
            && self.at_bottom
            && match side {
                Side::Left => self.at_left,
                Side::Right => self.at_right,
            }
    }

    pub fn window(&self, id: &str) -> Option<&Win> {
        self.windows.iter().find(|w| w.id == id)
    }

    pub fn window_mut(&mut self, id: &str) -> Option<&mut Win> {
        self.windows.iter_mut().find(|w| w.id == id)
    }

    pub fn my_window(&self) -> Option<&Win> {
        self.window(&self.window_id)
    }

    pub fn current(&self) -> Option<&Win> {
        self.windows.iter().find(|w| w.active)
    }

    /// The window holding a pane, and the pane's position among its work panes.
    pub fn find_pane(&self, id: &str) -> Option<(&Win, usize)> {
        self.windows.iter().find_map(|w| w.panes.iter().position(|p| p.id == id).map(|i| (w, i)))
    }

    /// Every pane in this sidebar's window, to tell a user's drag from a
    /// layout change.
    pub fn my_pane_ids(&self) -> Vec<String> {
        self.my_window()
            .map(|w| w.panes.iter().map(|p| p.id.clone()).chain(w.sidebars.iter().cloned()).collect())
            .unwrap_or_default()
    }

    /// Navigable rows in order, honouring collapsed windows.
    pub fn nodes(&self, rail: bool) -> Vec<NodeKey> {
        let mut out = Vec::new();
        for w in &self.windows {
            out.push(NodeKey::Window(w.id.clone()));
            if expanded(w, rail) {
                out.extend(w.panes.iter().map(|p| NodeKey::Pane(p.id.clone())));
            }
        }
        out
    }
}

/// Windows with a single pane never unfold, nor does anything on the rail.
pub fn expanded(w: &Win, rail: bool) -> bool {
    !rail && !w.collapsed && w.panes.len() > 1
}

/// The numeric part of a pane id, for ordering duplicate sidebars.
pub fn pane_number(id: &str) -> u32 {
    id.trim_start_matches('%').parse().unwrap_or(u32::MAX)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A session like the design's: work, with editor (3 panes), server
    /// (2 panes), git and db; the sidebar is %0 in editor.
    pub fn sample() -> Snapshot {
        let s = SEP;
        let mut out = String::new();
        out += &format!("S{s}$1{s}work{s}@1{s}0{s}1{s}1{s}28{s}120{s}1{s}0{s}1{s}1{s}0\n");
        out += &format!("O{s}\n");
        out += &format!("W{s}@1{s}1{s}1{s}{s}editor{s}1:editor*\n");
        out += &format!("W{s}@2{s}2{s}0{s}1{s}server{s}#[fg=red]2:server#\n");
        out += &format!("W{s}@3{s}3{s}0{s}{s}git{s}3:git-\n");
        out += &format!("W{s}@4{s}4{s}0{s}{s}db{s}4:db!\n");
        for (w, p, active, last, sb, left, top, wd, ht, cmd) in [
            ("@1", "%0", 0, 0, "1", 0, 0, 28, 40, "arbor"),
            ("@1", "%1", 1, 0, "", 29, 0, 45, 40, "nvim"),
            ("@1", "%2", 0, 1, "", 75, 0, 45, 20, "zsh"),
            ("@1", "%3", 0, 0, "", 75, 21, 45, 19, "zsh"),
            ("@2", "%4", 1, 0, "", 0, 0, 60, 40, "npm"),
            ("@2", "%5", 0, 0, "", 61, 0, 59, 40, "tail"),
            ("@3", "%6", 1, 0, "", 0, 0, 120, 40, "lazygit"),
            ("@4", "%7", 1, 0, "", 0, 0, 120, 40, "psql"),
        ] {
            out += &format!("P{s}{w}{s}{p}{s}{active}{s}{last}{s}{sb}{s}{left}{s}{top}{s}{wd}{s}{ht}{s}{cmd}\n");
        }
        Snapshot::parse(&out).unwrap()
    }

    #[test]
    fn parses_session() {
        let snap = sample();
        assert_eq!(snap.session_name, "work");
        assert_eq!(snap.windows.len(), 4);
        let editor = snap.my_window().unwrap();
        assert_eq!(editor.panes.len(), 3);
        assert_eq!(editor.sidebars, vec!["%0"]);
        assert_eq!(editor.work_target(), Some("%1"));
        assert!(snap.windows[1].collapsed);
        assert_eq!(snap.windows[1].label.plain(), "2:server#");
        assert!(snap.docked(Side::Left));
        assert!(!snap.focused());
    }

    #[test]
    fn nodes_follow_collapse() {
        let snap = sample();
        let nodes = snap.nodes(false);
        assert_eq!(nodes.len(), 4 + 3);
        assert_eq!(nodes[1], NodeKey::Pane("%1".into()));
        assert_eq!(snap.nodes(true).len(), 4);
    }
}
