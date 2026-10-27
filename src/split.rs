//! Splitting from the tree. The user's own split-window bindings are read
//! with list-keys, so the tree's split buttons show their keys, and those
//! keys are wrapped so that while the tree has the keyboard they split the
//! work pane rather than the sidebar.

use anyhow::{Context, Result, bail};

use crate::cli;
use crate::tmux::{self, Batch, shell_quote};

/// Separators inside @arbor-splits; neither appears in key bindings.
const RECORD: char = '\x1e';
const FIELD: char = '\x1d';

/// One prefix-table binding that runs split-window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    /// 'h' (side by side) or 'v' (stacked).
    pub dir: char,
    pub repeat: bool,
    /// The key as list-keys prints it, in config syntax (`\%`).
    pub key: String,
    /// The command as list-keys prints it.
    pub cmd: String,
}

impl Binding {
    /// The key as typed: for display and for bind-key's arguments.
    pub fn label(&self) -> String {
        unescape_key(&self.key)
    }
}

fn unescape_key(k: &str) -> String {
    if let Some(rest) = k.strip_prefix('\\')
        && rest.chars().count() == 1
    {
        return rest.to_string();
    }
    let quoted = |q: char| k.len() >= 2 && k.starts_with(q) && k.ends_with(q);
    if quoted('\'') || quoted('"') {
        return k[1..k.len() - 1].to_string();
    }
    k.to_string()
}

/// Splits a `list-keys` line into (table, repeat, key, command).
fn parse_line(line: &str) -> Option<(String, bool, String, String)> {
    let mut rest = line.trim_start().strip_prefix("bind-key")?;
    let mut table = "prefix".to_string();
    let mut repeat = false;
    loop {
        rest = rest.trim_start();
        let end = rest.find(char::is_whitespace)?;
        match &rest[..end] {
            "-r" => repeat = true,
            "-n" => table = "root".into(),
            "-T" => {
                rest = rest[end..].trim_start();
                let e = rest.find(char::is_whitespace)?;
                table = rest[..e].to_string();
                rest = &rest[e..];
                continue;
            }
            key => return Some((table, repeat, key.to_string(), rest[end..].trim().to_string())),
        }
        rest = &rest[end..];
    }
}

/// Splits a single tmux command into its arguments. Returns None for
/// command lists and blocks, which are left alone.
pub fn split_args(s: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        ch => cur.push(ch),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => cur.push(chars.next()?),
                        ch => cur.push(ch),
                    }
                }
            }
            '\\' => {
                in_word = true;
                cur.push(chars.next()?);
            }
            _ => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        out.push(cur);
    }
    if out.iter().any(|a| a == ";" || a == "{" || a == "}") {
        return None;
    }
    Some(out)
}

/// The direction of a lone split-window command.
fn split_dir(cmd: &str) -> Option<char> {
    let args = split_args(cmd)?;
    let (name, rest) = args.split_first()?;
    if name != "split-window" && name != "splitw" {
        return None;
    }
    let mut horizontal = false;
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        let Some(flags) = a.strip_prefix('-').filter(|f| !f.is_empty() && *f != "-") else { break };
        for (i, f) in flags.char_indices() {
            if f == 'h' {
                horizontal = true;
            }
            // Flags taking a value consume the rest of the group or the
            // next argument.
            if "celtF".contains(f) {
                if i + 1 == flags.len() {
                    it.next();
                }
                break;
            }
        }
    }
    Some(if horizontal { 'h' } else { 'v' })
}

fn encode(list: &[Binding]) -> String {
    list.iter()
        .map(|b| {
            [b.dir.to_string(), (b.repeat as u8).to_string(), b.key.clone(), b.cmd.clone()].join(&FIELD.to_string())
        })
        .collect::<Vec<_>>()
        .join(&RECORD.to_string())
}

fn decode(s: &str) -> Vec<Binding> {
    s.split(RECORD)
        .filter_map(|r| {
            let f: Vec<&str> = r.splitn(4, FIELD).collect();
            let [dir, repeat, key, cmd] = f[..] else { return None };
            Some(Binding { dir: dir.chars().next()?, repeat: repeat == "1", key: key.into(), cmd: cmd.into() })
        })
        .collect()
}

fn saved() -> Vec<Binding> {
    decode(&tmux::show_global("@arbor-splits").unwrap_or_default())
}

/// Collects the prefix split bindings from a list-keys listing. Keys we
/// already wrapped take their original binding from `saved`.
fn collect(listing: &str, saved: &[Binding]) -> Vec<Binding> {
    let mut out = Vec::new();
    for line in listing.lines() {
        let Some((table, repeat, key, cmd)) = parse_line(line) else { continue };
        if table != "prefix" {
            continue;
        }
        if cmd.contains("@arbor-sidebar") {
            out.extend(saved.iter().find(|b| b.key == key).cloned());
        } else if let Some(dir) = split_dir(&cmd) {
            out.push(Binding { dir, repeat, key, cmd });
        }
    }
    out
}

/// Wraps every split binding: in the sidebar it calls `arbor split`,
/// elsewhere it runs the original command untouched.
pub fn install(bin: &str) -> Result<()> {
    let listing = tmux::run(["list-keys", "-T", "prefix"]).unwrap_or_default();
    let list = collect(&listing, &saved());
    let key_for = |dir: char| list.iter().find(|b| b.dir == dir).map(Binding::label).unwrap_or_default();
    let mut b = Batch::new();
    b.cmd(["set-option", "-g", "@arbor-splits", &encode(&list)]);
    b.cmd(["set-option", "-g", "@arbor-split-h-key", &key_for('h')]);
    b.cmd(["set-option", "-g", "@arbor-split-v-key", &key_for('v')]);
    for (i, s) in list.iter().enumerate() {
        let label = s.label();
        // A lone ";" would read as a command separator.
        if label == ";" {
            continue;
        }
        let wrapped = format!("run-shell -b \"{} split {i} '#{{pane_id}}'\"", shell_quote(bin));
        let mut args = vec!["bind-key"];
        if s.repeat {
            args.push("-r");
        }
        args.extend(["-T", "prefix", &label, "if-shell", "-F", "#{@arbor-sidebar}", &wrapped, &s.cmd]);
        b.cmd(args);
    }
    b.run()?;
    Ok(())
}

/// Puts the original bindings back.
pub fn uninstall() -> Result<()> {
    let list = saved();
    if !list.is_empty() {
        // The saved lines are config syntax; let tmux parse them.
        let conf: String = list
            .iter()
            .map(|b| format!("bind-key {}-T prefix {} {}\n", if b.repeat { "-r " } else { "" }, b.key, b.cmd))
            .collect();
        let path = std::env::temp_dir().join(format!("arbor-splits-{}.conf", std::process::id()));
        std::fs::write(&path, conf)?;
        let r = tmux::run(["source-file", &path.to_string_lossy()]);
        let _ = std::fs::remove_file(&path);
        r?;
    }
    for k in ["@arbor-splits", "@arbor-split-h-key", "@arbor-split-v-key"] {
        let _ = tmux::run(["set-option", "-gu", k]);
    }
    Ok(())
}

/// Splits the work pane next to `sidebar` with the user's binding: `which`
/// is a binding index (from a wrapped key) or a direction (a button).
pub fn split(which: &str, sidebar: &str) -> Result<()> {
    let list = saved();
    let binding = match which {
        "h" | "v" => list.iter().find(|b| which.starts_with(b.dir)),
        n => list.get(n.parse::<usize>().context("bad split")?),
    };
    let default = || vec!["split-window".to_string(), format!("-{}", if which == "h" { 'h' } else { 'v' })];
    let mut args = binding.and_then(|b| split_args(&b.cmd)).unwrap_or_else(default);
    if args.first().is_none_or(|a| a != "split-window" && a != "splitw") {
        bail!("not a split: {}", args.join(" "));
    }
    let rows = cli::window_rows(sidebar)?;
    let window = rows.iter().find(|r| r.pane == sidebar).map(|r| r.window.clone()).context("sidebar gone")?;
    let work = cli::work_pane(&rows, &window).context("no pane to split")?;
    if !args.iter().any(|a| a == "-t") {
        args.splice(1..1, ["-t".to_string(), work.pane.clone()]);
    }
    // tmux expands -c formats such as #{pane_current_path} against the
    // active pane, which is the sidebar while the tree has the keyboard.
    // The new pane takes focus anyway, so select the work pane first.
    let mut cmd = vec!["select-pane".to_string(), "-t".into(), work.pane.clone(), ";".into()];
    cmd.extend(args);
    let cwd = tmux::run(["display-message", "-p", "-t", &work.pane, "#{session_path}"])?;
    tmux::run_from(cwd.trim(), &cmd)?;
    Ok(())
}

/// `arbor split`, from a wrapped key: errors go to the status line, since
/// nothing else would show them.
pub fn run(which: Option<&str>, sidebar: Option<&str>) -> Result<()> {
    let (Some(which), Some(sidebar)) = (which, sidebar) else { bail!("usage: arbor split <h|v|index> <pane>") };
    if let Err(e) = split(which, sidebar) {
        let msg = format!("{e:#}");
        let msg = msg.rsplit(": ").next().unwrap_or(&msg).to_string();
        let _ = tmux::run(["display-message", &msg]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str = r##"bind-key    -T prefix \"      split-window
bind-key    -T prefix \%      split-window -h
bind-key    -T prefix >       display-menu -T "x" h { split-window -h } v { split-window -v }
bind-key    -T prefix |       split-window -h -c "#{pane_current_path}"
bind-key -r -T prefix M-v     split-window -v -l "30%"
bind-key    -T prefix c       new-window
bind-key    -T root   C-s     split-window -h
"##;

    #[test]
    fn finds_split_bindings() {
        let list = collect(LISTING, &[]);
        let summary: Vec<(char, String, bool)> = list.iter().map(|b| (b.dir, b.label(), b.repeat)).collect();
        assert_eq!(
            summary,
            vec![
                ('v', "\"".into(), false),
                ('h', "%".into(), false),
                ('h', "|".into(), false),
                ('v', "M-v".into(), true)
            ]
        );
        assert_eq!(list[2].cmd, r##"split-window -h -c "#{pane_current_path}""##);
    }

    #[test]
    fn wrapped_keys_keep_their_original() {
        let saved = vec![Binding { dir: 'h', repeat: false, key: "\\%".into(), cmd: "split-window -h".into() }];
        let listing = r##"bind-key    -T prefix \%      if-shell -F "#{@arbor-sidebar}" "run-shell -b \"'arbor' split 0 '#{pane_id}'\"" "split-window -h""##;
        assert_eq!(collect(listing, &saved), saved);
    }

    #[test]
    fn arguments() {
        assert_eq!(
            split_args(r##"split-window -h -c "#{pane_current_path}" 'a b'"##).unwrap(),
            vec!["split-window", "-h", "-c", "#{pane_current_path}", "a b"]
        );
        assert_eq!(split_args(r"split-window \; select-layout"), None);
        assert_eq!(split_dir("split-window -c -h"), Some('v'));
        assert_eq!(split_dir("split-window -bh"), Some('h'));
        assert_eq!(split_dir("splitw -fh -l 30%"), Some('h'));
        assert_eq!(split_dir("new-window"), None);
    }

    #[test]
    fn round_trips() {
        let list = collect(LISTING, &[]);
        assert_eq!(decode(&encode(&list)), list);
        assert_eq!(decode(""), vec![]);
    }
}
