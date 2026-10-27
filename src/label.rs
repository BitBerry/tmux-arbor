//! Window labels are tmux's own window-status formats, already expanded, so
//! they may carry `#[...]` style directives. These are applied on top of the
//! row's colours, the way the status line would show them.

use crate::screen::{
    BLINK, BOLD, Color, DIM, HIDDEN, ITALIC, OVERLINE, REVERSE, STRIKE, Style, UNDER_ANY, UNDER_CURLY, UNDER_DASHED,
    UNDER_DOTTED, UNDER_DOUBLE, UNDERLINE,
};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Part {
    Text(String),
    Style(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Label {
    parts: Vec<Part>,
}

impl Label {
    pub fn parse(s: &str) -> Label {
        let mut parts = Vec::new();
        let mut buf = String::new();
        let mut i = 0;
        while i < s.len() {
            let rest = &s[i..];
            if rest.starts_with("##") {
                buf.push('#');
                i += 2;
                continue;
            }
            if let Some(body) = rest.strip_prefix("#[")
                && let Some(end) = body.find(']')
            {
                if !buf.is_empty() {
                    parts.push(Part::Text(std::mem::take(&mut buf)));
                }
                parts.push(Part::Style(body[..end].to_string()));
                i += 2 + end + 1;
                continue;
            }
            let ch = rest.chars().next().unwrap_or(' ');
            buf.push(ch);
            i += ch.len_utf8();
        }
        if !buf.is_empty() {
            parts.push(Part::Text(buf));
        }
        // tmux's default format pads flagless windows with a space for the
        // status line; in the tree it only gets in the way.
        if let Some(Part::Text(t)) = parts.last_mut() {
            t.truncate(t.trim_end().len());
            if t.is_empty() {
                parts.pop();
            }
        }
        Label { parts }
    }

    pub fn plain(&self) -> String {
        self.parts
            .iter()
            .filter_map(|p| match p {
                Part::Text(t) => Some(t.as_str()),
                Part::Style(_) => None,
            })
            .collect()
    }

    /// Text runs with their styles, starting from `base`. `default` in a
    /// directive goes back to `base`, so the row highlight survives it.
    pub fn spans(&self, base: Style) -> Vec<(String, Style)> {
        let mut cur = base;
        let mut out = Vec::new();
        for p in &self.parts {
            match p {
                Part::Text(t) => out.push((t.clone(), cur)),
                Part::Style(d) => apply(&mut cur, base, d),
            }
        }
        out
    }
}

fn attr(name: &str) -> Option<u16> {
    Some(match name {
        "bright" | "bold" => BOLD,
        "dim" => DIM,
        "underscore" => UNDERLINE,
        "double-underscore" => UNDER_DOUBLE,
        "curly-underscore" => UNDER_CURLY,
        "dotted-underscore" => UNDER_DOTTED,
        "dashed-underscore" => UNDER_DASHED,
        "blink" => BLINK,
        "reverse" => REVERSE,
        "hidden" => HIDDEN,
        "italics" => ITALIC,
        "strikethrough" => STRIKE,
        "overline" => OVERLINE,
        _ => return None,
    })
}

/// Applies one `#[...]` directive. Layout directives (align, list, range,
/// push-default, ...) mean nothing in a tree row and are skipped.
pub fn apply(cur: &mut Style, base: Style, directive: &str) {
    for tok in directive.split(|c: char| c == ',' || c.is_whitespace()).filter(|t| !t.is_empty()) {
        let tok = tok.to_ascii_lowercase();
        if tok == "default" {
            *cur = base;
        } else if tok == "none" {
            cur.attrs = 0;
        } else if let Some(v) = tok.strip_prefix("fg=") {
            if let Some(c) = parse_color(v) {
                cur.fg = if c == Color::Default { base.fg } else { c };
            }
        } else if let Some(v) = tok.strip_prefix("bg=") {
            if let Some(c) = parse_color(v) {
                cur.bg = if c == Color::Default { base.bg } else { c };
            }
        } else if let Some(v) = tok.strip_prefix("us=") {
            if let Some(c) = parse_color(v) {
                cur.us = c;
            }
        } else if let Some(a) = attr(&tok) {
            if a & UNDER_ANY != 0 {
                cur.attrs &= !UNDER_ANY;
            }
            cur.attrs |= a;
        } else if let Some(a) = tok.strip_prefix("no").and_then(attr) {
            cur.attrs &= !a;
        }
    }
}

pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim().to_ascii_lowercase();
    const NAMES: [&str; 8] = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];
    if s == "default" || s == "terminal" {
        return Some(Color::Default);
    }
    if let Some(i) = NAMES.iter().position(|n| *n == s) {
        return Some(Color::Idx(i as u8));
    }
    if let Some(i) = s.strip_prefix("bright").and_then(|n| NAMES.iter().position(|m| *m == n)) {
        return Some(Color::Idx(i as u8 + 8));
    }
    if let Some(n) = s.strip_prefix("colour").or_else(|| s.strip_prefix("color")) {
        return n.parse::<u8>().ok().map(Color::Idx);
    }
    if let Some(hex) = s.strip_prefix('#')
        && hex.len() == 6
        && let Ok(v) = u32::from_str_radix(hex, 16)
    {
        return Some(Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_text_and_styles() {
        let l = Label::parse("#[fg=red,bold]1#[default]:editor##*");
        assert_eq!(l.plain(), "1:editor#*");
        let base = Style::default().bg(Color::Idx(4));
        let spans = l.spans(base);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].1.fg, Color::Idx(1));
        assert_eq!(spans[0].1.bg, Color::Idx(4));
        assert_ne!(spans[0].1.attrs & BOLD, 0);
        assert_eq!(spans[1].1, base);
    }

    #[test]
    fn layout_directives_are_ignored() {
        let l = Label::parse("#[range=window|1 list=focus align=left]x#[norange]");
        let spans = l.spans(Style::default());
        assert_eq!(spans, vec![("x".to_string(), Style::default())]);
    }

    #[test]
    fn trailing_padding_is_dropped() {
        assert_eq!(Label::parse("2:server ").plain(), "2:server");
        assert_eq!(Label::parse("#[bold]2:a #[default] ").plain(), "2:a ");
    }

    #[test]
    fn unterminated_directive_is_text() {
        assert_eq!(Label::parse("a#[b").plain(), "a#[b");
    }

    #[test]
    fn colors() {
        assert_eq!(parse_color("colour235"), Some(Color::Idx(235)));
        assert_eq!(parse_color("brightred"), Some(Color::Idx(9)));
        assert_eq!(parse_color("#5980A6"), Some(Color::Rgb(0x59, 0x80, 0xa6)));
        assert_eq!(parse_color("nonsense"), None);
    }

    #[test]
    fn no_prefix_clears_attributes() {
        let mut s = Style::default().with(BOLD | ITALIC);
        apply(&mut s, Style::default(), "nobold");
        assert_eq!(s.attrs, ITALIC);
    }
}
