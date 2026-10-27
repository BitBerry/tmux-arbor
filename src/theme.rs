//! Colours. The design uses one steel-blue accent on a neutral ramp: a light
//! fill says "you are here", a dashed line says "it will land here", and the
//! solid dark fill is the keyboard cursor. Rows sit on the terminal's own
//! background unless @arbor-bg sets one.

use crate::screen::Color;

#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    pub bg: Color,
    pub text: Color,
    pub muted: Color,
    pub guide: Color,
    pub divider: Color,
    pub accent: Color,
    pub cur_win_bg: Color,
    pub cur_win_fg: Color,
    pub cur_win_sub: Color,
    pub cur_pane_bg: Color,
    pub cur_pane_fg: Color,
    pub cur_pane_num: Color,
    pub cursor_bg: Color,
    pub cursor_fg: Color,
    pub cursor_sub: Color,
    pub drop_bg: Color,
    pub drop_fg: Color,
    pub drop_line: Color,
    pub hover_bg: Color,
    pub chip_bg: Color,
    pub chip_fg: Color,
    pub border: Color,
    pub border_cur: Color,
    pub dot: Color,
    pub dot_active: Color,
}

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

// The design system's ramps.
const N100: Color = rgb(0xf5f5f8);
const N200: Color = rgb(0xe7e7ea);
const N400: Color = rgb(0xb7b7ba);
const N500: Color = rgb(0x98989b);
const N600: Color = rgb(0x7a7a7d);
const N700: Color = rgb(0x5d5d60);
const N800: Color = rgb(0x424244);
const N900: Color = rgb(0x2b2b2d);
const A100: Color = rgb(0xeef6ff);
const A200: Color = rgb(0xd6ebff);
const A300: Color = rgb(0xb5d9fd);
const A400: Color = rgb(0x94bce3);
const A500: Color = rgb(0x749dc4);
const A600: Color = rgb(0x597ea3);
const A700: Color = rgb(0x416180);
const A800: Color = rgb(0x2c455d);
const A900: Color = rgb(0x1d2d3d);

pub fn light() -> Palette {
    Palette {
        bg: Color::Default,
        text: Color::Default,
        muted: N700,
        guide: N500,
        divider: N400,
        accent: A700,
        cur_win_bg: A200,
        cur_win_fg: A900,
        cur_win_sub: A800,
        cur_pane_bg: A100,
        cur_pane_fg: A800,
        cur_pane_num: A700,
        cursor_bg: A700,
        cursor_fg: N100,
        cursor_sub: N200,
        drop_bg: A100,
        drop_fg: A900,
        drop_line: A600,
        hover_bg: N200,
        chip_bg: N900,
        chip_fg: N100,
        border: N400,
        border_cur: A600,
        dot: N500,
        dot_active: A600,
    }
}

/// The same roles mirrored onto the dark end of each ramp.
pub fn dark() -> Palette {
    Palette {
        bg: Color::Default,
        text: Color::Default,
        muted: N500,
        guide: N600,
        divider: N800,
        accent: A400,
        cur_win_bg: A800,
        cur_win_fg: A100,
        cur_win_sub: A300,
        cur_pane_bg: A900,
        cur_pane_fg: A200,
        cur_pane_num: A400,
        cursor_bg: A600,
        cursor_fg: N100,
        cursor_sub: N200,
        drop_bg: A900,
        drop_fg: A100,
        drop_line: A500,
        hover_bg: N900,
        chip_bg: N200,
        chip_fg: N900,
        border: N700,
        border_cur: A500,
        dot: N600,
        dot_active: A500,
    }
}

/// Whether a background colour reads as light (relative luminance).
pub fn is_light(r: u8, g: u8, b: u8) -> bool {
    let lin = |c: u8| {
        let c = c as f64 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b) > 0.4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luminance() {
        assert!(is_light(0xff, 0xff, 0xff));
        assert!(is_light(0xf2, 0xf2, 0xf3));
        assert!(!is_light(0x1e, 0x1e, 0x1e));
        assert!(!is_light(0x28, 0x2c, 0x34));
    }
}
