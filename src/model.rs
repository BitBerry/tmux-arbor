//! What a sidebar knows about its session, read from tmux in one invocation
//! and grouped into a 2-level Directory -> Pane tree.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Result, bail};

use crate::config::{Config, DirDisplay, DirSort, GroupBy};
use crate::label::Label;
use crate::tmux::{self, SEP};

/// Separator between collapsed directory paths in `@arbor-collapsed-dirs`.
pub const COLLAPSED_SEP: char = '␞';

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum NodeKey {
    Dir(String),
    Pane(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PaneInfo {
    pub id: String,
    pub active: bool,
    pub last: bool,
    /// Display name: @arbor-name if set, else the automatic pane label.
    pub cmd: String,
    /// Plain text of the automatic pane label (@arbor-pane-format or running command).
    pub auto_cmd: String,
    pub custom_name: Option<String>,
    pub label: Label,
    pub cwd: String,
    pub git_root: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Win {
    pub id: String,
    pub index: u32,
    pub name: String,
    pub flags: String,
    /// The session's current window.
    pub active: bool,
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
pub struct PaneItem {
    pub wid: String,
    pub win_index: u32,
    pub win_name: String,
    pub win_flags: String,
    pub win_active: bool,
    pub pane_idx: usize,
    pub win_pane_count: usize,
    pub pane: PaneInfo,
}

impl PaneItem {
    pub fn is_focused(&self) -> bool {
        self.win_active && self.pane.active
    }

    pub fn is_win_sibling(&self) -> bool {
        self.win_active && !self.pane.active
    }

    pub fn badge(&self) -> String {
        if self.is_focused() {
            format!("{}*", self.win_index)
        } else if self.win_flags.contains('#') {
            format!("{}#", self.win_index)
        } else if self.win_flags.contains('!') {
            format!("{}!", self.win_index)
        } else {
            self.win_index.to_string()
        }
    }

    pub fn rail_label(&self) -> String {
        if self.win_pane_count <= 1 {
            self.win_index.to_string()
        } else {
            format!("{}.{}", self.win_index, self.pane_idx + 1)
        }
    }

    pub fn rel_subdir(&self, group_raw: &str, home: Option<&str>) -> Option<String> {
        if self.pane.cwd == group_raw {
            return None;
        }
        if let Some(rest) = self.pane.cwd.strip_prefix(group_raw)
            && let Some(sub) = rest.strip_prefix('/')
            && !sub.is_empty()
        {
            return Some(sub.to_string());
        }
        Some(shorten_home(&self.pane.cwd, home))
    }

    pub fn tooltip(&self, home: Option<&str>) -> String {
        let disp = shorten_home(&self.pane.cwd, home);
        let name = match &self.pane.custom_name {
            Some(custom) if custom != &self.pane.auto_cmd => format!("{custom} ({})", self.pane.auto_cmd),
            _ => self.pane.cmd.clone(),
        };
        if self.win_pane_count > 1 {
            format!("{name} · w{} ({}/{}) · {disp}", self.win_index, self.pane_idx + 1, self.win_pane_count)
        } else {
            format!("{name} · w{} · {disp}", self.win_index)
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DirGroup {
    pub raw_path: String,
    pub display_path: String,
    pub label: String,
    pub collapsed: bool,
    pub min_win_index: u32,
    pub has_focused: bool,
    pub has_activity: bool,
    pub has_bell: bool,
    pub items: Vec<PaneItem>,
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
    /// Where the sidebar sits in its window.
    pub pane_left: u16,
    pub pane_top: u16,
    /// A divider in this window is being dragged with the mouse.
    pub dragging: bool,
    pub collapsed_dirs: BTreeSet<String>,
    pub windows: Vec<Win>,
    pub cfg: Config,
}

fn join(fields: &[&str]) -> String {
    fields.join(&SEP.to_string())
}

/// Pane rows show the running command unless @arbor-pane-format says
/// otherwise (say, a Claude session's name from the pane title).
const PANE_LABEL: &str = "#{?#{@arbor-pane-format},#{E:@arbor-pane-format},#{pane_current_command}}";

pub fn home_dir() -> Option<&'static str> {
    static HOME: OnceLock<Option<String>> = OnceLock::new();
    HOME.get_or_init(|| {
        std::env::var("HOME").ok().and_then(|h| {
            let trimmed = h.trim_end_matches('/').to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        })
    })
    .as_deref()
}

pub fn normalize_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return "~".into();
    }
    if trimmed == "/" {
        return "/".into();
    }
    trimmed.trim_end_matches('/').to_string()
}

pub fn shorten_home(path: &str, home: Option<&str>) -> String {
    if path == "~" || path.starts_with("~/") {
        return path.to_string();
    }
    if let Some(h) = home.filter(|h| !h.is_empty() && *h != "/") {
        if path == h {
            return "~".into();
        }
        if let Some(rest) = path.strip_prefix(h)
            && rest.starts_with('/')
        {
            return format!("~{rest}");
        }
    }
    path.to_string()
}

pub fn find_git_root(path: &str) -> Option<String> {
    for anc in Path::new(path).ancestors() {
        if anc.join(".git").exists() {
            return Some(normalize_path(&anc.to_string_lossy()));
        }
    }
    None
}

fn abbrev_segment(seg: &str) -> String {
    if seg.starts_with('.') { seg.chars().take(2).collect() } else { seg.chars().take(1).collect() }
}

fn format_short_dir(disp: &str) -> String {
    if disp == "/" {
        return "/".into();
    }
    if disp == "~" {
        return "~/".into();
    }
    let segs: Vec<&str> = disp.split('/').filter(|s| !s.is_empty()).collect();
    if segs.len() <= 2 {
        return format!("{disp}/");
    }
    let last = segs.len() - 1;
    let out: Vec<String> = segs
        .iter()
        .enumerate()
        .map(|(i, s)| if i == 0 || i == last { (*s).to_string() } else { abbrev_segment(s) })
        .collect();
    let prefix = if disp.starts_with('/') { "/" } else { "" };
    format!("{prefix}{}/", out.join("/"))
}

pub fn compute_dir_labels(paths: &[String], home: Option<&str>, mode: DirDisplay) -> HashMap<String, String> {
    let mut labels = HashMap::with_capacity(paths.len());
    if paths.is_empty() {
        return labels;
    }
    let disps: Vec<String> = paths.iter().map(|p| shorten_home(p, home)).collect();
    if mode == DirDisplay::Short {
        for (raw, disp) in paths.iter().zip(disps.iter()) {
            labels.insert(raw.clone(), format_short_dir(disp));
        }
        return labels;
    }

    let segs_list: Vec<Vec<&str>> = disps.iter().map(|d| d.split('/').filter(|s| !s.is_empty()).collect()).collect();
    let mut depths: Vec<usize> = vec![1; paths.len()];

    let tail = |i: usize, d: usize| -> String {
        let disp = &disps[i];
        if disp == "/" {
            return "/".into();
        }
        if disp == "~" {
            return "~".into();
        }
        let segs = &segs_list[i];
        let keep = d.min(segs.len());
        segs[segs.len() - keep..].join("/")
    };

    for _ in 0..16 {
        let mut buckets: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, &d) in depths.iter().enumerate() {
            buckets.entry(tail(i, d)).or_default().push(i);
        }
        let mut changed = false;
        for idxs in buckets.values() {
            if idxs.len() > 1 {
                for &i in idxs {
                    if depths[i] < segs_list[i].len() {
                        depths[i] += 1;
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }

    for (i, raw) in paths.iter().enumerate() {
        let t = tail(i, depths[i]);
        let formatted = match t.as_str() {
            "/" => "/".into(),
            "~" => "~/".into(),
            _ => format!("{t}/"),
        };
        labels.insert(raw.clone(), formatted);
    }
    labels
}

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
            "#{@arbor-drag}",
            "#{pane_left}",
            "#{pane_top}",
            "#{@arbor-collapsed-dirs}",
        ]);
        let o = format!("O{SEP}{}", Config::query_format());
        let w =
            join(&["W", "#{window_id}", "#{window_index}", "#{window_active}", "#{window_flags}", "#{window_name}"]);
        let p = join(&[
            "P",
            "#{window_id}",
            "#{pane_id}",
            "#{pane_active}",
            "#{pane_last}",
            crate::tip::KIND,
            "#{pane_current_path}",
            "#{@arbor-name}",
            PANE_LABEL,
        ]);
        let out = tmux::Batch::new()
            .cmd(["display-message", "-p", "-t", pane, &s])
            .cmd(["display-message", "-p", "-t", pane, &o])
            .cmd(["list-windows", "-t", pane, "-F", &w])
            .cmd(["list-panes", "-s", "-t", pane, "-F", &p])
            .run()?;
        let mut snap = Snapshot::parse(&out)?;
        if snap.cfg.group_by == GroupBy::Git {
            snap.resolve_git_roots();
        }
        Ok(snap)
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
                    let collapsed_dirs = f
                        .get(16)
                        .map(|raw| {
                            raw.split(COLLAPSED_SEP)
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                                .map(normalize_path)
                                .collect()
                        })
                        .unwrap_or_default();
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
                        dragging: f.get(13).is_some_and(|d| on(d)),
                        pane_left: f.get(14).map_or(0, |v| num(v)),
                        pane_top: f.get(15).map_or(0, |v| num(v)),
                        collapsed_dirs,
                        windows: Vec::new(),
                        cfg: Config::default(),
                    });
                }
                "O" => cfg = Config::parse(rest),
                "W" => {
                    let f: Vec<&str> = rest.splitn(5, SEP).collect();
                    if f.len() < 5 {
                        continue;
                    }
                    windows.push(Win {
                        id: f[0].into(),
                        index: f[1].parse().unwrap_or(0),
                        active: on(f[2]),
                        flags: f[3].into(),
                        name: f[4].into(),
                        panes: Vec::new(),
                        sidebars: Vec::new(),
                    });
                }
                "P" => {
                    let f: Vec<&str> = rest.splitn(8, SEP).collect();
                    if f.len() < 8 {
                        continue;
                    }
                    let custom_name = (!f[6].trim().is_empty()).then(|| f[6].trim().to_string());
                    let label = Label::parse(f[7]);
                    let auto_cmd = label.plain();
                    let cmd = custom_name.clone().unwrap_or_else(|| auto_cmd.clone());
                    let info = PaneInfo {
                        id: f[1].into(),
                        active: on(f[2]),
                        last: on(f[3]),
                        cmd,
                        auto_cmd,
                        custom_name,
                        label,
                        cwd: normalize_path(f[5]),
                        git_root: None,
                    };
                    // A sidebar's popup is not part of the tree.
                    if f[4] != "tip" {
                        panes.push((f[0].into(), info, f[4] == "1"));
                    }
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

    pub fn resolve_git_roots(&mut self) {
        let mut cache: HashMap<String, Option<String>> = HashMap::new();
        for w in &mut self.windows {
            for p in &mut w.panes {
                p.git_root = cache.entry(p.cwd.clone()).or_insert_with(|| find_git_root(&p.cwd)).clone();
            }
        }
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

    pub fn pane_mut(&mut self, id: &str) -> Option<&mut PaneInfo> {
        self.windows.iter_mut().find_map(|w| w.panes.iter_mut().find(|p| p.id == id))
    }

    pub fn pane_count(&self) -> usize {
        self.windows.iter().map(|w| w.panes.len()).sum()
    }

    pub fn focused_pane(&self) -> Option<String> {
        let w = self.current()?;
        w.work_target().map(str::to_string)
    }

    pub fn pane_group_dir(&self, pid: &str) -> Option<String> {
        let (_, i) = self.find_pane(pid)?;
        let (w, _) = self.find_pane(pid)?;
        let p = &w.panes[i];
        Some(if self.cfg.group_by == GroupBy::Git {
            p.git_root.as_deref().unwrap_or(&p.cwd).to_string()
        } else {
            p.cwd.clone()
        })
    }

    pub fn groups(&self) -> Vec<DirGroup> {
        self.groups_with_home(home_dir())
    }

    pub fn groups_with_home(&self, home: Option<&str>) -> Vec<DirGroup> {
        let mut order: Vec<String> = Vec::new();
        struct Acc {
            min_win_index: u32,
            has_focused: bool,
            has_activity: bool,
            has_bell: bool,
            items: Vec<PaneItem>,
        }
        let mut by_dir: HashMap<String, Acc> = HashMap::new();

        for w in &self.windows {
            let count = w.panes.len();
            for (pi, p) in w.panes.iter().enumerate() {
                let raw = if self.cfg.group_by == GroupBy::Git {
                    p.git_root.as_deref().unwrap_or(&p.cwd).to_string()
                } else {
                    p.cwd.clone()
                };
                let acc = by_dir.entry(raw.clone()).or_insert_with(|| {
                    order.push(raw);
                    Acc {
                        min_win_index: w.index,
                        has_focused: false,
                        has_activity: false,
                        has_bell: false,
                        items: Vec::new(),
                    }
                });
                acc.min_win_index = acc.min_win_index.min(w.index);
                if w.active && p.active {
                    acc.has_focused = true;
                }
                if w.flags.contains('#') {
                    acc.has_activity = true;
                }
                if w.flags.contains('!') {
                    acc.has_bell = true;
                }
                acc.items.push(PaneItem {
                    wid: w.id.clone(),
                    win_index: w.index,
                    win_name: w.name.clone(),
                    win_flags: w.flags.clone(),
                    win_active: w.active,
                    pane_idx: pi,
                    win_pane_count: count,
                    pane: p.clone(),
                });
            }
        }

        let labels = compute_dir_labels(&order, home, self.cfg.dir_display);
        let mut groups: Vec<DirGroup> = order
            .into_iter()
            .filter_map(|raw| {
                let acc = by_dir.remove(&raw)?;
                let label = labels.get(&raw).cloned().unwrap_or_else(|| raw.clone());
                let display_path = shorten_home(&raw, home);
                let collapsed = self.collapsed_dirs.contains(&raw);
                Some(DirGroup {
                    raw_path: raw,
                    display_path,
                    label,
                    collapsed,
                    min_win_index: acc.min_win_index,
                    has_focused: acc.has_focused,
                    has_activity: acc.has_activity,
                    has_bell: acc.has_bell,
                    items: acc.items,
                })
            })
            .collect();

        match self.cfg.dir_sort {
            DirSort::Window => groups.sort_by_key(|g| g.min_win_index),
            DirSort::Alpha => {
                groups.sort_by(|a, b| a.label.cmp(&b.label).then_with(|| a.min_win_index.cmp(&b.min_win_index)))
            }
            DirSort::Active => groups
                .sort_by(|a, b| b.has_focused.cmp(&a.has_focused).then_with(|| a.min_win_index.cmp(&b.min_win_index))),
        }
        groups
    }

    /// Navigable nodes in order, honouring collapsed directory groups.
    pub fn nodes(&self, rail: bool) -> Vec<NodeKey> {
        let mut out = Vec::new();
        for g in self.groups() {
            if !rail {
                out.push(NodeKey::Dir(g.raw_path.clone()));
            }
            if !rail && g.collapsed {
                continue;
            }
            for it in g.items {
                out.push(NodeKey::Pane(it.pane.id));
            }
        }
        out
    }
}

use crate::config::Side;

/// The numeric part of a pane id, for ordering duplicate sidebars.
pub fn pane_number(id: &str) -> u32 {
    id.trim_start_matches('%').parse().unwrap_or(u32::MAX)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A session with 4 windows across 3 directories; the sidebar is %0 in w1.
    pub fn sample() -> Snapshot {
        let s = SEP;
        let mut out = String::new();
        out += &format!("S{s}$1{s}work{s}@1{s}0{s}1{s}1{s}28{s}120{s}1{s}0{s}1{s}1{s}0{s}0{s}0{s}0{s}\n");
        out += &format!("O{s}\n");
        out += &format!("W{s}@1{s}1{s}1{s}*{s}editor\n");
        out += &format!("W{s}@2{s}2{s}0{s}#{s}server\n");
        out += &format!("W{s}@3{s}3{s}0{s}-{s}git\n");
        out += &format!("W{s}@4{s}4{s}0{s}!{s}db\n");
        for (w, p, active, last, sb, cwd, custom, cmd) in [
            ("@1", "%0", 0, 0, "1", "/home/u/ws/tmux-arbor", "", "arbor"),
            ("@1", "%1", 1, 0, "", "/home/u/ws/tmux-arbor", "", "nvim"),
            ("@1", "%2", 0, 1, "", "/home/u/ws/tmux-arbor/src", "", "zsh"),
            ("@1", "%3", 0, 0, "", "/home/u/ws/tmux-arbor", "", "cargo"),
            ("@2", "%4", 1, 0, "", "/home/u/ws/api-server", "", "npm"),
            ("@2", "%5", 0, 0, "", "/home/u/ws/api-server", "", "tail"),
            ("@3", "%6", 1, 0, "", "/home/u/ws/tmux-arbor", "", "lazygit"),
            ("@4", "%7", 1, 0, "", "/home/u", "", "psql"),
        ] {
            out += &format!("P{s}{w}{s}{p}{s}{active}{s}{last}{s}{sb}{s}{cwd}{s}{custom}{s}{cmd}\n");
        }
        Snapshot::parse(&out).unwrap()
    }

    #[test]
    fn parses_session_and_groups_by_cwd() {
        let snap = sample();
        assert_eq!(snap.session_name, "work");
        assert_eq!(snap.windows.len(), 4);
        assert_eq!(snap.pane_count(), 7);
        let editor = snap.my_window().unwrap();
        assert_eq!(editor.panes.len(), 3);
        assert_eq!(editor.sidebars, vec!["%0"]);
        assert_eq!(editor.work_target(), Some("%1"));
        assert!(snap.docked(Side::Left));
        assert!(!snap.focused());

        let groups = snap.groups_with_home(Some("/home/u"));
        // Paths: /home/u/ws/tmux-arbor (3 panes: %1, %3, %6),
        //        /home/u/ws/tmux-arbor/src (1 pane: %2),
        //        /home/u/ws/api-server (2 panes: %4, %5),
        //        /home/u (1 pane: %7)
        assert_eq!(groups.len(), 4);
        assert_eq!(groups[0].label, "tmux-arbor/");
        assert_eq!(groups[0].items.len(), 3);
        assert!(groups[0].has_focused);
        assert_eq!(groups[1].label, "src/");
        assert_eq!(groups[1].items.len(), 1);
        assert_eq!(groups[2].label, "api-server/");
        assert!(groups[2].has_activity);
        assert_eq!(groups[3].label, "~/");
        assert!(groups[3].has_bell);
    }

    #[test]
    fn git_grouping_merges_subdir_and_tracks_rel_subdir() {
        let mut snap = sample();
        snap.cfg.group_by = GroupBy::Git;
        for w in &mut snap.windows {
            for p in &mut w.panes {
                if p.cwd.starts_with("/home/u/ws/tmux-arbor") {
                    p.git_root = Some("/home/u/ws/tmux-arbor".into());
                } else if p.cwd.starts_with("/home/u/ws/api-server") {
                    p.git_root = Some("/home/u/ws/api-server".into());
                }
            }
        }
        let groups = snap.groups_with_home(Some("/home/u"));
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].items.len(), 4);
        assert_eq!(groups[0].items[1].pane.id, "%2");
        assert_eq!(groups[0].items[1].rel_subdir(&groups[0].raw_path, Some("/home/u")).as_deref(), Some("src"));
    }

    #[test]
    fn smart_disambiguation_and_short_labels() {
        let paths = vec![
            "/home/u/ws/app".to_string(),
            "/home/u/work/app".to_string(),
            "/home/u/archive/2025/app".to_string(),
            "/home/u".to_string(),
        ];
        let smart = compute_dir_labels(&paths, Some("/home/u"), DirDisplay::Smart);
        assert_eq!(smart["/home/u/ws/app"], "ws/app/");
        assert_eq!(smart["/home/u/work/app"], "work/app/");
        assert_eq!(smart["/home/u/archive/2025/app"], "2025/app/");
        assert_eq!(smart["/home/u"], "~/");

        let short = compute_dir_labels(&paths, Some("/home/u"), DirDisplay::Short);
        assert_eq!(short["/home/u/ws/app"], "~/w/app/");
        assert_eq!(short["/home/u/archive/2025/app"], "~/a/2/app/");
        assert_eq!(short["/home/u"], "~/");
    }

    #[test]
    fn dir_sorting_variants() {
        let mut snap = sample();
        snap.cfg.dir_sort = DirSort::Alpha;
        let alpha: Vec<String> = snap.groups_with_home(Some("/home/u")).into_iter().map(|g| g.label).collect();
        assert_eq!(alpha, vec!["api-server/", "src/", "tmux-arbor/", "~/"]);

        snap.windows[0].active = false;
        snap.windows[1].active = true;
        snap.cfg.dir_sort = DirSort::Active;
        let active: Vec<String> = snap.groups_with_home(Some("/home/u")).into_iter().map(|g| g.label).collect();
        assert_eq!(active[0], "api-server/");
    }

    #[test]
    fn nodes_follow_dir_collapse() {
        let mut snap = sample();
        assert_eq!(snap.nodes(false).len(), 4 + 7);
        snap.collapsed_dirs.insert("/home/u/ws/tmux-arbor".into());
        let nodes = snap.nodes(false);
        assert_eq!(nodes.len(), 4 + 4);
        assert_eq!(nodes[0], NodeKey::Dir("/home/u/ws/tmux-arbor".into()));
        assert_eq!(nodes[1], NodeKey::Dir("/home/u/ws/tmux-arbor/src".into()));
        assert_eq!(snap.nodes(true).len(), 7);
    }

    #[test]
    fn custom_pane_name_overrides_cmd_and_keeps_auto_in_tooltip() {
        let s = SEP;
        let mut out = String::new();
        out += &format!("S{s}$1{s}work{s}@1{s}0{s}1{s}1{s}28{s}120{s}1{s}0{s}1{s}1{s}0{s}0{s}0{s}0{s}\n");
        out += &format!("O{s}\n");
        out += &format!("W{s}@1{s}1{s}1{s}*{s}agy\n");
        out += &format!("P{s}@1{s}%1{s}1{s}0{s}{s}/home/u/ws/xray-manager{s}防火墙排查{s}agy\n");
        let snap = Snapshot::parse(&out).unwrap();
        let groups = snap.groups_with_home(Some("/home/u"));
        let item = &groups[0].items[0];
        assert_eq!(item.pane.cmd, "防火墙排查");
        assert_eq!(item.pane.auto_cmd, "agy");
        assert_eq!(item.pane.custom_name.as_deref(), Some("防火墙排查"));
        assert_eq!(item.tooltip(Some("/home/u")), "防火墙排查 (agy) · w1 · ~/ws/xray-manager");
    }
}
