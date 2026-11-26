//! Clavier et souris de `gitlog` et de `gitchange` : chaque touche, chaque geste
//! devient une action de l'application.

use std::time::Duration;

use anyhow::{Result, bail};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Position;

use crate::kit::term::Screen;
use crate::log::app::{Action, App};
use crate::log::ui;

/// Lignes défilées par cran de molette.
const WHEEL: isize = 3;

impl Screen for App {
    fn tick(&mut self) -> bool {
        App::tick(self)
    }

    fn busy(&self) -> bool {
        App::busy(self)
    }

    fn on_key(&mut self, key: KeyEvent) -> bool {
        on_key(self, key).map(|action| self.apply(action)).is_some()
    }

    fn on_mouse(&mut self, mouse: MouseEvent) -> bool {
        on_mouse(self, mouse).map(|action| self.apply(action)).is_some()
    }

    fn draw(&mut self, frame: &mut Frame) {
        ui::draw(frame, self);
    }

    fn should_quit(&self) -> bool {
        self.should_quit
    }
}

pub fn on_key(app: &App, key: KeyEvent) -> Option<Action> {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return (key.code == KeyCode::Char('c')).then_some(Action::Quit);
    }
    // La liste des branches reçoit les lettres telles quelles, et les touches de déplacement.
    if app.picker.is_some() && !app.help {
        return Some(match key.code {
            KeyCode::Char(c) => Action::PromptKey(c),
            KeyCode::Backspace => Action::PromptBackspace,
            KeyCode::Enter => Action::PromptSubmit,
            KeyCode::Esc => Action::PromptCancel,
            KeyCode::Up => Action::Move(-1),
            KeyCode::Down => Action::Move(1),
            KeyCode::PageUp => Action::Page(-1),
            KeyCode::PageDown => Action::Page(1),
            KeyCode::Home => Action::Top,
            KeyCode::End => Action::Bottom,
            _ => return None,
        });
    }
    // Pendant la saisie d'une recherche, les lettres sont du texte, pas des raccourcis.
    if app.prompt.is_some() && !app.help {
        return Some(match key.code {
            KeyCode::Char(c) => Action::PromptKey(c),
            KeyCode::Backspace => Action::PromptBackspace,
            KeyCode::Enter => Action::PromptSubmit,
            KeyCode::Esc => Action::PromptCancel,
            _ => return None,
        });
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
        KeyCode::Left => Action::Horizontal(-4),
        KeyCode::Right => Action::Horizontal(4),
        KeyCode::Char('h') => Action::GotoHead,
        KeyCode::Char('w') => Action::ToggleWrap,
        KeyCode::Char('b') => Action::BranchMenu,
        KeyCode::Char('s') => Action::Search,
        KeyCode::Char('/') => Action::Find,
        KeyCode::Char('S') => Action::CycleNodes,
        KeyCode::Char('R') => Action::Refresh,
        KeyCode::Char('y') => Action::CopySelected,
        KeyCode::Char('n') => Action::Step(1),
        KeyCode::Char('N') => Action::Step(-1),
        KeyCode::Char('?') => Action::ToggleHelp,
        KeyCode::Char(c @ '1'..='4') => Action::GoLevel(c as u8 - b'0'),
        _ => return None,
    })
}

/// Traduit un clic ou la molette en action, d'après les zones du dernier rendu.
fn on_mouse(app: &App, mouse: MouseEvent) -> Option<Action> {
    let hit = &app.hit;
    let pos = Position::new(mouse.column, mouse.row);
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            // Un clic referme le panneau des raccourcis.
            if app.help {
                return Some(Action::ToggleHelp);
            }
            // Sur le code d'un diff, le bouton commence une sélection.
            if hit.code.as_ref().is_some_and(|code| code.area.contains(pos)) {
                return Some(Action::SelectStart(pos));
            }
            // Sous la fenêtre du fichier entier, rien n'est cliquable.
            if app.depth() == 4 {
                return None;
            }
            if hit.folder.is_some_and(|rect| rect.contains(pos)) {
                return Some(Action::CopyFolder);
            }
            if hit.remote.is_some_and(|rect| rect.contains(pos)) {
                return Some(Action::CopyRemote);
            }
            if let Some((_, level)) = hit.chips.iter().find(|(rect, _)| rect.contains(pos)) {
                return Some(Action::GoLevel(*level));
            }
            if hit.pane_copy.is_some_and(|rect| rect.contains(pos)) {
                return Some(Action::CopyHash);
            }
            if hit.graph.contains(pos) {
                // Sous la dernière ligne du graphe, il n'y a rien à ouvrir.
                let row = *hit.graph_rows.get((mouse.row - hit.graph.y) as usize)?;
                return Some(Action::ClickCommit(row));
            }
            if hit.branches.contains(pos) {
                return Some(Action::ClickBranch(hit.branch_top + (mouse.row - hit.branches.y) as usize));
            }
            if hit.files.contains(pos) {
                return Some(Action::ClickFile(hit.file_top + (mouse.row - hit.files.y) as usize));
            }
            None
        }
        MouseEventKind::Drag(MouseButton::Left) if app.selecting => Some(Action::SelectDrag(pos)),
        MouseEventKind::Up(MouseButton::Left) if app.selecting => Some(Action::SelectEnd),
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let delta = if mouse.kind == MouseEventKind::ScrollUp { -WHEEL } else { WHEEL };
            // La fenêtre du fichier entier recouvre tout : la molette lui revient.
            if app.depth() == 4 || hit.pane.contains(pos) {
                Some(Action::ScrollPane(delta))
            } else if hit.graph.contains(pos) || hit.branches.contains(pos) {
                Some(Action::ScrollGraph(delta))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Écrit les premières lignes du graphe en texte : utile pour vérifier un dépôt sans interface.
pub fn dump(app: &mut App, rows: usize) -> Result<()> {
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
        // La ligne des modifications non commitées n'a pas de hash.
        let hash = if oid.is_worktree() { " ".repeat(7) } else { oid.short() };
        println!("{graph:<graph_w$}  {hash}  {labels}{}", meta.subject);
    }
    Ok(())
}
