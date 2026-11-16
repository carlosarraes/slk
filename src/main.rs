mod cli;
mod client;
mod commands;
mod config;
mod curl;
mod error;
mod output;
mod permalink;
mod render;
mod resolve;
mod time;

fn main() {
    std::process::exit(cli::run());
}
