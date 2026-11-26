//! Clavier et souris de `gitmerge` : chaque touche, chaque geste devient une
//! action de l'application. En mode insertion, les touches s'écrivent.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Position;

use crate::kit::edit::{Edit, Entry, Mode, Read, held};
use crate::kit::term::Screen;
use crate::merge::app::{Action, App};
use crate::merge::model::Side;
use crate::merge::ui;

/// Rangées défilées par cran de molette.
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

pub fn on_key(app: &mut App, key: KeyEvent) -> Option<Action> {
    let (ctrl, alt) = held(key);
    if ctrl && key.code == KeyCode::Char('c') {
        return Some(Action::Quit);
    }
    // Une question est posée : `y` l'accepte, toute autre touche l'annule.
    if app.confirm.is_some() {
        return Some(Action::Answer(matches!(key.code, KeyCode::Char('y' | 'Y'))));
    }
    if app.help {
        return matches!(key.code, KeyCode::Esc | KeyCode::Char('?' | 'q')).then_some(Action::Help);
    }
    if app.prompt.is_some() {
        return Some(match key.code {
            KeyCode::Char(c) if !ctrl && !alt => Action::PromptKey(c),
            KeyCode::Backspace => Action::PromptBackspace,
            KeyCode::Enter => Action::PromptSubmit,
            KeyCode::Esc => Action::PromptCancel,
            _ => return None,
        });
    }
    if ctrl && key.code == KeyCode::Char('s') {
        return Some(Action::Save);
    }
    match app.keys.read(app.mode, key) {
        Read::Edit(edit) => return Some(Action::Edit(edit)),
        Read::Wait => return Some(Action::Redraw),
        Read::Pass => {}
    }
    // D'une ligne à l'autre, dans les deux modes.
    let moved = match key.code {
        KeyCode::Up => Some(Action::Move(-1)),
        KeyCode::Down => Some(Action::Move(1)),
        KeyCode::PageUp => Some(Action::Page(-1)),
        KeyCode::PageDown => Some(Action::Page(1)),
        _ => None,
    };
    if moved.is_some() || app.mode == Mode::Insert || ctrl || alt {
        return moved;
    }
    Some(match key.code {
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Esc => Action::Back,
        KeyCode::Char('?') => Action::Help,
        KeyCode::Char('j') => Action::Move(1),
        KeyCode::Char('k') => Action::Move(-1),
        KeyCode::Char('g') => Action::Top,
        KeyCode::Char('G') => Action::Bottom,
        KeyCode::Enter => Action::Edit(Edit::Insert(Entry::Here)),
        KeyCode::Char('n') => Action::Next(1),
        KeyCode::Char('N') => Action::Next(-1),
        // Les crochets et les accolades demandent AltGr sur bien des claviers :
        // chacun a une touche de rechange.
        KeyCode::Char(']') | KeyCode::Tab => Action::Conflict(1),
        KeyCode::Char('[') | KeyCode::BackTab => Action::Conflict(-1),
        KeyCode::Char('}' | 'c') => Action::Change(1),
        KeyCode::Char('{' | 'C') => Action::Change(-1),
        KeyCode::Char('>') => Action::Take(Side::Left),
        KeyCode::Char('<') => Action::Take(Side::Right),
        KeyCode::Char('x') => Action::Dismiss,
        KeyCode::Char('S') => Action::Stage,
        KeyCode::Char('R') => Action::Reload,
        KeyCode::Char('w') => Action::Wrap,
        KeyCode::Char('y') => Action::Copy,
        KeyCode::Char('/') => Action::Find,
        _ => return None,
    })
}

pub fn on_mouse(app: &App, mouse: MouseEvent) -> Option<Action> {
    if app.help || app.confirm.is_some() || app.prompt.is_some() {
        return None;
    }
    match mouse.kind {
        MouseEventKind::ScrollUp => Some(Action::Scroll(-WHEEL)),
        MouseEventKind::ScrollDown => Some(Action::Scroll(WHEEL)),
        MouseEventKind::Down(MouseButton::Left) => Some(Action::Click(Position::new(mouse.column, mouse.row))),
        _ => None,
    }
}
