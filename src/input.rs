//! Decodes what tmux writes to the sidebar pane: keys, SGR mouse reports,
//! focus changes and the reply to the background-colour query.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Esc,
    Tab,
    Backspace,
    Up,
    Down,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseKind {
    Press(u8),
    Release,
    Drag(u8),
    Move,
    WheelUp,
    WheelDown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mouse {
    pub kind: MouseKind,
    /// Zero-based cell position.
    pub x: u16,
    pub y: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Key(Key),
    Mouse(Mouse),
    FocusIn,
    FocusOut,
    /// Terminal background, from an OSC 11 reply.
    Background(u8, u8, u8),
}

#[derive(Default)]
pub struct Parser {
    buf: Vec<u8>,
}

enum Step {
    Event(Event, usize),
    Skip(usize),
    Incomplete,
}

impl Parser {
    pub fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// A lone ESC is either the Esc key or the start of a sequence; the
    /// caller waits briefly, then calls `flush_esc`.
    pub fn pending_esc(&self) -> bool {
        self.buf == [0x1b]
    }

    pub fn flush_esc(&mut self) -> Option<Event> {
        if self.pending_esc() {
            self.buf.clear();
            return Some(Event::Key(Key::Esc));
        }
        None
    }

    pub fn next(&mut self) -> Option<Event> {
        loop {
            match step(&self.buf) {
                Step::Event(ev, n) => {
                    self.buf.drain(..n);
                    return Some(ev);
                }
                Step::Skip(n) => {
                    self.buf.drain(..n);
                }
                Step::Incomplete => {
                    // Garbage that never terminates should not wedge input.
                    if self.buf.len() > 4096 {
                        self.buf.clear();
                    }
                    return None;
                }
            }
        }
    }
}

fn step(b: &[u8]) -> Step {
    let Some(&first) = b.first() else { return Step::Incomplete };
    match first {
        0x1b => escape(b),
        b'\r' | b'\n' => Step::Event(Event::Key(Key::Enter), 1),
        b'\t' => Step::Event(Event::Key(Key::Tab), 1),
        0x7f | 0x08 => Step::Event(Event::Key(Key::Backspace), 1),
        c if c < 0x20 => Step::Skip(1),
        _ => {
            let len = match first {
                0xc0..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf7 => 4,
                0x80..=0xbf | 0xf8..=0xff => return Step::Skip(1),
                _ => 1,
            };
            if b.len() < len {
                return Step::Incomplete;
            }
            match std::str::from_utf8(&b[..len]).ok().and_then(|s| s.chars().next()) {
                Some(ch) => Step::Event(Event::Key(Key::Char(ch)), len),
                None => Step::Skip(1),
            }
        }
    }
}

fn escape(b: &[u8]) -> Step {
    let Some(&kind) = b.get(1) else { return Step::Incomplete };
    match kind {
        b'[' => csi(b),
        b'O' => {
            let Some(&c) = b.get(2) else { return Step::Incomplete };
            match arrow(c) {
                Some(k) => Step::Event(Event::Key(k), 3),
                None => Step::Skip(3),
            }
        }
        b']' | b'P' | b'_' => string(b),
        // ESC followed by anything else: the Esc key, then that key.
        _ => Step::Event(Event::Key(Key::Esc), 1),
    }
}

fn arrow(c: u8) -> Option<Key> {
    Some(match c {
        b'A' => Key::Up,
        b'B' => Key::Down,
        b'C' => Key::Right,
        b'D' => Key::Left,
        _ => return None,
    })
}

fn csi(b: &[u8]) -> Step {
    let Some(end) = b[2..].iter().position(|c| (0x40..=0x7e).contains(c)).map(|i| i + 2) else {
        return Step::Incomplete;
    };
    let params = &b[2..end];
    let fin = b[end];
    let n = end + 1;
    if params.first() == Some(&b'<') && (fin == b'M' || fin == b'm') {
        return match sgr_mouse(&params[1..], fin == b'M') {
            Some(m) => Step::Event(Event::Mouse(m), n),
            None => Step::Skip(n),
        };
    }
    match fin {
        b'I' if params.is_empty() => Step::Event(Event::FocusIn, n),
        b'O' if params.is_empty() => Step::Event(Event::FocusOut, n),
        c => match arrow(c) {
            Some(k) => Step::Event(Event::Key(k), n),
            None => Step::Skip(n),
        },
    }
}

fn sgr_mouse(params: &[u8], press: bool) -> Option<Mouse> {
    let s = std::str::from_utf8(params).ok()?;
    let mut it = s.split(';').map(|p| p.parse::<u16>());
    let (b, x, y) = (it.next()?.ok()?, it.next()?.ok()?, it.next()?.ok()?);
    let button = (b & 3) as u8;
    let kind = if b & 64 != 0 {
        match button {
            0 => MouseKind::WheelUp,
            1 => MouseKind::WheelDown,
            _ => return None,
        }
    } else if !press {
        MouseKind::Release
    } else if b & 32 != 0 {
        if button == 3 { MouseKind::Move } else { MouseKind::Drag(button) }
    } else {
        MouseKind::Press(button)
    };
    Some(Mouse { kind, x: x.saturating_sub(1), y: y.saturating_sub(1) })
}

/// OSC, DCS and APC strings, terminated by BEL or ST.
fn string(b: &[u8]) -> Step {
    let mut i = 2;
    let (body_end, n) = loop {
        match b.get(i) {
            None => return Step::Incomplete,
            Some(0x07) => break (i, i + 1),
            Some(0x1b) => match b.get(i + 1) {
                None => return Step::Incomplete,
                Some(b'\\') => break (i, i + 2),
                Some(_) => break (i, i + 1),
            },
            Some(_) => i += 1,
        }
    };
    if b[1] == b']'
        && let Some((r, g, bl)) = background(&b[2..body_end])
    {
        return Step::Event(Event::Background(r, g, bl), n);
    }
    Step::Skip(n)
}

/// Parses `11;rgb:RRRR/GGGG/BBBB` (1 to 4 hex digits per channel).
fn background(body: &[u8]) -> Option<(u8, u8, u8)> {
    let s = std::str::from_utf8(body).ok()?;
    let rgb = s.strip_prefix("11;rgb:")?;
    let mut ch = rgb.split('/').map(|c| {
        let c = c.get(..c.len().min(4))?;
        let v = u32::from_str_radix(c, 16).ok()?;
        let max = (1u32 << (4 * c.len() as u32)) - 1;
        Some((v * 255 / max.max(1)) as u8)
    });
    Some((ch.next()??, ch.next()??, ch.next()??))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(bytes: &[u8]) -> Vec<Event> {
        let mut p = Parser::default();
        p.feed(bytes);
        std::iter::from_fn(|| p.next()).collect()
    }

    #[test]
    fn keys() {
        assert_eq!(
            all(b"jK\r\x1b[A\x1bOB<"),
            vec![
                Event::Key(Key::Char('j')),
                Event::Key(Key::Char('K')),
                Event::Key(Key::Enter),
                Event::Key(Key::Up),
                Event::Key(Key::Down),
                Event::Key(Key::Char('<')),
            ]
        );
        assert_eq!(all("é".as_bytes()), vec![Event::Key(Key::Char('é'))]);
    }

    #[test]
    fn lone_escape_waits() {
        let mut p = Parser::default();
        p.feed(b"\x1b");
        assert_eq!(p.next(), None);
        assert!(p.pending_esc());
        assert_eq!(p.flush_esc(), Some(Event::Key(Key::Esc)));
    }

    #[test]
    fn mouse() {
        let ev = all(b"\x1b[<0;5;3M\x1b[<32;6;3M\x1b[<0;6;3m\x1b[<35;1;1M\x1b[<65;2;2M");
        let kinds: Vec<MouseKind> = ev
            .iter()
            .map(|e| match e {
                Event::Mouse(m) => m.kind,
                _ => panic!("{e:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![MouseKind::Press(0), MouseKind::Drag(0), MouseKind::Release, MouseKind::Move, MouseKind::WheelDown]
        );
        assert_eq!(ev[0], Event::Mouse(Mouse { kind: MouseKind::Press(0), x: 4, y: 2 }));
    }

    #[test]
    fn focus_and_background() {
        assert_eq!(all(b"\x1b[I\x1b[O"), vec![Event::FocusIn, Event::FocusOut]);
        assert_eq!(all(b"\x1b]11;rgb:ffff/8080/0000\x1b\\"), vec![Event::Background(255, 128, 0)]);
        assert_eq!(all(b"\x1b]11;rgb:1e/1e/1e\x07"), vec![Event::Background(0x1e, 0x1e, 0x1e)]);
    }

    #[test]
    fn partial_sequences_wait() {
        let mut p = Parser::default();
        p.feed(b"\x1b[<0;5");
        assert_eq!(p.next(), None);
        p.feed(b";3M");
        assert!(matches!(p.next(), Some(Event::Mouse(_))));
    }
}
