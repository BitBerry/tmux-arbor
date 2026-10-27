//! Raw mode for the sidebar pane, restored on drop.

use std::io::{self, Write};

/// Alternate screen, hidden cursor, no autowrap, button and motion mouse
/// reports in SGR form, focus reports, and a pane title.
const ENTER: &str = "\x1b[?1049h\x1b[?25l\x1b[?7l\x1b[?1002h\x1b[?1003h\x1b[?1006h\x1b[?1004h\x1b]2;arbor\x1b\\";
const LEAVE: &str = "\x1b[?1004l\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?7h\x1b[?25h\x1b[0m\x1b[?1049l";

/// Asks the terminal (tmux, here) for its background colour.
pub const QUERY_BG: &str = "\x1b]11;?\x1b\\";

pub struct Raw {
    saved: Option<libc::termios>,
}

impl Raw {
    pub fn enter() -> io::Result<Raw> {
        // SAFETY: plain termios calls on stdin with a zeroed struct to fill.
        let saved = unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut t) == 0 {
                let orig = t;
                libc::cfmakeraw(&mut t);
                libc::tcsetattr(0, libc::TCSANOW, &t);
                Some(orig)
            } else {
                None
            }
        };
        write(ENTER)?;
        Ok(Raw { saved })
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        let _ = write(LEAVE);
        if let Some(t) = self.saved {
            // SAFETY: restores the attributes read in `enter`.
            unsafe {
                libc::tcsetattr(0, libc::TCSANOW, &t);
            }
        }
    }
}

pub fn write(s: &str) -> io::Result<()> {
    let mut out = io::stdout().lock();
    out.write_all(s.as_bytes())?;
    out.flush()
}

/// Columns and rows of the pane.
pub fn size() -> (u16, u16) {
    // SAFETY: TIOCGWINSZ fills a winsize struct.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 {
            return (ws.ws_col, ws.ws_row);
        }
    }
    (crate::config::DEFAULT_WIDTH, 24)
}
