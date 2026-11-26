//! Terminal et boucle d'événements, les mêmes pour tous les outils. Un outil est
//! un écran : il reçoit les touches et les gestes de souris, avance son travail
//! de fond, se dessine, et dit quand il faut quitter.

use std::io::{self, Stdout};
use std::time::Duration;

use anyhow::{Context, Result};
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEvent, KeyEventKind, MouseEvent};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use ratatui::{Frame, Terminal};

/// Durée d'attente d'un événement tant qu'un travail de fond est en cours.
const TICK: Duration = Duration::from_millis(16);

/// Écran d'un outil de la suite, tel que la boucle d'événements le fait tourner.
pub trait Screen {
    /// Avance le travail de fond (lecture de git, messages éphémères) ; vrai s'il
    /// y a quelque chose à redessiner.
    fn tick(&mut self) -> bool;
    /// Un travail de fond est en cours : la boucle ne s'endort pas jusqu'au
    /// prochain événement.
    fn busy(&self) -> bool;
    /// Une touche ; vrai si elle a eu un effet.
    fn on_key(&mut self, key: KeyEvent) -> bool;
    /// Un geste de souris ; vrai s'il a eu un effet.
    fn on_mouse(&mut self, mouse: MouseEvent) -> bool;
    fn draw(&mut self, frame: &mut Frame);
    fn should_quit(&self) -> bool;
}

/// Fait tourner un écran dans le terminal jusqu'à ce qu'il demande à quitter.
pub fn run(screen: &mut dyn Screen) -> Result<()> {
    let mut terminal = init_terminal().inspect_err(|_| restore_terminal())?;
    let result = event_loop(&mut terminal, screen);
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
    enable_raw_mode().context("not an interactive terminal")?;
    execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    Ok(Terminal::new(CrosstermBackend::new(io::stdout()))?)
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
}

fn event_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, screen: &mut dyn Screen) -> Result<()> {
    let mut dirty = true;
    loop {
        dirty |= screen.tick();
        if dirty {
            terminal.draw(|frame| screen.draw(frame))?;
            dirty = false;
        }
        if screen.should_quit() {
            return Ok(());
        }
        // Sans travail de fond, on dort jusqu'au prochain événement : aucun redessin inutile.
        if screen.busy() && !event::poll(TICK)? {
            continue;
        }
        // Une rafale d'événements (molette, répétition clavier) donne un seul redessin.
        loop {
            dirty |= match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => screen.on_key(key),
                Event::Mouse(mouse) => screen.on_mouse(mouse),
                Event::Resize(..) => true,
                _ => false,
            };
            if !event::poll(Duration::ZERO)? {
                break;
            }
        }
    }
}
