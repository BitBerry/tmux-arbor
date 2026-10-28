//! The one-line popup that shows a cut-off name whole. It is a floating
//! pane (tmux 3.8) laid next to the sidebar, outside it: a floating pane
//! takes the mouse events over it, so over the sidebar it would swallow
//! the very hover that shows it. The pane runs `arbor tip`, which only
//! prints the text.

use std::io::Write;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::Result;

use crate::label::parse_color;
use crate::screen::{Color, Style, width};
use crate::tmux::{self, shell_quote};

/// Title that marks a popup pane, so the tree and commands skip it.
pub const TITLE: &str = "arbor-tip";
/// Format telling panes apart: "tip" for a popup, "1" for a sidebar, ""
/// for a work pane.
pub const KIND: &str = "#{?#{==:#{pane_title},arbor-tip},tip,#{@arbor-sidebar}}";

/// Whether this tmux has floating panes.
pub fn supported() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| tmux::run(["list-commands", "new-pane"]).is_ok())
}

/// Where a popup goes, in window coordinates, and what it says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Popup {
    pub x: u16,
    pub y: u16,
    pub text: String,
    pub fg: Color,
    pub bg: Color,
}

fn hex(c: Color) -> String {
    match c {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Idx(n) => format!("colour{n}"),
        Color::Default => "default".into(),
    }
}

/// Opens a popup in the window of `owner` (the sidebar pane), without
/// taking the keyboard. Returns its pane id.
pub fn open(owner: &str, bin: &str, p: &Popup) -> Result<String> {
    let w = (width(&p.text) + 2).to_string();
    let (x, y) = (p.x.to_string(), p.y.to_string());
    let [text, colors, pid] = env(p);
    let id = tmux::run([
        "new-pane",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        owner,
        "-x",
        &w,
        "-y",
        "1",
        "-X",
        &x,
        "-Y",
        &y,
        "-B",
        "none",
        "-T",
        TITLE,
        "-e",
        &text,
        "-e",
        &colors,
        "-e",
        &pid,
        &format!("exec {} tip", shell_quote(bin)),
    ])?
    .trim()
    .to_string();
    let _ = tmux::run(["set-option", "-p", "-t", &id, "remain-on-exit", "off"]);
    Ok(id)
}

fn env(p: &Popup) -> [String; 3] {
    [
        format!("ARBOR_TIP={}", p.text),
        format!("ARBOR_TIP_COLORS={},{}", hex(p.fg), hex(p.bg)),
        format!("ARBOR_TIP_OWNER={}", std::process::id()),
    ]
}

/// Moves an open popup and gives it new text, in one tmux call: cheaper
/// than closing it and opening another as the pointer goes down a list.
pub fn update(pane: &str, bin: &str, p: &Popup) -> Result<()> {
    let w = (width(&p.text) + 2).to_string();
    let (x, y) = (p.x.to_string(), p.y.to_string());
    let [text, colors, pid] = env(p);
    tmux::Batch::new()
        .cmd(["move-pane", "-t", pane, "-X", &x, "-Y", &y])
        .cmd(["resize-pane", "-t", pane, "-x", &w, "-y", "1"])
        .cmd([
            "respawn-pane",
            "-k",
            "-t",
            pane,
            "-e",
            &text,
            "-e",
            &colors,
            "-e",
            &pid,
            &format!("exec {} tip", shell_quote(bin)),
        ])
        .run()?;
    Ok(())
}

pub fn close(pane: &str) {
    let _ = tmux::run(["kill-pane", "-t", pane]);
}

/// `arbor tip`: prints the text and stays until the sidebar that opened it
/// is gone (it normally closes the popup itself).
pub fn run() -> Result<()> {
    let text = std::env::var("ARBOR_TIP").unwrap_or_default();
    let colors = std::env::var("ARBOR_TIP_COLORS").unwrap_or_default();
    let mut parts = colors.split(',').map(|c| parse_color(c).unwrap_or_default());
    let style = Style::default().fg(parts.next().unwrap_or_default()).bg(parts.next().unwrap_or_default());
    let owner: i32 = std::env::var("ARBOR_TIP_OWNER").ok().and_then(|p| p.parse().ok()).unwrap_or(0);
    let mut out = std::io::stdout().lock();
    write!(out, "\x1b[?25l\x1b[?7l\x1b[H{} {text} \x1b[0m", style.sgr())?;
    out.flush()?;
    drop(out);
    loop {
        std::thread::sleep(Duration::from_secs(1));
        // SAFETY: kill(2) with signal 0 only checks that the pid exists.
        if owner > 0 && unsafe { libc::kill(owner, 0) } != 0 {
            return Ok(());
        }
    }
}
