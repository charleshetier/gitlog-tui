//! gitmerge : les conflits d'une fusion, sur trois colonnes.

use std::process::ExitCode;

use gitlogs_tui::{Tool, launch};

fn main() -> ExitCode {
    launch(Tool::Merge)
}
