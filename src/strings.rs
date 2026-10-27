//! User-facing text in the two supported languages.

use crate::config::{Lang, RAIL};
use crate::screen::width;

pub struct Strings {
    zh: bool,
}

impl Strings {
    pub fn new(lang: Lang) -> Self {
        Strings { zh: lang == Lang::Zh }
    }

    fn pick(&self, zh: &str, en: &str) -> String {
        if self.zh { zh.to_string() } else { en.to_string() }
    }

    pub fn windows(&self, n: usize) -> String {
        if n == 1 { "1 window".into() } else { format!("{n} windows") }
    }

    pub fn hints_focused(&self) -> Vec<String> {
        let list: &[&str] = if self.zh {
            &["j k 移动", "h l 折叠", "⏎ 切换", "J K 排序", "< > 宽度", "q 退出"]
        } else {
            &["j k move", "h l fold", "⏎ open", "J K reorder", "< > width", "q leave"]
        };
        list.iter().map(|s| s.to_string()).collect()
    }

    pub fn hints_idle(&self, prefix: &str) -> Vec<String> {
        if self.zh {
            vec![format!("{prefix} T 键盘操作"), "拖动节点可重排".into()]
        } else {
            vec![format!("{prefix} T keyboard"), "drag to rearrange".into()]
        }
    }

    pub fn before(&self, x: &str) -> String {
        if self.zh { format!("放到 {x} 之前") } else { format!("before {x}") }
    }

    pub fn after(&self, x: &str) -> String {
        if self.zh { format!("放到 {x} 之后") } else { format!("after {x}") }
    }

    pub fn swap(&self, x: &str) -> String {
        if self.zh { format!("与 {x} 交换位置") } else { format!("swap with {x}") }
    }

    pub fn join(&self, x: &str) -> String {
        if self.zh { format!("合并进 {x}") } else { format!("join {x}") }
    }

    pub fn break_out(&self) -> String {
        self.pick("拆成新 window", "break out to a new window")
    }

    pub fn new_window_zone(&self) -> String {
        self.pick("+ 拆成新 window", "+ new window")
    }

    pub fn cannot_drop(&self) -> String {
        self.pick("此处不能放置", "can't drop here")
    }

    pub fn cols_saved(&self, n: u16) -> String {
        if self.zh { format!("{n} 列 · 已记住") } else { format!("{n} cols · saved") }
    }

    pub fn rail(&self) -> String {
        if self.zh { format!("窄条 · {RAIL} 列") } else { format!("rail · {RAIL} cols") }
    }

    pub fn reset(&self, n: u16) -> String {
        if self.zh { format!("恢复默认 {n} 列") } else { format!("default {n} cols") }
    }

    /// The split buttons: split-window -h and -v.
    pub fn split_h(&self) -> String {
        self.pick("水平分割", "hsplit")
    }

    pub fn split_v(&self) -> String {
        self.pick("竖直分割", "vsplit")
    }

    pub fn no_space(&self) -> String {
        self.pick("当前 pane 太小", "pane too small to split")
    }

    pub fn side(&self, right: bool) -> String {
        match (self.zh, right) {
            (true, true) => "移到右侧".into(),
            (true, false) => "移到左侧".into(),
            (false, true) => "moved right".into(),
            (false, false) => "moved left".into(),
        }
    }
}

/// Greedily packs `items` into lines no wider than `max`, joined by " · ".
pub fn wrap(items: &[String], max: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for item in items {
        match lines.last_mut() {
            Some(line) if width(line) + 3 + width(item) <= max => {
                line.push_str(" · ");
                line.push_str(item);
            }
            _ => lines.push(item.clone()),
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_on_separators() {
        let items: Vec<String> = ["j k 移动", "h l 折叠", "⏎ 切换"].iter().map(|s| s.to_string()).collect();
        assert_eq!(wrap(&items, 40), vec!["j k 移动 · h l 折叠 · ⏎ 切换"]);
        assert_eq!(wrap(&items, 20), vec!["j k 移动 · h l 折叠", "⏎ 切换"]);
    }
}
