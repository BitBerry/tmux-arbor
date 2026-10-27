//! Thin wrapper over the tmux CLI. Every query and command goes through here.

use std::process::{Command, Stdio};
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};

/// Field separator for format queries; tmux passes it through verbatim.
pub const SEP: char = '\x1f';

fn program() -> String {
    std::env::var("ARBOR_TMUX").unwrap_or_else(|_| "tmux".to_string())
}

/// tmux treats a trailing `;` on any argument as a command separator, so a
/// value that happens to end in one is escaped.
fn escape(arg: &str) -> String {
    match arg.strip_suffix(';') {
        Some(head) if arg != ";" => format!("{head}\\;"),
        _ => arg.to_string(),
    }
}

/// Runs one tmux invocation. A lone `";"` argument separates commands.
pub fn run<I, S>(args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    exec(Command::new(program()), args)
}

/// Runs tmux from `cwd`. A detached client's own directory is where a new
/// pane without -c starts, so this makes it start where an attached client
/// would put it.
pub fn run_from<I, S>(cwd: &str, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut cmd = Command::new(program());
    if std::path::Path::new(cwd).is_dir() {
        cmd.current_dir(cwd);
    }
    exec(cmd, args)
}

fn exec<I, S>(mut cmd: Command, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args: Vec<String> = args
        .into_iter()
        .map(|a| {
            let a = a.as_ref();
            if a == ";" { a.to_string() } else { escape(a) }
        })
        .collect();
    let out = cmd.args(&args).stdin(Stdio::null()).stderr(Stdio::piped()).output().context("cannot run tmux")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("tmux {}: {}", args.first().map(String::as_str).unwrap_or(""), err.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Several commands sent in one tmux invocation.
#[derive(Default)]
pub struct Batch {
    args: Vec<String>,
}

impl Batch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cmd<I, S>(&mut self, parts: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        if !self.args.is_empty() {
            self.args.push(";".into());
        }
        self.args.extend(parts.into_iter().map(|s| s.as_ref().to_string()));
        self
    }

    pub fn run(&self) -> Result<String> {
        if self.args.is_empty() {
            return Ok(String::new());
        }
        run(&self.args)
    }
}

pub fn show_global(name: &str) -> Result<String> {
    let out = run(["show-options", "-gqv", name])?;
    Ok(out.strip_suffix('\n').unwrap_or(&out).to_string())
}

pub fn set_global(name: &str, value: &str) -> Result<()> {
    run(["set-option", "-g", name, value]).map(|_| ())
}

/// The server's version as (major, minor); unparsable builds count as new.
pub fn version() -> (u32, u32) {
    static VERSION: OnceLock<(u32, u32)> = OnceLock::new();
    *VERSION.get_or_init(|| {
        Command::new(program())
            .arg("-V")
            .output()
            .ok()
            .and_then(|o| parse_version(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or((99, 0))
    })
}

fn parse_version(s: &str) -> Option<(u32, u32)> {
    let start = s.find(|c: char| c.is_ascii_digit())?;
    let mut parts = s[start..].split(|c: char| !c.is_ascii_digit());
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    Some((major, minor))
}

/// Quotes a string for `sh`, for commands handed to `run-shell`.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("tmux 3.8\n"), Some((3, 8)));
        assert_eq!(parse_version("tmux 3.3a"), Some((3, 3)));
        assert_eq!(parse_version("tmux next-3.6"), Some((3, 6)));
        assert_eq!(parse_version("tmux master"), None);
    }

    #[test]
    fn trailing_semicolons_are_escaped() {
        assert_eq!(escape("a;"), "a\\;");
        assert_eq!(escape(";"), ";");
        assert_eq!(escape("a;b"), "a;b");
    }
}
