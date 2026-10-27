//! Commands run from key bindings, hooks and the sidebar itself. Shared
//! state lives in tmux options; sidebars are told to redraw with signals.

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::config::{Config, MAX_W, MIN_FULL, Mode, Side};
use crate::split;
use crate::status;
use crate::tmux::{self, Batch, SEP, shell_quote};

/// Our slot in each hook array, so user hooks are left alone.
const HOOK_INDEX: u32 = 47;
/// A window keeps at least this many columns for work when a sidebar opens
/// on its own (hooks); explicit commands open it regardless.
const MIN_WORK: u16 = 20;

/// Hooks that may bring a window without a sidebar into view.
const SYNC_HOOKS: &[&str] = &[
    "client-attached",
    "client-session-changed",
    "session-window-changed",
    "after-new-window",
    "after-new-session",
    "window-unzoomed",
];
/// Layout commands that can pull the sidebar off its edge.
const RELAYOUT_HOOKS: &[&str] =
    &["after-select-layout", "after-next-layout", "after-previous-layout", "after-rotate-window", "after-swap-pane"];
/// Anything else the tree shows.
const NOTIFY_HOOKS: &[&str] = &[
    "window-pane-changed",
    "window-layout-changed",
    "window-renamed",
    "window-linked",
    "window-unlinked",
    "pane-exited",
    "after-kill-pane",
    "alert-activity",
    "alert-bell",
    "alert-silence",
    "session-renamed",
];
const THEME_HOOKS: &[&str] = &["client-light-theme", "client-dark-theme"];

const USAGE: &str = "usage: arbor <command>

  init                 set up bindings and hooks (run by arbor.tmux)
  toggle [window]      show or hide the sidebar
  focus [window]       move the keyboard into the tree
  cycle [window]       full › rail › hidden
  rail                 switch between rail and full
  resize <+N|-N|N|reset>
  flip                 move the sidebar to the other side
  split <h|v> <pane>   split the work pane next to a sidebar
  sync                 open sidebars where clients are looking
  notify               redraw every sidebar
  uninstall            remove sidebars, bindings and hooks
  sidebar              run a sidebar (inside its pane)";

pub fn run(args: &[String]) -> Result<()> {
    let arg = |i: usize| args.get(i).map(String::as_str).filter(|s| !s.is_empty());
    match arg(0) {
        Some("init") => init(),
        Some("sidebar") => crate::sidebar::run(),
        Some("toggle") => toggle(arg(1)),
        Some("focus") => focus(arg(1)),
        Some("cycle") => cycle(arg(1)),
        Some("rail") => {
            let mut cfg = Config::load()?;
            cfg.mode = if cfg.mode == Mode::Rail { Mode::Full } else { Mode::Rail };
            commit(&cfg)
        }
        Some("resize") => resize(arg(1).unwrap_or("reset")),
        Some("flip") => {
            let mut cfg = Config::load()?;
            cfg.position = cfg.position.other();
            flip(&cfg)
        }
        Some("sync") => sync(&Config::load()?, None),
        Some("notify") => notify(libc::SIGUSR1),
        Some("hook") => hook(arg(1).unwrap_or(""), arg(2)),
        Some("border-dblclick") => border_dblclick(arg(1)),
        Some("split") => split::run(arg(1), arg(2)),
        Some("uninstall") => uninstall(),
        Some("version" | "--version" | "-V") => {
            println!("arbor {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => {
            println!("{USAGE}");
            Ok(())
        }
    }
}

/// One pane anywhere on the server, as the control commands need it.
#[derive(Clone, Debug)]
pub struct PaneRow {
    pub window: String,
    pub pane: String,
    pub sidebar: bool,
    /// Published by the sidebar process once it can take signals.
    pub pid: Option<i32>,
    pub active: bool,
    pub last: bool,
    pub zoomed: bool,
    pub left: u16,
    pub width: u16,
    pub window_width: u16,
    pub at_left: bool,
    pub at_right: bool,
    pub at_top: bool,
    pub at_bottom: bool,
}

impl PaneRow {
    pub fn docked(&self, side: Side) -> bool {
        self.at_top
            && self.at_bottom
            && match side {
                Side::Left => self.at_left,
                Side::Right => self.at_right,
            }
    }
}

fn row_format() -> String {
    [
        "#{window_id}",
        "#{pane_id}",
        "#{@arbor-sidebar}",
        "#{@arbor-pid}",
        "#{pane_active}",
        "#{pane_last}",
        "#{window_zoomed_flag}",
        "#{pane_left}",
        "#{pane_width}",
        "#{window_width}",
        "#{pane_at_left}",
        "#{pane_at_right}",
        "#{pane_at_top}",
        "#{pane_at_bottom}",
    ]
    .join(&SEP.to_string())
}

fn parse_rows(out: &str) -> Vec<PaneRow> {
    let on = |s: &str| s == "1";
    let num = |s: &str| s.parse::<u16>().unwrap_or(0);
    out.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split(SEP).collect();
            if f.len() < 14 {
                return None;
            }
            Some(PaneRow {
                window: f[0].into(),
                pane: f[1].into(),
                sidebar: on(f[2]),
                pid: f[3].parse().ok(),
                active: on(f[4]),
                last: on(f[5]),
                zoomed: on(f[6]),
                left: num(f[7]),
                width: num(f[8]),
                window_width: num(f[9]),
                at_left: on(f[10]),
                at_right: on(f[11]),
                at_top: on(f[12]),
                at_bottom: on(f[13]),
            })
        })
        .collect()
}

pub fn all_rows() -> Result<Vec<PaneRow>> {
    Ok(parse_rows(&tmux::run(["list-panes", "-a", "-F", &row_format()])?))
}

/// Panes of the window holding `target` (a window or pane id).
pub fn window_rows(target: &str) -> Result<Vec<PaneRow>> {
    Ok(parse_rows(&tmux::run(["list-panes", "-t", target, "-F", &row_format()])?))
}

fn sidebars(rows: &[PaneRow]) -> impl Iterator<Item = &PaneRow> {
    rows.iter().filter(|r| r.sidebar)
}

/// The work pane a sidebar should split from or hand focus to.
pub fn work_pane<'a>(rows: &'a [PaneRow], window: &str) -> Option<&'a PaneRow> {
    let work = || rows.iter().filter(move |r| r.window == window && !r.sidebar);
    work().find(|r| r.active).or_else(|| work().find(|r| r.last)).or_else(|| work().next())
}

/// Serializes sidebar creation and docking across concurrent hooks.
struct Lock(#[allow(dead_code)] Option<File>);

impl Lock {
    fn acquire() -> Lock {
        let file = OpenOptions::new().create(true).truncate(false).write(true).open(lock_path()).ok();
        if let Some(f) = &file {
            // SAFETY: flock on an open descriptor; released when it closes.
            unsafe {
                libc::flock(f.as_raw_fd(), libc::LOCK_EX);
            }
        }
        Lock(file)
    }
}

fn lock_path() -> PathBuf {
    let socket = std::env::var("TMUX").ok().and_then(|t| t.split(',').next().map(str::to_string));
    match socket {
        Some(s) if !s.is_empty() => PathBuf::from(format!("{s}.arbor.lock")),
        // SAFETY: getuid cannot fail.
        _ => std::env::temp_dir().join(format!("arbor-{}.lock", unsafe { libc::getuid() })),
    }
}

/// Opens a sidebar in `window` unless it has one. Returns its pane id.
fn open_sidebar(window: &str, rows: &[PaneRow], cfg: &Config, force: bool) -> Result<Option<String>> {
    if let Some(sb) = sidebars(rows).find(|r| r.window == window) {
        return Ok(Some(sb.pane.clone()));
    }
    let Some(target) = work_pane(rows, window) else { return Ok(None) };
    let cols = cfg.cols();
    if cols == 0 || (!force && (target.zoomed || target.window_width < cols + MIN_WORK)) {
        return Ok(None);
    }
    let cols = cols.to_string();
    let cmd = format!("exec {} sidebar", shell_quote(&cfg.bin_path()));
    let mut args = vec!["split-window", "-h", "-f", "-d"];
    if cfg.position == Side::Left {
        args.push("-b");
    }
    args.extend(["-l", &cols, "-t", &target.pane, "-P", "-F", "#{pane_id}", &cmd]);
    let id = tmux::run(&args)?.trim().to_string();
    // The sidebar marks itself too; doing it here closes the race with a
    // second hook looking at the same window.
    let _ = tmux::run(["set-option", "-p", "-t", &id, "@arbor-sidebar", "1"]);
    Ok(Some(id))
}

/// Makes sure every window a client is looking at has a sidebar.
pub fn sync(cfg: &Config, force: Option<&str>) -> Result<()> {
    if cfg.mode == Mode::Hidden {
        return Ok(());
    }
    let _lock = Lock::acquire();
    let clients = tmux::run(["list-clients", "-F", "#{window_id}"]).unwrap_or_default();
    let mut windows: Vec<&str> = clients.lines().map(str::trim).filter(|w| !w.is_empty()).collect();
    windows.extend(force);
    windows.sort_unstable();
    windows.dedup();
    let rows = all_rows()?;
    for w in windows {
        if let Err(e) = open_sidebar(w, &rows, cfg, Some(w) == force) {
            eprintln!("arbor: {w}: {e:#}");
        }
    }
    Ok(())
}

/// Writes mode, width and side back to the shared options.
pub fn persist(cfg: &Config) -> Result<()> {
    Batch::new()
        .cmd(["set-option", "-g", "@arbor-mode", cfg.mode.name()])
        .cmd(["set-option", "-g", "@arbor-width", &cfg.width.to_string()])
        .cmd(["set-option", "-g", "@arbor-position", cfg.position.name()])
        .run()?;
    Ok(())
}

/// Resizes every sidebar to the configured width. Zoomed windows are left
/// alone: resizing would unzoom them.
pub fn apply_all(cfg: &Config) -> Result<()> {
    let cols = cfg.cols();
    let rows = all_rows()?;
    let mut b = Batch::new();
    for r in sidebars(&rows).filter(|r| !r.zoomed && r.width != cols) {
        b.cmd(["resize-pane", "-t", &r.pane, "-x", &cols.to_string()]);
    }
    let _ = b.run();
    Ok(())
}

pub fn signal(rows: &[PaneRow], sig: i32) {
    for pid in sidebars(rows).filter_map(|r| r.pid) {
        // SAFETY: kill(2) with a pid read from tmux; a stale pid is harmless.
        unsafe {
            libc::kill(pid, sig);
        }
    }
}

/// SIGUSR1 asks sidebars to re-read tmux; SIGUSR2 also re-detects the theme.
pub fn notify(sig: i32) -> Result<()> {
    signal(&all_rows()?, sig);
    Ok(())
}

/// Persists a mode or width change, applies it everywhere, and redraws.
pub fn commit(cfg: &Config) -> Result<()> {
    if cfg.mode == Mode::Hidden {
        return hide(cfg);
    }
    persist(cfg)?;
    status::apply(cfg, cfg.mode)?;
    apply_all(cfg)?;
    notify(libc::SIGUSR1)
}

pub fn hide(cfg: &Config) -> Result<()> {
    let mut b = Batch::new();
    if cfg.mode != Mode::Hidden {
        b.cmd(["set-option", "-g", "@arbor-last-mode", cfg.mode.name()]);
    }
    b.cmd(["set-option", "-g", "@arbor-mode", "hidden"]);
    b.run()?;
    status::apply(cfg, Mode::Hidden)?;
    let rows = all_rows()?;
    let mut kill = Batch::new();
    for r in sidebars(&rows) {
        kill.cmd(["kill-pane", "-t", &r.pane]);
    }
    let _ = kill.run();
    Ok(())
}

fn show(cfg: &mut Config, mode: Mode, window: Option<&str>) -> Result<()> {
    cfg.mode = mode;
    persist(cfg)?;
    status::apply(cfg, mode)?;
    sync(cfg, window)?;
    notify(libc::SIGUSR1)
}

fn toggle(window: Option<&str>) -> Result<()> {
    let mut cfg = Config::load()?;
    if cfg.mode == Mode::Hidden {
        let mode = cfg.last_mode;
        show(&mut cfg, mode, window)
    } else {
        hide(&cfg)
    }
}

/// prefix W: full › rail › hidden › full.
fn cycle(window: Option<&str>) -> Result<()> {
    let mut cfg = Config::load()?;
    match cfg.mode {
        Mode::Full => {
            cfg.mode = Mode::Rail;
            commit(&cfg)
        }
        Mode::Rail => {
            // Coming back from hidden after a cycle starts full again.
            cfg.mode = Mode::Full;
            hide(&cfg)
        }
        Mode::Hidden => show(&mut cfg, Mode::Full, window),
    }
}

fn current_window() -> Result<String> {
    Ok(tmux::run(["display-message", "-p", "#{window_id}"])?.trim().to_string())
}

/// prefix T: shows the sidebar if needed and gives it the keyboard.
fn focus(window: Option<&str>) -> Result<()> {
    let mut cfg = Config::load()?;
    let window = match window {
        Some(w) => w.to_string(),
        None => current_window()?,
    };
    if cfg.mode == Mode::Hidden {
        let mode = cfg.last_mode;
        show(&mut cfg, mode, Some(&window))?;
    }
    let sb = {
        let _lock = Lock::acquire();
        open_sidebar(&window, &window_rows(&window)?, &cfg, true)?
    };
    if let Some(sb) = sb {
        tmux::run(["select-pane", "-t", &sb])?;
    }
    Ok(())
}

/// Moves a sidebar back to full height against its edge.
fn dock(sb: &PaneRow, rows: &[PaneRow], cfg: &Config) -> Result<()> {
    let Some(target) = work_pane(rows, &sb.window) else { return Ok(()) };
    let cols = cfg.cols().to_string();
    let mut args = vec!["join-pane", "-h", "-f"];
    if cfg.position == Side::Left {
        args.push("-b");
    }
    // Keep the keyboard where it is: only an active sidebar stays active.
    if !sb.active {
        args.push("-d");
    }
    args.extend(["-l", &cols, "-s", &sb.pane, "-t", &target.pane]);
    tmux::run(&args)?;
    Ok(())
}

/// Docks one sidebar and sets its width, whichever is off.
pub fn fix_sidebar(pane: &str, cfg: &Config) -> Result<()> {
    let _lock = Lock::acquire();
    let rows = window_rows(pane)?;
    let Some(sb) = rows.iter().find(|r| r.pane == pane) else { return Ok(()) };
    if sb.zoomed {
        return Ok(());
    }
    if !sb.docked(cfg.position) {
        dock(sb, &rows, cfg)?;
    } else if sb.width != cfg.cols() {
        tmux::run(["resize-pane", "-t", pane, "-x", &cfg.cols().to_string()])?;
    }
    Ok(())
}

pub fn flip(cfg: &Config) -> Result<()> {
    persist(cfg)?;
    {
        let _lock = Lock::acquire();
        let rows = all_rows()?;
        for sb in sidebars(&rows).filter(|r| !r.zoomed) {
            if let Err(e) = dock(sb, &rows, cfg) {
                eprintln!("arbor: {}: {e:#}", sb.pane);
            }
        }
    }
    notify(libc::SIGUSR1)
}

/// Hands the keyboard back to the work area of the sidebar's window, if the
/// sidebar has it.
pub fn focus_work(sidebar: &str) -> Result<()> {
    let rows = window_rows(sidebar)?;
    let Some(sb) = rows.iter().find(|r| r.pane == sidebar) else { return Ok(()) };
    if sb.active
        && let Some(work) = work_pane(&rows, &sb.window)
    {
        tmux::run(["select-pane", "-t", &work.pane])?;
    }
    Ok(())
}

/// One step of < or >: ±2 columns, snapping to the rail below the minimum.
pub fn step_width(cfg: &mut Config, delta: i32) -> bool {
    match cfg.mode {
        Mode::Rail if delta > 0 => {
            cfg.mode = Mode::Full;
            true
        }
        Mode::Full if delta < 0 && cfg.width <= MIN_FULL => {
            cfg.mode = Mode::Rail;
            true
        }
        Mode::Full => {
            let w = (cfg.width as i32 + delta).clamp(MIN_FULL as i32, MAX_W as i32) as u16;
            let changed = w != cfg.width;
            cfg.width = w;
            changed
        }
        _ => false,
    }
}

fn resize(arg: &str) -> Result<()> {
    let mut cfg = Config::load()?;
    if arg == "reset" {
        cfg.mode = Mode::Full;
        cfg.width = cfg.default_width;
    } else if arg.starts_with(['+', '-']) {
        step_width(&mut cfg, arg.parse().context("bad width step")?);
    } else {
        let w: u16 = arg.parse().context("bad width")?;
        cfg.mode = Mode::Full;
        cfg.width = w.clamp(MIN_FULL, MAX_W);
    }
    commit(&cfg)
}

/// Double-click on the sidebar's divider restores the default width. tmux
/// reports no position for border clicks, only the pane the border belongs
/// to: the one on its left.
fn border_dblclick(pane: Option<&str>) -> Result<()> {
    let Some(pane) = pane else { return Ok(()) };
    let mut cfg = Config::load()?;
    let rows = window_rows(pane)?;
    let (Some(sb), Some(p)) = (sidebars(&rows).next(), rows.iter().find(|r| r.pane == pane)) else { return Ok(()) };
    let ours = match cfg.position {
        Side::Left => p.sidebar,
        Side::Right => !p.sidebar && p.left + p.width + 1 == sb.left,
    };
    if !ours {
        return Ok(());
    }
    cfg.mode = Mode::Full;
    cfg.width = cfg.default_width;
    commit(&cfg)
}

fn hook(name: &str, window: Option<&str>) -> Result<()> {
    if THEME_HOOKS.contains(&name) {
        return notify(libc::SIGUSR2);
    }
    if SYNC_HOOKS.contains(&name) {
        let cfg = Config::load()?;
        sync(&cfg, None)?;
    } else if RELAYOUT_HOOKS.contains(&name)
        && let Some(w) = window
    {
        let cfg = Config::load()?;
        if cfg.mode != Mode::Hidden
            && let Some(sb) = sidebars(&window_rows(w)?).next()
        {
            fix_sidebar(&sb.pane, &cfg)?;
        }
    }
    notify(libc::SIGUSR1)
}

fn run_shell(bin: &str, args: &str) -> String {
    format!("run-shell -b \"{} {args}\"", shell_quote(bin))
}

/// The command half of a `list-keys` line for `key`.
fn binding_command(listing: &str, key: &str) -> Option<String> {
    let line = listing.lines().find(|l| l.split_whitespace().any(|w| w == key))?;
    let at = line.find(&format!(" {key} "))? + key.len() + 2;
    Some(line[at..].trim().to_string())
}

fn init() -> Result<()> {
    let exe = std::env::current_exe().context("cannot locate arbor")?;
    let bin = exe.canonicalize().unwrap_or(exe).to_string_lossy().into_owned();
    let get = |k: &str| tmux::show_global(k).unwrap_or_default();

    let mut b = Batch::new();
    b.cmd(["set-option", "-g", "@arbor-bin", &bin]);
    for (k, v) in [
        ("@arbor-mode", "full"),
        ("@arbor-position", "left"),
        ("@arbor-style", "branches"),
        ("@arbor-hide-status-windows", "on"),
        ("@arbor-status-button", "on"),
    ] {
        if get(k).is_empty() {
            b.cmd(["set-option", "-g", k, v]);
        }
    }
    let width = get("@arbor-width");
    let width = if width.is_empty() { crate::config::DEFAULT_WIDTH.to_string() } else { width };
    b.cmd(["set-option", "-g", "@arbor-width", &width]);
    if get("@arbor-default-width").is_empty() {
        b.cmd(["set-option", "-g", "@arbor-default-width", &width]);
    }
    b.run()?;

    for (opt, default, action) in
        [("@arbor-key-toggle", "Tab", "toggle"), ("@arbor-key-focus", "T", "focus"), ("@arbor-key-cycle", "W", "cycle")]
    {
        let key = get(opt);
        let key = if key.is_empty() { default.to_string() } else { key };
        if key != "none" {
            tmux::run([
                "bind-key",
                &key,
                "run-shell",
                "-b",
                &format!("{} {action} '#{{window_id}}'", shell_quote(&bin)),
            ])?;
        }
    }

    let mut hooks = Batch::new();
    let all: Vec<&str> = [SYNC_HOOKS, RELAYOUT_HOOKS, NOTIFY_HOOKS, THEME_HOOKS].concat();
    for h in &all {
        hooks.cmd([
            "set-hook",
            "-g",
            &format!("{h}[{HOOK_INDEX}]"),
            &run_shell(&bin, &format!("hook {h} '#{{window_id}}'")),
        ]);
    }
    if hooks.run().is_err() {
        // An older tmux lacks some hooks; install what it has.
        for h in &all {
            let _ = tmux::run([
                "set-hook",
                "-g",
                &format!("{h}[{HOOK_INDEX}]"),
                &run_shell(&bin, &format!("hook {h} '#{{window_id}}'")),
            ]);
        }
    }

    // The user's split keys must split the work pane, not the tree.
    split::install(&bin)?;

    // Focusing the tree must not rename the window to "arbor".
    let rename = get("automatic-rename-format");
    if !rename.contains("@arbor-sidebar") {
        Batch::new()
            .cmd(["set-option", "-g", "@arbor-orig-rename-format", &rename])
            .cmd([
                "set-option",
                "-g",
                "automatic-rename-format",
                &format!("#{{?#{{@arbor-sidebar}},#{{window_name}},{}}}", escape_branch(&rename)),
            ])
            .run()?;
    }

    let cfg = Config::load()?;
    if status::wants_button(&cfg) {
        let listing = tmux::run(["list-keys", "-T", "root", "MouseDown1Status"]).unwrap_or_default();
        let orig = binding_command(&listing, "MouseDown1Status").unwrap_or_else(|| "switch-client -t =".into());
        let orig = if orig.contains("arbor") { get("@arbor-orig-status-click") } else { orig };
        let orig = if orig.is_empty() { "switch-client -t =".to_string() } else { orig };
        Batch::new()
            .cmd(["set-option", "-g", "@arbor-orig-status-click", &orig])
            .cmd([
                "bind-key",
                "-T",
                "root",
                "MouseDown1Status",
                "if-shell",
                "-F",
                "#{==:#{mouse_status_range},arbor}",
                &run_shell(&bin, "toggle '#{window_id}'"),
                &orig,
            ])
            .run()?;
    }
    let listing = tmux::run(["list-keys", "-T", "root", "DoubleClick1Border"]).unwrap_or_default();
    if listing.trim().is_empty() || listing.contains("arbor") {
        tmux::run([
            "bind-key",
            "-T",
            "root",
            "DoubleClick1Border",
            "run-shell",
            "-b",
            "-t",
            "=",
            &format!("{} border-dblclick '#{{pane_id}}'", shell_quote(&bin)),
        ])?;
    }

    status::apply(&cfg, cfg.mode)?;
    sync(&cfg, None)?;
    notify(libc::SIGUSR1)
}

/// Escapes a format so it can sit in a `#{?cond,a,b}` branch: top-level
/// commas and closing braces become `#,` and `#}`.
pub fn escape_branch(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0u32;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '#' => {
                out.push('#');
                if let Some(n) = chars.next() {
                    out.push(n);
                    if n == '{' {
                        depth += 1;
                    }
                }
            }
            '}' if depth > 0 => {
                depth -= 1;
                out.push('}');
            }
            '}' => out.push_str("#}"),
            ',' if depth == 0 => out.push_str("#,"),
            _ => out.push(c),
        }
    }
    out
}

fn uninstall() -> Result<()> {
    let get = |k: &str| tmux::show_global(k).unwrap_or_default();
    if let Ok(rows) = all_rows() {
        let mut kill = Batch::new();
        for r in sidebars(&rows) {
            kill.cmd(["kill-pane", "-t", &r.pane]);
        }
        let _ = kill.run();
    }
    status::restore()?;
    split::uninstall()?;
    let rename = get("@arbor-orig-rename-format");
    if !rename.is_empty() {
        tmux::set_global("automatic-rename-format", &rename)?;
    }
    for h in [SYNC_HOOKS, RELAYOUT_HOOKS, NOTIFY_HOOKS, THEME_HOOKS].concat() {
        let _ = tmux::run(["set-hook", "-gu", &format!("{h}[{HOOK_INDEX}]")]);
    }
    for (opt, default) in [("@arbor-key-toggle", "Tab"), ("@arbor-key-focus", "T"), ("@arbor-key-cycle", "W")] {
        let key = get(opt);
        let key = if key.is_empty() { default.to_string() } else { key };
        let listing = tmux::run(["list-keys", "-T", "prefix", &key]).unwrap_or_default();
        if listing.contains("arbor") {
            let _ = tmux::run(["unbind-key", &key]);
        }
    }
    let click = get("@arbor-orig-status-click");
    if !click.is_empty() {
        // The saved binding is tmux command syntax; let tmux parse it.
        let path = std::env::temp_dir().join(format!("arbor-restore-{}.conf", std::process::id()));
        std::fs::write(&path, format!("bind-key -T root MouseDown1Status {click}\n"))?;
        let r = tmux::run(["source-file", &path.to_string_lossy()]);
        let _ = std::fs::remove_file(&path);
        r?;
    }
    if tmux::run(["list-keys", "-T", "root", "DoubleClick1Border"]).unwrap_or_default().contains("arbor") {
        let _ = tmux::run(["unbind-key", "-T", "root", "DoubleClick1Border"]);
    }
    for k in ["@arbor-bin", "@arbor-orig-rename-format", "@arbor-orig-status-click"] {
        let _ = tmux::run(["set-option", "-gu", k]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_escaping() {
        assert_eq!(
            escape_branch("#{?pane_in_mode,[tmux],#{pane_current_command}}#{?pane_dead,[dead],}"),
            "#{?pane_in_mode,[tmux],#{pane_current_command}}#{?pane_dead,[dead],}"
        );
        assert_eq!(escape_branch("a,b}"), "a#,b#}");
        assert_eq!(escape_branch("x#,y"), "x#,y");
    }

    #[test]
    fn binding_commands() {
        let listing = "bind-key    -T root MouseDown1Status          switch-client -t =\n";
        assert_eq!(binding_command(listing, "MouseDown1Status").as_deref(), Some("switch-client -t ="));
        assert_eq!(binding_command("", "MouseDown1Status"), None);
    }

    #[test]
    fn width_steps() {
        let mut c = Config { width: 16, ..Config::default() };
        assert!(step_width(&mut c, -2));
        assert_eq!((c.mode, c.width), (Mode::Full, 14));
        assert!(step_width(&mut c, -2));
        assert_eq!(c.mode, Mode::Rail);
        assert!(step_width(&mut c, 2));
        assert_eq!((c.mode, c.width), (Mode::Full, 14));
        c.width = MAX_W;
        assert!(!step_width(&mut c, 2));
    }

    #[test]
    fn rows_without_a_published_pid_are_not_signalled() {
        let s = SEP;
        let line = format!("@1{s}%3{s}1{s}{s}0{s}0{s}0{s}0{s}28{s}120{s}1{s}0{s}1{s}1");
        let rows = parse_rows(&line);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pid, None);
        assert!(rows[0].docked(Side::Left));
    }
}
