//! État de `gitmerge` : les fichiers en conflit, celui qu'on règle, le curseur
//! dans son résultat, et ce que chaque action y change.

use anyhow::Result;
use ratatui::layout::{Position, Rect};

use crate::kit::edit::{self, Edit, Entry, Keys, Mode};
use crate::kit::find::Needle;
use crate::kit::git::Repo;
use crate::kit::notice::{Clipboard, Flash};
use crate::kit::syntax::Tinter;
use crate::merge::git::{self, Incoming, Unmerged, Versions};
use crate::merge::model::{Doc, Kind, Pos, Side, Snapshot, State};

/// Rangées gardées au-dessus de l'endroit où l'on saute : un peu de contexte.
const JUMP_MARGIN: usize = 4;

/// Rangées gardées entre le curseur et le bord de la vue.
pub const SCROLL_MARGIN: usize = 2;

/// Nombre de pas qu'on peut défaire.
const MAX_UNDO: usize = 500;

/// Occurrence d'une recherche : la ligne du résultat, puis le rang et la
/// longueur dans le texte affiché.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub line: usize,
    pub start: usize,
    pub len: usize,
}

#[derive(Clone, Debug)]
pub struct Find {
    pub query: String,
    /// Occurrences, dans l'ordre de lecture.
    pub hits: Vec<Hit>,
    /// Occurrence où l'on se trouve, parmi `hits`.
    pub current: Option<usize>,
}

impl Find {
    /// Occurrences d'une ligne du résultat : début, longueur, et si c'est
    /// l'occurrence courante.
    pub fn on(&self, line: usize) -> impl Iterator<Item = (usize, usize, bool)> + '_ {
        let first = self.hits.partition_point(|hit| hit.line < line);
        self.hits[first..]
            .iter()
            .enumerate()
            .take_while(move |(_, hit)| hit.line == line)
            .map(move |(i, hit)| (hit.start, hit.len, self.current == Some(first + i)))
    }
}

/// Le résultat et le curseur tels qu'ils étaient avant une modification.
struct Step {
    segs: Snapshot,
    row: usize,
    col: usize,
}

/// Un fichier en cours de fusion : son document, le curseur et la vue.
pub struct Sheet {
    pub doc: Doc,
    /// Rangée du curseur dans le document, et colonne dans la ligne du résultat.
    pub row: usize,
    pub col: usize,
    /// Rang, dans le texte affiché, que le curseur cherche à garder d'une ligne à l'autre.
    want: usize,
    /// Première rangée affichée, et défilement de côté.
    pub top: usize,
    pub x: usize,
    undo: Vec<Step>,
    redo: Vec<Step>,
    /// Le résultat a changé depuis sa dernière écriture.
    pub dirty: bool,
    /// Le résultat a été écrit dans le fichier.
    pub saved: bool,
    /// Le fichier a été remis à git comme réglé.
    pub staged: bool,
    /// Le fichier de travail avait déjà été retouché à la main.
    pub edited: bool,
    /// Teintes des trois colonnes : la nôtre, le résultat, celle qui arrive.
    pub tints: [Tinter; 3],
    pub find: Option<Find>,
}

impl Sheet {
    fn new(versions: &Versions, path: &str, syntax: bool) -> Sheet {
        let mut doc = Doc::new(&versions.base, &versions.ours, &versions.theirs);
        doc.crlf |= versions.crlf;
        let tinter = || Tinter::new(syntax.then_some(path));
        // Le curseur va au premier conflit, ou au premier changement.
        let first = doc.segs.iter().position(|seg| seg.pending()).or_else(|| doc.segs.iter().position(|seg| seg.kind != Kind::Equal));
        let row = first.map_or(0, |seg| doc.seg_row(seg));
        Sheet {
            doc,
            row,
            col: 0,
            want: 0,
            top: row.saturating_sub(JUMP_MARGIN),
            x: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            dirty: false,
            saved: false,
            staged: false,
            edited: versions.edited,
            tints: [tinter(), tinter(), tinter()],
            find: None,
        }
    }

    pub fn pos(&self) -> Pos {
        self.doc.at_row(self.row)
    }

    /// Ligne du résultat sous le curseur ; aucune sur une rangée vide.
    pub fn line(&self) -> Option<&str> {
        self.doc.line(self.pos())
    }

    /// Ramène le curseur dans le document et dans sa ligne.
    pub fn settle_cursor(&mut self, mode: Mode) {
        self.settle(mode);
    }

    fn settle(&mut self, mode: Mode) {
        self.row = self.row.min(self.doc.rows().saturating_sub(1));
        self.col = self.col.min(edit::last_col(self.line().unwrap_or(""), mode));
    }

    /// Le curseur a bougé dans sa ligne : c'est là qu'il voudra rester.
    fn keep_column(&mut self) {
        self.want = edit::cell(self.line().unwrap_or(""), self.col);
    }

    fn go(&mut self, pos: Pos, col: usize) {
        self.row = self.doc.row_of(pos);
        self.col = col;
        self.keep_column();
    }

    /// Change de rangée en gardant, autant que possible, la même colonne à l'écran.
    fn move_rows(&mut self, delta: isize, mode: Mode) {
        let last = self.doc.rows().saturating_sub(1) as isize;
        self.row = (self.row as isize + delta).clamp(0, last) as usize;
        let line = self.line().unwrap_or("");
        self.col = edit::col_at(line, self.want).min(edit::last_col(line, mode));
    }

    /// Amène la vue sur la rangée du curseur après un saut, avec un peu de contexte au-dessus.
    fn reveal(&mut self, page: usize) {
        let rows = self.doc.segs.get(self.pos().seg).map_or(1, |seg| seg.rows());
        let visible = self.row >= self.top && self.row + rows.min(page) <= self.top + page;
        if !visible {
            self.top = self.row.saturating_sub(JUMP_MARGIN);
        }
    }

    /// Retient l'état présent, pour pouvoir y revenir.
    fn record(&mut self) {
        self.undo.push(Step { segs: self.doc.snapshot(), row: self.row, col: self.col });
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Le résultat a changé à partir de la ligne `line`.
    fn changed(&mut self, line: usize) {
        self.dirty = true;
        self.staged = false;
        self.tints[1].invalidate(line);
        if let Some(find) = self.find.take() {
            self.find = Some(Find { hits: self.search(&find.query), query: find.query, current: None });
        }
    }

    fn search(&self, query: &str) -> Vec<Hit> {
        let needle = Needle::new(query);
        let mut hits = Vec::new();
        if needle.is_empty() {
            return hits;
        }
        for (line, text) in self.doc.lines().enumerate() {
            hits.extend(needle.starts(&edit::shown(text)).into_iter().map(|start| Hit { line, start, len: needle.len() }));
        }
        hits
    }

    /// Place du curseur, comme une occurrence : ligne du résultat et rang affiché.
    fn cursor_place(&self) -> (usize, usize) {
        (self.doc.line_number(self.pos()), edit::cell(self.line().unwrap_or(""), self.col))
    }

    /// Met le curseur sur l'occurrence courante.
    fn go_to_hit(&mut self, page: usize) {
        let Some(hit) = self.find.as_ref().and_then(|find| find.hits.get(find.current?)).copied() else { return };
        let pos = self.doc.pos_of_line(hit.line);
        let col = edit::col_at(self.doc.line(pos).unwrap_or(""), hit.start);
        self.go(pos, col);
        self.reveal(page);
    }

    /// Conflits qui attendent encore dans ce fichier.
    pub fn unresolved(&self) -> usize {
        self.doc.unresolved()
    }
}

/// Fichier en conflit, lu quand on y arrive.
pub struct File {
    pub entry: Unmerged,
    pub state: FileState,
}

pub enum FileState {
    Unread,
    Open(Box<Sheet>),
    /// Ne peut pas être fusionné ici : pourquoi.
    Refused(String),
}

impl File {
    pub fn sheet(&self) -> Option<&Sheet> {
        match &self.state {
            FileState::Open(sheet) => Some(sheet),
            _ => None,
        }
    }

    fn dirty(&self) -> bool {
        self.sheet().is_some_and(|sheet| sheet.dirty)
    }

    pub fn staged(&self) -> bool {
        self.sheet().is_some_and(|sheet| sheet.staged)
    }
}

/// Zones de l'écran, mémorisées au dessin pour traduire les clics.
#[derive(Clone, Default)]
pub struct HitMap {
    pub body: Rect,
    /// Pour chaque ligne du corps : la rangée du document, et le rang de la
    /// ligne d'écran dans une rangée repliée.
    pub lines: Vec<(usize, usize)>,
    /// Colonnes des flèches, à gauche et à droite du résultat.
    pub left_gutter: u16,
    pub right_gutter: u16,
    /// Colonnes du code du résultat : début et fin.
    pub code: (u16, u16),
    pub wrap: bool,
    pub folder: Option<Rect>,
    pub remote: Option<Rect>,
}

/// Question posée avant de perdre des résultats non écrits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirm {
    Quit,
    Reload,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// `q`, `Ctrl+C` : quitter, après confirmation s'il reste des résultats non écrits.
    Quit,
    /// `Esc` : fermer l'aide, annuler une question, retirer une recherche.
    Back,
    Help,
    Move(isize),
    Page(isize),
    Top,
    Bottom,
    /// Molette : la vue défile, le curseur suit s'il sort de l'écran.
    Scroll(isize),
    Edit(Edit),
    /// `n` / `N` : conflit suivant ou précédent ; pendant une recherche, occurrence.
    Next(isize),
    /// `Tab` / `Maj+Tab` : fichier en conflit suivant ou précédent.
    File(isize),
    /// `}` / `{` : changement suivant ou précédent.
    Change(isize),
    /// `>` / `<` : prendre notre côté, ou celui qui arrive.
    Take(Side),
    /// `Espace` : tenir le conflit pour réglé tel qu'il est.
    Dismiss,
    /// `m` / `M` : fusionner mot à mot le conflit sous le curseur, ou tous ceux
    /// du fichier qui s'y prêtent.
    Merge { all: bool },
    Save,
    Stage,
    Reload,
    Wrap,
    Copy,
    Find,
    PromptKey(char),
    PromptBackspace,
    PromptSubmit,
    PromptCancel,
    Click(Position),
    /// Réponse à la question posée.
    Answer(bool),
    /// Une touche en attend une autre, ou n'a rien fait : seulement redessiner.
    Redraw,
}

pub struct App {
    pub repo: Repo,
    pub plain: bool,
    pub syntax: bool,
    pub files: Vec<File>,
    pub current: usize,
    /// Nom de notre côté, et ce qui arrive.
    pub ours: String,
    pub incoming: Incoming,
    pub mode: Mode,
    pub keys: Keys,
    pub wrap: bool,
    pub help: bool,
    /// Recherche en cours de saisie.
    pub prompt: Option<String>,
    /// Curseur, vue et recherche d'avant la saisie, pour y revenir.
    before_find: Option<(usize, usize, usize, usize, Option<Find>)>,
    pub confirm: Option<Confirm>,
    flash: Flash,
    clipboard: Clipboard,
    pub hit: HitMap,
    /// Rangées visibles dans le corps, au dernier dessin.
    pub page: usize,
    pub should_quit: bool,
}

impl App {
    /// Ouvre les fichiers en conflit du dépôt ; aucun n'est lu avant qu'on y arrive.
    pub fn open(repo: Repo, plain: bool, syntax: bool) -> Result<App> {
        let files = git::unmerged(&repo.root)?;
        let mut app = App {
            ours: git::ours_name(&repo.head),
            incoming: git::incoming(&repo.root),
            repo,
            plain,
            syntax,
            files: files.into_iter().map(|entry| File { entry, state: FileState::Unread }).collect(),
            current: 0,
            mode: Mode::Normal,
            keys: Keys::default(),
            wrap: false,
            help: false,
            prompt: None,
            before_find: None,
            confirm: None,
            flash: Flash::default(),
            clipboard: Clipboard::default(),
            hit: HitMap::default(),
            page: 20,
            should_quit: false,
        };
        app.read_current();
        Ok(app)
    }

    pub fn flash(&self) -> Option<&str> {
        self.flash.text()
    }

    pub fn tick(&mut self) -> bool {
        self.flash.expire()
    }

    pub fn busy(&self) -> bool {
        self.flash.is_shown()
    }

    pub fn file(&self) -> Option<&File> {
        self.files.get(self.current)
    }

    pub fn sheet(&self) -> Option<&Sheet> {
        self.file().and_then(File::sheet)
    }

    pub fn sheet_mut(&mut self) -> Option<&mut Sheet> {
        match self.files.get_mut(self.current).map(|file| &mut file.state) {
            Some(FileState::Open(sheet)) => Some(sheet),
            _ => None,
        }
    }

    /// Fichiers dont le résultat n'est pas écrit.
    fn unsaved(&self) -> usize {
        self.files.iter().filter(|file| file.dirty()).count()
    }

    /// Lit le fichier courant s'il ne l'a pas encore été.
    fn read_current(&mut self) {
        let (root, syntax) = (self.repo.root.clone(), self.syntax);
        let Some(file) = self.files.get_mut(self.current) else { return };
        if matches!(file.state, FileState::Unread) {
            file.state = match git::versions(&root, &file.entry) {
                Ok(versions) => FileState::Open(Box::new(Sheet::new(&versions, &file.entry.path, syntax))),
                Err(why) => FileState::Refused(why),
            };
            // Le résultat repart des versions de l'index, pas du fichier de travail.
            if file.sheet().is_some_and(|sheet| sheet.edited) {
                self.flash.set("This file was already edited by hand: saving replaces it");
            }
        }
    }

    pub fn apply(&mut self, action: Action) {
        // Une question attend sa réponse : toute autre touche l'annule.
        if let Some(confirm) = self.confirm.take() {
            match (action, confirm) {
                (Action::Answer(true), Confirm::Quit) => self.should_quit = true,
                (Action::Answer(true), Confirm::Reload) => self.reload(),
                _ => {}
            }
            return;
        }
        let (mode, page) = (self.mode, self.page);
        match action {
            Action::Quit => match self.unsaved() {
                0 => self.should_quit = true,
                _ => self.confirm = Some(Confirm::Quit),
            },
            Action::Back => self.back(),
            Action::Help => self.help = !self.help,
            Action::Move(delta) => self.with_sheet(|sheet| sheet.move_rows(delta, mode)),
            Action::Page(delta) => self.with_sheet(|sheet| sheet.move_rows(delta * page.saturating_sub(2).max(1) as isize, mode)),
            Action::Top => self.with_sheet(|sheet| sheet.move_rows(isize::MIN / 2, mode)),
            Action::Bottom => self.with_sheet(|sheet| sheet.move_rows(isize::MAX / 2, mode)),
            Action::Scroll(delta) => self.with_sheet(|sheet| {
                let last = sheet.doc.rows().saturating_sub(1) as isize;
                sheet.top = (sheet.top as isize + delta).clamp(0, last) as usize;
                // Le curseur reste à l'écran, à distance des bords.
                let (first, last) = (sheet.top + SCROLL_MARGIN, sheet.top + page.saturating_sub(1 + SCROLL_MARGIN));
                let row = if delta > 0 { sheet.row.max(first) } else { sheet.row.min(last.max(first)) };
                sheet.move_rows(row as isize - sheet.row as isize, mode);
            }),
            Action::Edit(edit) => self.edit(edit),
            Action::Next(delta) => {
                if !self.find_step(delta) {
                    self.jump(delta, true);
                }
            }
            Action::File(delta) => self.switch_file(delta),
            Action::Change(delta) => self.jump(delta, false),
            Action::Take(side) => {
                let seg = self.sheet().map(|sheet| sheet.pos().seg);
                if let Some(seg) = seg {
                    self.take(seg, side);
                }
            }
            Action::Dismiss => self.dismiss(),
            Action::Merge { all } => self.merge_simple(all),
            Action::Save => self.save(),
            Action::Stage => self.stage(),
            Action::Reload => match self.unsaved() {
                0 => self.reload(),
                _ => self.confirm = Some(Confirm::Reload),
            },
            Action::Wrap => {
                self.wrap = !self.wrap;
                self.flash.set(if self.wrap { "Long lines wrap" } else { "Long lines are cut" });
            }
            Action::Copy => {
                if let Some(line) = self.sheet().and_then(Sheet::line).map(str::to_string) {
                    let told = self.clipboard.copy_and_tell(&line);
                    self.flash.set(told);
                }
            }
            Action::Find => self.open_find(),
            Action::PromptKey(c) => {
                if let Some(query) = &mut self.prompt {
                    query.push(c);
                    self.find_live();
                }
            }
            Action::PromptBackspace => {
                if let Some(query) = &mut self.prompt {
                    query.pop();
                    self.find_live();
                }
            }
            Action::PromptSubmit => self.close_find(true),
            Action::PromptCancel => self.close_find(false),
            Action::Click(at) => self.click(at),
            Action::Answer(_) | Action::Redraw => {}
        }
    }

    fn with_sheet(&mut self, act: impl FnOnce(&mut Sheet)) {
        if let Some(sheet) = self.sheet_mut() {
            act(sheet);
        }
    }

    fn back(&mut self) {
        if self.help {
            self.help = false;
        } else if let Some(sheet) = self.sheet_mut() {
            sheet.find = None;
        }
    }

    /// Applique une commande d'édition au résultat.
    fn edit(&mut self, edit: Edit) {
        let mode = self.mode;
        let Some(sheet) = self.sheet_mut() else { return };
        sheet.settle(mode);
        let pos = sheet.pos();
        let number = sheet.doc.line_number(pos);
        let line = sheet.line().unwrap_or("").to_string();
        // Les blancs qui ouvrent la ligne : une ligne ouverte dessous les reprend.
        let indent: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        let mut next_mode = mode;
        let mut said = None;
        match edit {
            Edit::Left => sheet.col = sheet.col.saturating_sub(1),
            Edit::Right => sheet.col = (sheet.col + 1).min(edit::last_col(&line, mode)),
            Edit::LineStart => sheet.col = 0,
            Edit::LineEnd => sheet.col = edit::last_col(&line, mode),
            Edit::FirstWord => sheet.col = edit::first_word(&line),
            Edit::WordNext => sheet.col = edit::word_next(&line, sheet.col).min(edit::last_col(&line, mode)),
            Edit::WordPrev => sheet.col = edit::word_prev(&line, sheet.col),
            Edit::WordEnd => sheet.col = edit::word_end(&line, sheet.col).min(edit::last_col(&line, mode)),
            Edit::Insert(entry) => {
                sheet.record();
                next_mode = Mode::Insert;
                match entry {
                    Entry::Here => {}
                    Entry::After => sheet.col = (sheet.col + 1).min(line.chars().count()),
                    Entry::LineStart => sheet.col = edit::first_word(&line),
                    Entry::LineEnd => sheet.col = line.chars().count(),
                    Entry::Below | Entry::Above => {
                        let (mut pos, mut col) = (sheet.doc.open_line(pos, entry == Entry::Below), 0);
                        for c in indent.chars() {
                            (pos, col) = sheet.doc.insert_char(pos, col, c);
                        }
                        sheet.go(pos, col);
                        sheet.changed(number);
                    }
                }
            }
            Edit::Leave => {
                next_mode = Mode::Normal;
                sheet.col = sheet.col.saturating_sub(1);
                // Rien n'a été tapé : il n'y a rien à défaire.
                if sheet.undo.last().is_some_and(|step| sheet.doc.same_as(&step.segs)) {
                    sheet.undo.pop();
                }
            }
            Edit::Type(c) => {
                let (pos, col) = sheet.doc.insert_char(pos, sheet.col, c);
                sheet.go(pos, col);
                sheet.changed(number);
            }
            Edit::Newline => {
                let keep: String = indent.chars().take(sheet.col).collect();
                let (mut pos, mut col) = sheet.doc.split_line(pos, sheet.col);
                for c in keep.chars() {
                    (pos, col) = sheet.doc.insert_char(pos, col, c);
                }
                sheet.go(pos, col);
                sheet.changed(number);
            }
            Edit::Backspace => {
                let (pos, col) = sheet.doc.backspace(pos, sheet.col);
                sheet.go(pos, col);
                sheet.changed(number.saturating_sub(1));
            }
            Edit::Delete => {
                if mode == Mode::Normal {
                    sheet.record();
                }
                let (pos, col) = sheet.doc.remove_char(pos, sheet.col);
                sheet.go(pos, col);
                sheet.changed(number);
            }
            Edit::DeleteLine => {
                if sheet.doc.line(pos).is_some() {
                    sheet.record();
                    sheet.doc.delete_line(pos);
                    sheet.changed(number);
                } else {
                    said = Some("No line to delete here");
                }
            }
            Edit::Undo | Edit::Redo => {
                let (from, to) = if edit == Edit::Undo { (&mut sheet.undo, &mut sheet.redo) } else { (&mut sheet.redo, &mut sheet.undo) };
                match from.pop() {
                    Some(step) => {
                        to.push(Step { segs: sheet.doc.snapshot(), row: sheet.row, col: sheet.col });
                        sheet.doc.restore(step.segs);
                        (sheet.row, sheet.col) = (step.row, step.col);
                        sheet.changed(0);
                    }
                    None if edit == Edit::Undo => said = Some("Already at the oldest change"),
                    None => said = Some("Already at the newest change"),
                }
            }
        }
        sheet.settle(next_mode);
        sheet.keep_column();
        self.mode = next_mode;
        if let Some(said) = said {
            self.flash.set(said);
        }
    }

    /// Ce qu'il reste à régler dans le fichier, pour finir un message.
    fn left_to_do(sheet: &Sheet) -> String {
        match sheet.unresolved() {
            0 => "no conflict left: Ctrl+S saves, S stages".to_string(),
            1 => "1 conflict left".to_string(),
            n => format!("{n} conflicts left"),
        }
    }

    /// Prend un côté dans le bloc `seg`.
    fn take(&mut self, seg: usize, side: Side) {
        let mode = self.mode;
        let Some(sheet) = self.sheet_mut() else { return };
        let Some(segment) = sheet.doc.segs.get(seg).filter(|segment| segment.kind != Kind::Equal) else {
            self.flash.set("No change under the cursor");
            return;
        };
        let conflict = segment.kind == Kind::Conflict;
        let added = conflict && segment.state(side.other()) == State::Applied && segment.state(side) != State::Applied;
        let number = sheet.doc.line_number(Pos { seg, line: 0 });
        let row = sheet.doc.seg_row(seg);
        sheet.record();
        sheet.doc.take(seg, side);
        (sheet.row, sheet.col) = (row, 0);
        sheet.changed(number);
        sheet.settle(mode);
        sheet.keep_column();
        let name = if side == Side::Left { "ours" } else { "theirs" };
        let mut said = if added { format!("Added {name} below") } else { format!("Took {name}") };
        if conflict {
            said = format!("{said} · {}", Self::left_to_do(sheet));
        }
        self.flash.set(said);
    }

    fn dismiss(&mut self) {
        let Some(sheet) = self.sheet_mut() else { return };
        let seg = sheet.pos().seg;
        if !sheet.doc.segs.get(seg).is_some_and(|segment| segment.pending()) {
            self.flash.set("No pending conflict under the cursor");
            return;
        }
        sheet.record();
        sheet.doc.dismiss(seg);
        sheet.changed(usize::MAX);
        let said = format!("Kept the result as it is · {}", Self::left_to_do(sheet));
        self.flash.set(said);
    }

    /// `m` / `M` : règle mot à mot le conflit sous le curseur, ou tous ceux du
    /// fichier qui s'y prêtent, en un seul pas à défaire.
    fn merge_simple(&mut self, all: bool) {
        let Some(sheet) = self.sheet_mut() else { return };
        let here = sheet.pos().seg;
        let segs: Vec<usize> = if all { (0..sheet.doc.segs.len()).collect() } else { vec![here] };
        sheet.record();
        let merged: Vec<usize> = segs.into_iter().filter(|&seg| sheet.doc.merge_simple(seg)).collect();
        if merged.is_empty() {
            sheet.undo.pop();
            let pending = sheet.doc.segs.get(here).is_some_and(|segment| segment.pending());
            self.flash.set(match (all, pending) {
                (true, _) => "No conflict in this file merges word to word".to_string(),
                (false, true) => "The two sides changed the same words here".to_string(),
                (false, false) => "No pending conflict under the cursor".to_string(),
            });
            return;
        }
        // Le curseur suit son bloc, dont les lignes ont changé.
        let row = sheet.doc.seg_row(here.min(sheet.doc.segs.len().saturating_sub(1)));
        (sheet.row, sheet.col) = (row, 0);
        sheet.changed(sheet.doc.line_number(Pos { seg: merged[0], line: 0 }));
        sheet.settle(Mode::Normal);
        sheet.keep_column();
        let what = match merged.len() {
            1 => "Merged a conflict word to word".to_string(),
            n => format!("Merged {n} conflicts word to word"),
        };
        let said = format!("{what} · {}", Self::left_to_do(sheet));
        self.flash.set(said);
    }

    /// Saute au conflit (`pending`) ou au changement suivant ou précédent. Les
    /// conflits se parcourent en boucle.
    fn jump(&mut self, delta: isize, pending: bool) {
        let (mode, page) = (self.mode, self.page);
        let Some(sheet) = self.sheet_mut() else { return };
        let (here, count) = (sheet.pos().seg, sheet.doc.segs.len());
        if count == 0 {
            return;
        }
        let wanted = |seg: &usize| {
            let segment = &sheet.doc.segs[*seg];
            if pending { segment.pending() } else { segment.kind != Kind::Equal }
        };
        let found = match (delta > 0, pending) {
            (true, true) => (here + 1..count).chain(0..=here).find(wanted),
            (false, true) => (0..here).rev().chain((here..count).rev()).find(wanted),
            (true, false) => (here + 1..count).find(wanted),
            (false, false) => (0..here).rev().find(wanted),
        };
        let Some(seg) = found else {
            self.flash.set(if pending { "No conflict left in this file" } else { "No more changes" });
            return;
        };
        (sheet.row, sheet.col) = (sheet.doc.seg_row(seg), 0);
        sheet.settle(mode);
        sheet.keep_column();
        sheet.reveal(page);
    }

    /// Passe au fichier en conflit suivant ou précédent.
    fn switch_file(&mut self, delta: isize) {
        let last = self.files.len().saturating_sub(1) as isize;
        let to = (self.current as isize + delta).clamp(0, last) as usize;
        if to == self.current {
            self.flash.set(if delta > 0 { "Last conflicted file" } else { "First conflicted file" });
            return;
        }
        self.leave_insert();
        self.current = to;
        self.read_current();
    }

    /// Quitte le mode insertion, comme le ferait `Esc`.
    fn leave_insert(&mut self) {
        if self.mode == Mode::Insert {
            self.edit(Edit::Leave);
        }
        self.keys.clear();
    }

    /// Écrit le résultat dans le fichier de travail ; faux si l'écriture échoue.
    fn write(&mut self) -> bool {
        let root = self.repo.root.clone();
        let Some(file) = self.files.get_mut(self.current) else { return false };
        let FileState::Open(sheet) = &mut file.state else { return false };
        match git::save(&root, &file.entry.path, &sheet.doc.text()) {
            Ok(()) => {
                (sheet.dirty, sheet.saved) = (false, true);
                true
            }
            Err(err) => {
                self.flash.set(format!("{err:#}"));
                false
            }
        }
    }

    fn save(&mut self) {
        if !self.write() {
            return;
        }
        let Some(file) = self.file() else { return };
        let said = match file.sheet().map_or(0, Sheet::unresolved) {
            0 => format!("Saved {} · S stages it", file.entry.path),
            1 => format!("Saved {} · 1 conflict still holds the base", file.entry.path),
            n => format!("Saved {} · {n} conflicts still hold the base", file.entry.path),
        };
        self.flash.set(said);
    }

    /// Écrit le résultat puis le remet à git : le conflit du fichier est réglé.
    fn stage(&mut self) {
        let Some(sheet) = self.sheet() else { return };
        match sheet.unresolved() {
            0 => {}
            1 => return self.flash.set("1 conflict left: take a side, or Space keeps the result as it is"),
            n => return self.flash.set(format!("{n} conflicts left: take a side, or Space keeps the result as it is")),
        }
        if (sheet.dirty || !sheet.saved) && !self.write() {
            return;
        }
        let Some(path) = self.file().map(|file| file.entry.path.clone()) else { return };
        if let Err(err) = git::stage(&self.repo.root, &path) {
            return self.flash.set(format!("git add: {err:#}"));
        }
        self.with_sheet(|sheet| sheet.staged = true);
        // Au fichier suivant qui reste à régler, s'il y en a un.
        let count = self.files.len();
        let next = (1..count).map(|step| (self.current + step) % count).find(|&file| !self.files[file].staged());
        match next {
            Some(file) => {
                let left = self.files.iter().filter(|file| !file.staged()).count();
                self.leave_insert();
                self.current = file;
                self.read_current();
                self.flash.set(format!("Staged {path} · {left} file{} left", if left == 1 { "" } else { "s" }));
            }
            None => self.flash.set(format!("Staged {path} · every conflict is resolved: {}", self.incoming.conclusion())),
        }
    }

    /// Relit les fichiers en conflit : les résultats non écrits sont perdus.
    fn reload(&mut self) {
        let path = self.file().map(|file| file.entry.path.clone());
        match git::unmerged(&self.repo.root) {
            Ok(files) => {
                self.files = files.into_iter().map(|entry| File { entry, state: FileState::Unread }).collect();
                self.current = self.files.iter().position(|file| Some(&file.entry.path) == path.as_ref()).unwrap_or(0);
                self.incoming = git::incoming(&self.repo.root);
                (self.mode, self.prompt, self.before_find) = (Mode::Normal, None, None);
                self.keys.clear();
                self.read_current();
                self.flash.set(match self.files.len() {
                    0 => "No conflicted file left".to_string(),
                    1 => "1 conflicted file".to_string(),
                    n => format!("{n} conflicted files"),
                });
            }
            Err(err) => self.flash.set(format!("{err:#}")),
        }
    }

    /// `/` : ouvre la saisie d'une recherche dans le résultat.
    fn open_find(&mut self) {
        self.leave_insert();
        let Some(sheet) = self.sheet_mut() else { return };
        self.before_find = Some((sheet.row, sheet.col, sheet.top, sheet.x, sheet.find.take()));
        self.prompt = Some(String::new());
    }

    /// La saisie change : la recherche repart d'où était le curseur, et va à la
    /// première occurrence qui suit.
    fn find_live(&mut self) {
        let page = self.page;
        let Some(query) = self.prompt.clone() else { return };
        let Some(&(row, col, top, x, _)) = self.before_find.as_ref() else { return };
        let Some(sheet) = self.sheet_mut() else { return };
        (sheet.row, sheet.col, sheet.top, sheet.x) = (row, col, top, x);
        let hits = sheet.search(&query);
        let from = sheet.cursor_place();
        let current = hits.iter().position(|hit| (hit.line, hit.start) >= from).or((!hits.is_empty()).then_some(0));
        sheet.find = (!query.is_empty()).then_some(Find { query, hits, current });
        sheet.go_to_hit(page);
    }

    /// Fin de la saisie. `keep` garde la recherche si elle a trouvé quelque chose ;
    /// sinon le curseur revient où il était, et l'annulation rend la recherche d'avant.
    fn close_find(&mut self, keep: bool) {
        self.prompt = None;
        let Some((row, col, top, x, find)) = self.before_find.take() else { return };
        let Some(sheet) = self.sheet_mut() else { return };
        if keep && sheet.find.as_ref().is_some_and(|find| !find.hits.is_empty()) {
            return;
        }
        (sheet.row, sheet.col, sheet.top, sheet.x) = (row, col, top, x);
        sheet.find = find.filter(|_| !keep);
    }

    /// `n` / `N` pendant une recherche : l'occurrence qui suit ou précède le
    /// curseur, en boucle. Faux quand aucune recherche n'est en place.
    fn find_step(&mut self, delta: isize) -> bool {
        let page = self.page;
        let Some(sheet) = self.sheet_mut() else { return false };
        let from = sheet.cursor_place();
        let Some(find) = &mut sheet.find else { return false };
        if find.hits.is_empty() {
            self.flash.set("No match");
            return true;
        }
        let last = find.hits.len() - 1;
        find.current = Some(if delta > 0 {
            find.hits.iter().position(|hit| (hit.line, hit.start) > from).unwrap_or(0)
        } else {
            find.hits.iter().rposition(|hit| (hit.line, hit.start) < from).unwrap_or(last)
        });
        sheet.go_to_hit(page);
        true
    }

    /// Recherche à montrer en bas de l'écran : son texte, si elle est en cours de
    /// saisie, le rang de l'occurrence courante et leur nombre.
    pub fn shown_find(&self) -> Option<(&str, bool, Option<usize>, usize)> {
        let find = self.sheet().and_then(|sheet| sheet.find.as_ref());
        let state = (find.and_then(|find| find.current), find.map_or(0, |find| find.hits.len()));
        match &self.prompt {
            Some(query) => Some((query, true, state.0, state.1)),
            None => find.map(|find| (find.query.as_str(), false, state.0, state.1)),
        }
    }

    /// Clic : sur une flèche, le côté est pris ; dans le résultat, le curseur s'y
    /// pose ; sur l'en-tête, le chemin du dossier ou l'URL du dépôt sont copiés.
    fn click(&mut self, at: Position) {
        let hit = self.hit.clone();
        let copied = if hit.folder.is_some_and(|area| area.contains(at)) {
            Some(self.repo.root.display().to_string())
        } else if hit.remote.is_some_and(|area| area.contains(at)) {
            self.repo.remote_url.clone()
        } else {
            None
        };
        if let Some(text) = copied {
            let told = self.clipboard.copy_and_tell(&text);
            return self.flash.set(told);
        }
        if !hit.body.contains(at) {
            return;
        }
        let Some(&(row, part)) = hit.lines.get((at.y - hit.body.y) as usize) else { return };
        let mode = self.mode;
        let Some(sheet) = self.sheet_mut() else { return };
        let seg = sheet.doc.at_row(row).seg;
        let side = match at.x {
            x if x == hit.left_gutter => Some(Side::Left),
            x if x == hit.right_gutter => Some(Side::Right),
            _ => None,
        };
        if let Some(side) = side {
            if sheet.doc.segs.get(seg).is_some_and(|segment| segment.changed(side)) {
                self.take(seg, side);
            }
            return;
        }
        sheet.row = row;
        if (hit.code.0..hit.code.1).contains(&at.x) {
            let room = (hit.code.1 - hit.code.0) as usize;
            let skip = if hit.wrap { part * room } else { sheet.x };
            sheet.col = edit::col_at(sheet.line().unwrap_or(""), skip + (at.x - hit.code.0) as usize);
        }
        sheet.settle(mode);
        sheet.keep_column();
    }
}
