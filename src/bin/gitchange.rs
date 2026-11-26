//! gitchange : ce qui n'est pas encore commité, fichier par fichier.

use std::process::ExitCode;

use gitlogs_tui::{Tool, launch};

fn main() -> ExitCode {
    launch(Tool::Changes)
}
