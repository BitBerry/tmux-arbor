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

    pub fn panes(&self, n: usize) -> String {
        if n == 1 { "1 pane".into() } else { format!("{n} panes") }
    }

    pub fn panes_dirs(&self, p: usize, d: usize) -> String {
        format!("{p}p · {d}d")
    }

    pub fn hints_focused(&self) -> Vec<String> {
        let list: &[&str] = if self.zh {
            &["j k 移动", "h l 折叠", "⏎ 切换", "R 重命名", "c 新建", "J K 排序", "< > 宽度", "q 退出"]
        } else {
            &["j k move", "h l fold", "⏎ open", "R rename", "c new", "J K reorder", "< > width", "q leave"]
        };
        list.iter().map(|s| s.to_string()).collect()
    }

    pub fn hints_idle(&self, prefix: &str) -> Vec<String> {
        if self.zh {
            vec![format!("{prefix} T 键盘操作"), "拖动目录/pane 重排 · 双击或 ✎ 重命名".into()]
        } else {
            vec![format!("{prefix} T keyboard"), "drag to reorder · dblclick/✎ rename".into()]
        }
    }

    pub fn hints_rename(&self) -> Vec<String> {
        let list: &[&str] = if self.zh {
            &["⏎ 保存", "Esc 取消", "留空恢复默认"]
        } else {
            &["⏎ save", "Esc cancel", "empty resets"]
        };
        list.iter().map(|s| s.to_string()).collect()
    }

    pub fn rename_tip(&self) -> String {
        self.pick("重命名 (双击或按 R)", "rename (dblclick or R)")
    }

    pub fn renamed_to(&self, name: &str) -> String {
        if self.zh { format!("已重命名为 {name}") } else { format!("renamed to {name}") }
    }

    pub fn name_reset(&self) -> String {
        self.pick("已恢复默认名称", "name reset")
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

    pub fn join_pane(&self, name: &str, win_idx: u32) -> String {
        if self.zh { format!("与 {name} 合并分屏 (w{win_idx})") } else { format!("join {name} (w{win_idx})") }
    }

    pub fn dir_cwd_hint(&self) -> String {
        self.pick("目录由工作路径决定 · 拖至 pane 可合屏", "grouped by cwd · drop on a pane to join")
    }

    pub fn new_window_in(&self, label: &str) -> String {
        if self.zh { format!("在 {label} 新建 window") } else { format!("new window in {label}") }
    }

    pub fn at_dir_edge(&self) -> String {
        self.pick("已到达当前目录边界", "at directory boundary")
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

    pub fn cols(&self, n: u16) -> String {
        if self.zh { format!("{n} 列") } else { format!("{n} cols") }
    }

    pub fn release_hides(&self) -> String {
        self.pick("松开即隐藏", "release to hide")
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
