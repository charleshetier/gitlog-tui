//! gitlog : le graphe interactif des commits.

use std::process::ExitCode;

use gitlogs_tui::{Tool, launch};

fn main() -> ExitCode {
    launch(Tool::Log)
}
