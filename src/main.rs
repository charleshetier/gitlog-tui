//! gitlog : graphe git interactif dans le terminal.

mod app;
mod diff;
mod git;
mod graph;
#[cfg(test)]
mod tests;
mod ui;

use std::io::{self, Stdout};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::Position;

use crate::app::{Action, App};
use crate::git::Repo;

const HELP: &str = "\
gitlog : graphe git interactif

Usage : gitlog [OPTIONS] [CHEMIN]

  CHEMIN       dossier du dépôt (par défaut : le dossier courant)
  --plain      glyphes standard, sans police Powerline ni Nerd Font
               (aussi : variable d'environnement GITLOG_PLAIN=1)
  --dump N     écrit les N premières lignes du graphe en texte, sans interface
  -h, --help   affiche cette aide

Touches : ↑↓ j k naviguer · Entrée ouvrir · q ou Échap remonter · 1 2 3 niveau
          y copier le hash · PgUp PgDn g G · Ctrl+C quitter";

/// Durée d'attente d'un événement tant qu'un travail de fond est en cours.
const TICK: Duration = Duration::from_millis(16);
/// Lignes défilées par cran de molette.
const WHEEL: isize = 3;

struct Args {
    dir: PathBuf,
    plain: bool,
    dump: Option<usize>,
}

fn parse_args() -> Result<Option<Args>> {
    let mut args = Args {
        dir: PathBuf::from("."),
        plain: std::env::var_os("GITLOG_PLAIN").is_some_and(|v| !v.is_empty() && v != "0"),
        dump: None,
    };
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--plain" => args.plain = true,
            "--dump" => {
                let n = argv.next().and_then(|n| n.parse().ok());
                args.dump = Some(n.context("--dump attend un nombre de lignes")?);
            }
            flag if flag.starts_with('-') => bail!("option inconnue : {flag} (voir --help)"),
            path => args.dir = PathBuf::from(path),
        }
    }
    Ok(Some(args))
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("gitlog : {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let Some(args) = parse_args()? else {
        println!("{HELP}");
        return Ok(());
    };
    let repo = Repo::open(&args.dir)?;
    let mut app = App::new(repo, args.plain)?;
    if let Some(rows) = args.dump {
        return dump(&mut app, rows);
    }

    let mut terminal = init_terminal().inspect_err(|_| restore_terminal())?;
    let result = event_loop(&mut terminal, &mut app);
    restore_terminal();
    result
}

fn init_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    // Le terminal est rendu même si le programme panique.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        hook(info);
    }));
    enable_raw_mode().context("terminal non interactif")?;
    execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    Ok(Terminal::new(CrosstermBackend::new(io::stdout()))?)
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
}

fn event_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    let mut dirty = true;
    loop {
        dirty |= app.tick();
        if dirty {
            terminal.draw(|frame| ui::draw(frame, app))?;
            dirty = false;
        }
        if app.should_quit {
            return Ok(());
        }
        // Sans travail de fond, on dort jusqu'au prochain événement : aucun redessin inutile.
        if app.busy() && !event::poll(TICK)? {
            continue;
        }
        // Une rafale d'événements (molette, répétition clavier) donne un seul redessin.
        loop {
            dirty |= handle_event(app, event::read()?);
            if !event::poll(Duration::ZERO)? {
                break;
            }
        }
    }
}

fn handle_event(app: &mut App, event: Event) -> bool {
    let action = match event {
        Event::Key(key) if key.kind != KeyEventKind::Release => on_key(key),
        Event::Mouse(mouse) => on_mouse(app, mouse),
        Event::Resize(..) => return true,
        _ => None,
    };
    match action {
        Some(action) => {
            app.apply(action);
            true
        }
        None => false,
    }
}

fn on_key(key: KeyEvent) -> Option<Action> {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return (key.code == KeyCode::Char('c')).then_some(Action::Quit);
    }
    Some(match key.code {
        KeyCode::Char('q') => Action::Back { quit: true },
        KeyCode::Esc => Action::Back { quit: false },
        KeyCode::Up | KeyCode::Char('k') => Action::Move(-1),
        KeyCode::Down | KeyCode::Char('j') => Action::Move(1),
        KeyCode::PageUp => Action::Page(-1),
        KeyCode::PageDown => Action::Page(1),
        KeyCode::Home | KeyCode::Char('g') => Action::Top,
        KeyCode::End | KeyCode::Char('G') => Action::Bottom,
        KeyCode::Enter => Action::Open,
        KeyCode::Left | KeyCode::Char('h') => Action::Horizontal(-4),
        KeyCode::Right | KeyCode::Char('l') => Action::Horizontal(4),
        KeyCode::Char('y') => Action::CopySelected,
        KeyCode::Char(c @ '1'..='3') => Action::GoLevel(c as u8 - b'0'),
        _ => return None,
    })
}

/// Traduit un clic ou la molette en action, d'après les zones du dernier rendu.
fn on_mouse(app: &App, mouse: MouseEvent) -> Option<Action> {
    let hit = &app.hit;
    let pos = Position::new(mouse.column, mouse.row);
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some((_, level)) = hit.chips.iter().find(|(rect, _)| rect.contains(pos)) {
                return Some(Action::GoLevel(*level));
            }
            if hit.pane_copy.is_some_and(|rect| rect.contains(pos)) {
                return Some(Action::CopySelected);
            }
            if hit.graph.contains(pos) {
                let row = hit.graph_top + (mouse.row - hit.graph.y) as usize;
                if row >= app.store.len() {
                    return None;
                }
                let on_icon = hit.icon_x.is_some_and(|x| mouse.column.abs_diff(x) <= 1);
                return Some(if on_icon { Action::CopyRow(row) } else { Action::ClickCommit(row) });
            }
            if hit.files.contains(pos) {
                return Some(Action::ClickFile(hit.file_top + (mouse.row - hit.files.y) as usize));
            }
            None
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let delta = if mouse.kind == MouseEventKind::ScrollUp { -WHEEL } else { WHEEL };
            if hit.pane.contains(pos) {
                Some(Action::ScrollPane(delta))
            } else if hit.graph.contains(pos) {
                Some(Action::ScrollGraph(delta))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Écrit les premières lignes du graphe en texte : utile pour vérifier un dépôt sans interface.
fn dump(app: &mut App, rows: usize) -> Result<()> {
    app.graph_height = rows;
    while app.store.len() < rows && !app.log_done {
        if !app.tick() {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    if let Some(err) = &app.log_error {
        bail!("{err}");
    }
    let count = rows.min(app.store.len());
    let graph_w = (0..count).map(|row| app.store.cells(row).len()).max().unwrap_or(0);
    for row in 0..count {
        let graph: String = app.store.cells(row).iter().map(|cell| cell.glyph()).collect();
        let oid = app.store.oid(row);
        let labels: String = app
            .labels
            .get(&oid)
            .map(|labels| labels.iter().map(|l| format!("[{}] ", l.name)).collect())
            .unwrap_or_default();
        let meta = app.meta(row);
        println!("{graph:<graph_w$}  {}  {labels}{}", oid.short(), meta.subject);
    }
    Ok(())
}
