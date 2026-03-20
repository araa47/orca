mod cli;
mod config;
mod daemon;
mod events;
mod names;
mod prompts;
mod spawn;
mod state;
mod tmux;
mod wake;
mod worktree;

fn main() {
    cli::run();
}
