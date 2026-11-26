//! Édition de texte modale, à la manière de vi : en mode normal les touches sont
//! des commandes, en mode insertion elles s'écrivent. Ce module lit les touches
//! et sait se déplacer dans une ligne ; le texte lui-même appartient à l'outil,
//! qui applique chaque commande à son document.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::kit::syntax::TAB_WIDTH;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Normal,
    Insert,
}

impl Mode {
    /// Nom affiché dans la ligne d'état.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
        }
    }
}

/// Où commence la saisie quand on passe en mode insertion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    /// `i` : sous le curseur.
    Here,
    /// `a` : après le curseur.
    After,
    /// `I` : au premier caractère de la ligne.
    LineStart,
    /// `A` : en fin de ligne.
    LineEnd,
    /// `o` : sur une nouvelle ligne, dessous.
    Below,
    /// `O` : sur une nouvelle ligne, dessus.
    Above,
}

/// Commande d'édition tirée d'une touche.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edit {
    Left,
    Right,
    LineStart,
    LineEnd,
    /// Premier caractère de la ligne qui ne soit pas un blanc.
    FirstWord,
    WordNext,
    WordPrev,
    Insert(Entry),
    /// `Esc` : retour au mode normal.
    Leave,
    Type(char),
    Newline,
    Backspace,
    Delete,
    DeleteLine,
    Undo,
    Redo,
}

/// Ce que devient une touche.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Read {
    Edit(Edit),
    /// Première touche d'une commande à deux temps (`dd`) : la suite est attendue.
    Wait,
    /// Pas une touche d'édition : à l'outil d'en faire ce qu'il veut.
    Pass,
}

/// Lecteur de touches : il retient la première d'une commande à deux temps.
#[derive(Default)]
pub struct Keys {
    pending: Option<char>,
}

impl Keys {
    /// Touche en attente de sa suite, à rappeler dans la ligne d'état.
    pub fn pending(&self) -> Option<char> {
        self.pending
    }

    pub fn clear(&mut self) {
        self.pending = None;
    }

    pub fn read(&mut self, mode: Mode, key: KeyEvent) -> Read {
        let (ctrl, alt) = held(key);
        // Les déplacements dans la ligne sont les mêmes dans les deux modes.
        let shared = match key.code {
            KeyCode::Left if ctrl => Some(Edit::WordPrev),
            KeyCode::Right if ctrl => Some(Edit::WordNext),
            KeyCode::Left => Some(Edit::Left),
            KeyCode::Right => Some(Edit::Right),
            KeyCode::Home => Some(Edit::LineStart),
            KeyCode::End => Some(Edit::LineEnd),
            KeyCode::Delete => Some(Edit::Delete),
            _ => None,
        };
        if let Some(edit) = shared {
            self.pending = None;
            return Read::Edit(edit);
        }
        match mode {
            Mode::Insert => match key.code {
                KeyCode::Esc => Read::Edit(Edit::Leave),
                KeyCode::Enter => Read::Edit(Edit::Newline),
                KeyCode::Backspace => Read::Edit(Edit::Backspace),
                KeyCode::Tab => Read::Edit(Edit::Type('\t')),
                KeyCode::Char(c) if !ctrl && !alt => Read::Edit(Edit::Type(c)),
                _ => Read::Pass,
            },
            Mode::Normal => {
                if self.pending.take() == Some('d') {
                    // Toute autre suite annule la commande.
                    return if key.code == KeyCode::Char('d') && !ctrl { Read::Edit(Edit::DeleteLine) } else { Read::Wait };
                }
                let KeyCode::Char(c) = key.code else { return Read::Pass };
                if ctrl || alt {
                    return if ctrl && c == 'r' { Read::Edit(Edit::Redo) } else { Read::Pass };
                }
                Read::Edit(match c {
                    'h' => Edit::Left,
                    'l' => Edit::Right,
                    '0' => Edit::LineStart,
                    '$' => Edit::LineEnd,
                    '^' => Edit::FirstWord,
                    'i' => Edit::Insert(Entry::Here),
                    'a' => Edit::Insert(Entry::After),
                    'I' => Edit::Insert(Entry::LineStart),
                    'A' => Edit::Insert(Entry::LineEnd),
                    'o' => Edit::Insert(Entry::Below),
                    'O' => Edit::Insert(Entry::Above),
                    'u' => Edit::Undo,
                    'd' => {
                        self.pending = Some('d');
                        return Read::Wait;
                    }
                    _ => return Read::Pass,
                })
            }
        }
    }
}

/// `Ctrl` et `Alt` tenus avec la touche. Les deux ensemble sont `AltGr`, qui sert
/// à taper un caractère (`{`, `[`, `@` sur bien des claviers) : ni l'un ni l'autre.
pub fn held(key: KeyEvent) -> (bool, bool) {
    let (ctrl, alt) = (key.modifiers.contains(KeyModifiers::CONTROL), key.modifiers.contains(KeyModifiers::ALT));
    (ctrl && !alt, alt && !ctrl)
}

/// Dernière colonne où le curseur peut se tenir : après le dernier caractère en
/// insertion, dessus en mode normal.
pub fn last_col(line: &str, mode: Mode) -> usize {
    let len = line.chars().count();
    match mode {
        Mode::Insert => len,
        Mode::Normal => len.saturating_sub(1),
    }
}

/// Colonne du premier caractère qui n'est pas un blanc.
pub fn first_word(line: &str) -> usize {
    line.chars().position(|c| !c.is_whitespace()).unwrap_or(0)
}

/// Nature d'un caractère pour les déplacements de mot en mot.
fn class(c: char) -> u8 {
    match c {
        c if c.is_whitespace() => 0,
        c if c.is_alphanumeric() || c == '_' => 1,
        _ => 2,
    }
}

/// Début du mot suivant, ou la fin de la ligne.
pub fn word_next(line: &str, col: usize) -> usize {
    let chars: Vec<char> = line.chars().collect();
    let mut at = col.min(chars.len());
    if let Some(&first) = chars.get(at) {
        while chars.get(at).is_some_and(|&c| class(c) == class(first) && class(c) != 0) {
            at += 1;
        }
    }
    while chars.get(at).is_some_and(|&c| class(c) == 0) {
        at += 1;
    }
    at
}

/// Début du mot où l'on se trouve ou, si l'on y est déjà, du précédent.
pub fn word_prev(line: &str, col: usize) -> usize {
    let chars: Vec<char> = line.chars().collect();
    let mut at = col.min(chars.len());
    while at > 0 && class(chars[at - 1]) == 0 {
        at -= 1;
    }
    if at > 0 {
        let kind = class(chars[at - 1]);
        while at > 0 && class(chars[at - 1]) == kind {
            at -= 1;
        }
    }
    at
}

/// Le texte tel qu'il s'affiche : tabulations étendues, caractères de contrôle
/// neutralisés. Les teintes de la coloration syntaxique comptent ces caractères-là.
pub fn shown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\t' => out.extend([' '; TAB_WIDTH]),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// Rang, dans le texte affiché, du caractère de colonne `col`.
pub fn cell(text: &str, col: usize) -> usize {
    text.chars().take(col).map(|c| if c == '\t' { TAB_WIDTH } else { 1 }).sum()
}

/// Colonne du caractère qui occupe le rang `cell` du texte affiché.
pub fn col_at(text: &str, cell: usize) -> usize {
    let mut used = 0;
    for (col, c) in text.chars().enumerate() {
        used += if c == '\t' { TAB_WIDTH } else { 1 };
        if used > cell {
            return col;
        }
    }
    text.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn normal_keys_are_commands() {
        let mut keys = Keys::default();
        assert_eq!(keys.read(Mode::Normal, key(KeyCode::Char('i'))), Read::Edit(Edit::Insert(Entry::Here)));
        assert_eq!(keys.read(Mode::Normal, key(KeyCode::Char('h'))), Read::Edit(Edit::Left));
        assert_eq!(keys.read(Mode::Normal, key(KeyCode::Char('z'))), Read::Pass);
        assert_eq!(keys.read(Mode::Normal, KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)), Read::Edit(Edit::Redo));
        assert_eq!(keys.read(Mode::Normal, KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL)), Read::Edit(Edit::WordNext));
    }

    #[test]
    fn dd_takes_two_keys() {
        let mut keys = Keys::default();
        assert_eq!(keys.read(Mode::Normal, key(KeyCode::Char('d'))), Read::Wait);
        assert_eq!(keys.pending(), Some('d'));
        assert_eq!(keys.read(Mode::Normal, key(KeyCode::Char('d'))), Read::Edit(Edit::DeleteLine));
        // Une autre touche annule, sans rien faire d'elle-même.
        keys.read(Mode::Normal, key(KeyCode::Char('d')));
        assert_eq!(keys.read(Mode::Normal, key(KeyCode::Char('i'))), Read::Wait);
        assert_eq!(keys.pending(), None);
    }

    #[test]
    fn insert_keys_are_text() {
        let mut keys = Keys::default();
        assert_eq!(keys.read(Mode::Insert, key(KeyCode::Char('d'))), Read::Edit(Edit::Type('d')));
        assert_eq!(keys.read(Mode::Insert, key(KeyCode::Tab)), Read::Edit(Edit::Type('\t')));
        assert_eq!(keys.read(Mode::Insert, key(KeyCode::Esc)), Read::Edit(Edit::Leave));
        assert_eq!(keys.read(Mode::Insert, KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)), Read::Pass);
    }

    #[test]
    fn altgr_types_a_character() {
        let mut keys = Keys::default();
        let altgr = KeyModifiers::CONTROL | KeyModifiers::ALT;
        assert_eq!(keys.read(Mode::Insert, KeyEvent::new(KeyCode::Char('{'), altgr)), Read::Edit(Edit::Type('{')));
        assert_eq!(keys.read(Mode::Insert, KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT)), Read::Pass);
        // En mode normal, la touche reste celle du caractère tapé.
        assert_eq!(keys.read(Mode::Normal, KeyEvent::new(KeyCode::Char('i'), altgr)), Read::Edit(Edit::Insert(Entry::Here)));
        assert_eq!(keys.read(Mode::Normal, KeyEvent::new(KeyCode::Char('r'), altgr)), Read::Pass);
    }

    #[test]
    fn words() {
        let line = "let total = a.len() + 1;";
        assert_eq!(word_next(line, 0), 4);
        assert_eq!(word_next(line, 4), 10);
        assert_eq!(word_next(line, 12), 13);
        assert_eq!(word_next(line, 23), 24);
        assert_eq!(word_prev(line, 24), 23);
        assert_eq!(word_prev(line, 10), 4);
        assert_eq!(word_prev(line, 6), 4);
        assert_eq!(word_prev(line, 0), 0);
        assert_eq!(first_word("    x"), 4);
        assert_eq!((last_col("abc", Mode::Normal), last_col("abc", Mode::Insert), last_col("", Mode::Normal)), (2, 3, 0));
    }

    #[test]
    fn tabs_take_their_width() {
        assert_eq!(shown("\tif x {\u{7}"), "    if x { ");
        assert_eq!(cell("\tab", 1), TAB_WIDTH);
        assert_eq!(cell("\tab", 2), TAB_WIDTH + 1);
        assert_eq!(col_at("\tab", 2), 0);
        assert_eq!(col_at("\tab", TAB_WIDTH), 1);
        assert_eq!(col_at("\tab", 99), 3);
    }
}
