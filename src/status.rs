//! The status line. While the tree is shown it carries the window list, so
//! status-format[0] is swapped for a left/right-only version; hiding the
//! tree restores the user's own. A ≡ button toggles the sidebar.

use anyhow::Result;

use crate::config::{Config, Mode};
use crate::tmux;

/// tmux's default status-format[0] minus the window list.
const LEFT_RIGHT: &str = "#[align=left range=left #{E:status-left-style}]#[push-default]#{T;=/#{status-left-length}:status-left}#[pop-default]#[norange default]#[nolist align=right range=right #{E:status-right-style}]#[push-default]#{T;=/#{status-right-length}:status-right}#[pop-default]#[norange default]";

/// Clicks inside this range report `arbor` as #{mouse_status_range}.
pub const BUTTON: &str = "#[range=user|arbor]#[fg=#597ea3,bold] ≡ #[default]#[norange]";

/// User ranges in status formats arrived in tmux 3.4.
pub fn button_supported() -> bool {
    tmux::version() >= (3, 4)
}

pub fn wants_button(cfg: &Config) -> bool {
    cfg.hide_status_windows && cfg.status_button && button_supported()
}

fn compose(cfg: &Config, mode: Mode, orig: &str) -> String {
    if !cfg.hide_status_windows {
        return orig.to_string();
    }
    let body = if mode == Mode::Hidden { orig } else { LEFT_RIGHT };
    if wants_button(cfg) { format!("{BUTTON}{body}") } else { body.to_string() }
}

pub fn apply(cfg: &Config, mode: Mode) -> Result<()> {
    let cur = tmux::show_global("status-format[0]")?;
    let saved = tmux::show_global("@arbor-status-orig")?;
    let applied = tmux::show_global("@arbor-status-applied")?;
    // Anything other than what we last wrote is the user's (re-sourced
    // config, manual change), and becomes the new original.
    let orig = if saved.is_empty() || cur != applied { cur.clone() } else { saved };
    let want = compose(cfg, mode, &orig);
    let mut b = tmux::Batch::new();
    b.cmd(["set-option", "-g", "@arbor-status-orig", &orig]);
    b.cmd(["set-option", "-g", "@arbor-status-applied", &want]);
    if want != cur {
        b.cmd(["set-option", "-g", "status-format[0]", &want]);
    }
    b.run()?;
    Ok(())
}

/// Puts the user's status line back and forgets it.
pub fn restore() -> Result<()> {
    let orig = tmux::show_global("@arbor-status-orig")?;
    let mut b = tmux::Batch::new();
    if !orig.is_empty() {
        b.cmd(["set-option", "-g", "status-format[0]", &orig]);
    }
    b.cmd(["set-option", "-gu", "@arbor-status-orig"]);
    b.cmd(["set-option", "-gu", "@arbor-status-applied"]);
    b.run()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composes_by_mode() {
        let mut cfg = Config { status_button: false, ..Config::default() };
        assert_eq!(compose(&cfg, Mode::Full, "ORIG"), LEFT_RIGHT);
        assert_eq!(compose(&cfg, Mode::Hidden, "ORIG"), "ORIG");
        cfg.hide_status_windows = false;
        assert_eq!(compose(&cfg, Mode::Full, "ORIG"), "ORIG");
    }
}
