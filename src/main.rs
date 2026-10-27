//! arbor: a tree sidebar for tmux. One binary serves as the sidebar process
//! inside its pane and as the commands key bindings and hooks call.

mod cli;
mod config;
mod input;
mod label;
mod model;
mod screen;
mod sidebar;
mod status;
mod strings;
mod term;
mod theme;
mod tmux;
mod view;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = cli::run(&args) {
        eprintln!("arbor: {e:#}");
        std::process::exit(1);
    }
}
