//! Lancement d'un outil de la suite : ses arguments, son aide, puis son écran
//! dans le terminal. Chaque binaire s'y réduit.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};

use crate::kit::Tool;
use crate::kit::git::Repo;
use crate::kit::term;
use crate::log::app::App;
use crate::log::keys;
use crate::merge;

const LOG_HELP: &str = "\
gitlog: interactive git graph

Usage: gitlog [OPTIONS] [PATH]

  PATH         repository folder (default: the current folder)
  --plain      standard glyphs, without a Powerline or Nerd Font
               (also: environment variable GITLOG_PLAIN=1)
  --no-syntax  diffs without syntax highlighting (it needs a 24-bit terminal)
  --dump N     print the first N rows of the graph as text, without the interface
  -h, --help   show this help

Keys: ↑↓ j k move · Enter open · q or Esc go back · 1 2 3 level
      y copy the commit hash, or the path of the file under the cursor
      drag the mouse over a diff to select code: it is copied on release
      / search the code of a diff, then n N next / previous match, Esc to clear
      Enter on a diff opens the whole file in a floating window
      n N next / previous commit (file list), file (diff), change (whole file)
      s search, then c hash · m message · f file name · h hunk · +h added · -h removed
      S commit style · R refresh · ? all shortcuts
      b then h branch hierarchy · f pick the branches of the graph
             o show or hide the remote branches (origin) · O remote branches only
      ←→ scroll the diff sideways · PgUp PgDn g G · Ctrl+C quit
      h jump to the HEAD commit in the graph, without opening it
      w wrap or cut the long lines, in the graph and in the code";

const CHANGES_HELP: &str = "\
gitchange: the changes of a git repository that are not committed yet

Usage: gitchange [OPTIONS] [PATH]

  PATH         repository folder (default: the current folder)
  --plain      standard glyphs, without a Powerline or Nerd Font
               (also: environment variable GITLOG_PLAIN=1)
  --no-syntax  diffs without syntax highlighting (it needs a 24-bit terminal)
  -h, --help   show this help

Keys: ↑↓ j k move · Enter open · Esc go back · q go back, quit from the list · 1 2 3 level
      y copy the path of the file under the cursor
      drag the mouse over a diff to select code: it is copied on release
      / search the code of a diff, then n N next / previous match, Esc to clear
      Enter on a diff opens the whole file in a floating window
      n N next / previous file (diff), change (whole file)
      R read the changes again · w wrap or cut the long lines · ? all shortcuts
      ←→ scroll the diff sideways · PgUp PgDn g G · Ctrl+C quit";

const MERGE_HELP: &str = "\
gitmerge: resolve the conflicted files of a merge, a rebase or a cherry-pick

Usage: gitmerge [OPTIONS] [PATH]

  PATH         repository folder (default: the current folder)
  --plain      standard glyphs, without a Powerline or Nerd Font
               (also: environment variable GITLOG_PLAIN=1)
  --no-syntax  code without syntax highlighting (it needs a 24-bit terminal)
  -h, --help   show this help

Three columns: ours on the left, theirs on the right, the result in the middle.
The changes of one side only are already in the result; a conflict waits.

Keys: ↑↓ j k move · ] [ or Tab Shift+Tab next / previous conflict · } { or c C change
      > take ours · < take theirs · the other one then adds its lines below
      x keep the result as it is · u undo · Ctrl+R redo
      i a I A o O or Enter edit the result, Esc back to normal mode · dd delete a line
      n N next / previous conflicted file
      Ctrl+S write the result to the file · S write it and stage it (git add)
      / search the result, then n N next / previous match, Esc to clear
      w wrap or cut the long lines · y copy the line · R read the conflicts again
      ? all shortcuts · q quit";

struct Args {
    dir: PathBuf,
    plain: bool,
    syntax: bool,
    dump: Option<usize>,
}

/// La coloration syntaxique emploie des couleurs 24 bits : tous les terminaux de
/// Windows 10 et suivants les affichent, les autres l'annoncent par `COLORTERM`.
fn truecolor() -> bool {
    cfg!(windows) || std::env::var("COLORTERM").is_ok_and(|v| v == "truecolor" || v == "24bit")
}

fn parse_args(tool: Tool) -> Result<Option<Args>> {
    let mut args = Args {
        dir: PathBuf::from("."),
        plain: std::env::var_os("GITLOG_PLAIN").is_some_and(|v| !v.is_empty() && v != "0"),
        syntax: truecolor(),
        dump: None,
    };
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--plain" => args.plain = true,
            "--no-syntax" => args.syntax = false,
            // Le graphe en texte : propre à gitlog.
            "--dump" if tool == Tool::Log => {
                let n = argv.next().and_then(|n| n.parse().ok());
                args.dump = Some(n.context("--dump expects a number of rows")?);
            }
            flag if flag.starts_with('-') => bail!("unknown option: {flag} (see --help)"),
            path => args.dir = PathBuf::from(path),
        }
    }
    Ok(Some(args))
}

/// Point d'entrée d'un outil : chaque binaire de la suite s'y réduit.
pub fn launch(tool: Tool) -> ExitCode {
    match run(tool) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{}: {err:#}", tool.name());
            ExitCode::FAILURE
        }
    }
}

fn run(tool: Tool) -> Result<()> {
    let Some(args) = parse_args(tool)? else {
        println!("{}", help(tool));
        return Ok(());
    };
    let repo = Repo::open(&args.dir)?;
    match tool {
        Tool::Log | Tool::Changes => {
            let mut app = App::open(repo, args.plain, tool)?;
            app.syntax = args.syntax;
            if let Some(rows) = args.dump {
                return keys::dump(&mut app, rows);
            }
            term::run(&mut app)
        }
        Tool::Merge => {
            let mut app = merge::app::App::open(repo, args.plain, args.syntax)?;
            // Rien à fusionner : inutile de prendre le terminal.
            if app.files.is_empty() {
                println!("No conflicted file in {}.", app.repo.name);
                return Ok(());
            }
            term::run(&mut app)
        }
    }
}

fn help(tool: Tool) -> &'static str {
    match tool {
        Tool::Log => LOG_HELP,
        Tool::Changes => CHANGES_HELP,
        Tool::Merge => MERGE_HELP,
    }
}
