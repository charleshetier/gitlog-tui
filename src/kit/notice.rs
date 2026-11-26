//! Ce qu'un outil dit en passant : un message éphémère, en bas de l'écran, et la
//! copie dans le presse-papier du système.

use std::time::{Duration, Instant};

/// Durée d'affichage d'un message éphémère.
const FLASH: Duration = Duration::from_millis(1500);

/// Message éphémère (copie faite, butée atteinte), effacé de lui-même.
#[derive(Default)]
pub struct Flash(Option<(String, Instant)>);

impl Flash {
    pub fn set(&mut self, text: impl Into<String>) {
        self.0 = Some((text.into(), Instant::now()));
    }

    pub fn text(&self) -> Option<&str> {
        self.0.as_ref().map(|(text, _)| text.as_str())
    }

    pub fn is_shown(&self) -> bool {
        self.0.is_some()
    }

    /// Efface le message une fois son temps écoulé ; vrai s'il vient de l'être.
    pub fn expire(&mut self) -> bool {
        let over = self.0.as_ref().is_some_and(|(_, since)| since.elapsed() >= FLASH);
        if over {
            self.0 = None;
        }
        over
    }
}

/// Presse-papier du système, ouvert à la première copie.
#[derive(Default)]
pub struct Clipboard(Option<arboard::Clipboard>);

impl Clipboard {
    /// Copie `text` ; faux si le presse-papier n'est pas disponible.
    pub fn copy(&mut self, text: &str) -> bool {
        if self.0.is_none() {
            self.0 = arboard::Clipboard::new().ok();
        }
        self.0.as_mut().is_some_and(|clipboard| clipboard.set_text(text).is_ok())
    }

    /// Copie `text` et dit ce qui a été fait, pour le message éphémère.
    pub fn copy_and_tell(&mut self, text: &str) -> String {
        if !self.copy(text) {
            return "Clipboard unavailable".to_string();
        }
        // Un extrait de plusieurs lignes ne tient pas dans le message.
        match text.lines().count() {
            lines if lines > 1 => format!("Copied {lines} lines"),
            _ => format!("Copied: {}", text.trim()),
        }
    }
}
