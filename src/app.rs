//! État de l'application et actions qui le modifient.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::layout::Rect;

use crate::diff::{DiffLine, FileEntry};
use crate::git::{self, DiffMsg, DiffWorker, Label, LogMsg, Meta, MetaReader, Repo};
use crate::graph::{GraphStore, Oid};

/// Lignes gardées d'avance sous le bas de l'écran avant de laisser git en pause.
const LOOKAHEAD: usize = 2000;
/// Lots absorbés au plus par tour de boucle, pour garder l'interface réactive.
const BATCHES_PER_TICK: usize = 64;
/// Entrées du cache de métadonnées avant remise à zéro.
const META_CACHE: usize = 4096;
const FLASH: Duration = Duration::from_millis(1500);

pub enum FilesState {
    Loading,
    Ready(Vec<FileEntry>),
    Failed(String),
}

/// Niveau 3 : le diff d'un fichier.
pub struct DiffView {
    pub file: usize,
    pub lines: Vec<DiffLine>,
    pub done: bool,
    pub top: usize,
    pub x: usize,
    req: u64,
}

/// Volet de droite : niveau 2 (fichiers) et, si `diff` est présent, niveau 3.
pub struct Pane {
    pub oid: Oid,
    pub meta: Meta,
    pub files: FilesState,
    pub file_sel: usize,
    pub file_top: usize,
    pub diff: Option<DiffView>,
    req: u64,
}

/// Zones cliquables mémorisées au dernier rendu.
#[derive(Default)]
pub struct HitMap {
    pub graph: Rect,
    pub graph_top: usize,
    /// Colonne de l'icône de copie dans le graphe.
    pub icon_x: Option<u16>,
    pub pane: Rect,
    pub files: Rect,
    pub file_top: usize,
    pub chips: Vec<(Rect, u8)>,
    pub pane_copy: Option<Rect>,
}

pub enum Action {
    Quit,
    /// Remonte d'un niveau ; au niveau 1, quitte si `quit` est vrai.
    Back { quit: bool },
    Move(isize),
    Page(isize),
    Top,
    Bottom,
    Open,
    GoLevel(u8),
    ClickCommit(usize),
    ClickFile(usize),
    CopySelected,
    CopyRow(usize),
    ScrollGraph(isize),
    ScrollPane(isize),
    Horizontal(isize),
}

pub struct App {
    pub repo: Repo,
    pub plain: bool,
    pub store: GraphStore,
    pub labels: HashMap<Oid, Vec<Label>>,
    pub log_done: bool,
    pub log_error: Option<String>,
    pub want_all: bool,
    pub selected: usize,
    pub top: usize,
    /// Hauteurs visibles, renseignées par le rendu.
    pub graph_height: usize,
    pub pane_height: usize,
    pub pane: Option<Pane>,
    pub hit: HitMap,
    pub should_quit: bool,
    flash: Option<(String, Instant)>,
    log_rx: Option<Receiver<LogMsg>>,
    goto_end: bool,
    meta_reader: MetaReader,
    meta_cache: HashMap<usize, Meta>,
    diffs: DiffWorker,
    clipboard: Option<arboard::Clipboard>,
}

impl App {
    pub fn new(repo: Repo, plain: bool) -> Result<App> {
        // Le flux démarre en premier : git parcourt pendant que les refs sont lues.
        let log_rx = Some(git::spawn_log(repo.root.clone()));
        let labels = git::load_labels(&repo)?;
        Ok(App {
            meta_reader: MetaReader::new(&repo.root)?,
            diffs: DiffWorker::new(repo.root.clone()),
            repo,
            plain,
            store: GraphStore::default(),
            labels,
            log_done: false,
            log_error: None,
            want_all: false,
            selected: 0,
            top: 0,
            graph_height: 0,
            pane_height: 0,
            pane: None,
            hit: HitMap::default(),
            should_quit: false,
            flash: None,
            log_rx,
            goto_end: false,
            meta_cache: HashMap::new(),
            clipboard: None,
        })
    }

    /// Niveau de profondeur courant : 1 graphe, 2 fichiers, 3 diff.
    pub fn depth(&self) -> u8 {
        match &self.pane {
            None => 1,
            Some(pane) if pane.diff.is_some() => 3,
            Some(_) => 2,
        }
    }

    pub fn flash(&self) -> Option<&str> {
        self.flash.as_ref().map(|(text, _)| text.as_str())
    }

    /// Métadonnées d'une ligne, lues à la demande et gardées dans un petit cache.
    pub fn meta(&mut self, row: usize) -> Meta {
        if let Some(meta) = self.meta_cache.get(&row) {
            return meta.clone();
        }
        if self.meta_cache.len() >= META_CACHE {
            self.meta_cache.clear();
        }
        let meta = self.meta_reader.get(&self.store.oid(row));
        self.meta_cache.insert(row, meta.clone());
        meta
    }

    fn wanted_rows(&self) -> usize {
        if self.want_all {
            usize::MAX
        } else {
            self.top + self.graph_height + LOOKAHEAD
        }
    }

    /// Vrai tant qu'un travail de fond peut encore changer l'écran.
    pub fn busy(&self) -> bool {
        let loading_log = !self.log_done && self.store.len() < self.wanted_rows();
        let loading_pane = self.pane.as_ref().is_some_and(|p| {
            matches!(p.files, FilesState::Loading) || p.diff.as_ref().is_some_and(|d| !d.done)
        });
        loading_log || loading_pane || self.flash.is_some()
    }

    /// Absorbe ce que les threads de fond ont produit. Renvoie vrai s'il faut redessiner.
    pub fn tick(&mut self) -> bool {
        let mut changed = self.pump_log();
        changed |= self.pump_diffs();
        if self.flash.as_ref().is_some_and(|(_, at)| at.elapsed() >= FLASH) {
            self.flash = None;
            changed = true;
        }
        changed
    }

    fn pump_log(&mut self) -> bool {
        let mut changed = false;
        let wanted = self.wanted_rows();
        for _ in 0..BATCHES_PER_TICK {
            if self.log_done || self.store.len() >= wanted {
                break;
            }
            let Some(rx) = &self.log_rx else { break };
            match rx.try_recv() {
                Ok(LogMsg::Batch(batch)) => {
                    self.store.append(batch);
                    changed = true;
                }
                Ok(LogMsg::Error(err)) => {
                    self.log_error = Some(err);
                    changed = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.log_done = true;
                    self.log_rx = None;
                    changed = true;
                }
            }
        }
        if self.goto_end && self.log_done {
            self.goto_end = false;
            self.want_all = false;
            self.selected = self.store.len().saturating_sub(1);
            self.ensure_visible();
            changed = true;
        }
        changed
    }

    fn pump_diffs(&mut self) -> bool {
        let mut changed = false;
        while let Ok(msg) = self.diffs.rx.try_recv() {
            let Some(pane) = &mut self.pane else { continue };
            match msg {
                DiffMsg::Files { id, result } if id == pane.req => {
                    pane.files = match result {
                        Ok(files) => FilesState::Ready(files),
                        Err(err) => FilesState::Failed(err),
                    };
                    changed = true;
                }
                DiffMsg::Lines { id, mut lines, done } => {
                    if let Some(diff) = pane.diff.as_mut().filter(|d| d.req == id) {
                        diff.lines.append(&mut lines);
                        diff.done = done;
                        changed = true;
                    }
                }
                _ => {}
            }
        }
        changed
    }

    fn ensure_visible(&mut self) {
        let height = self.graph_height.max(1);
        if self.selected < self.top {
            self.top = self.selected;
        } else if self.selected >= self.top + height {
            self.top = self.selected + 1 - height;
        }
    }

    fn open_commit(&mut self, row: usize) {
        if row >= self.store.len() {
            return;
        }
        self.selected = row;
        let oid = self.store.oid(row);
        if self.pane.as_ref().is_some_and(|p| p.oid == oid && p.diff.is_none()) {
            return;
        }
        let req = self.diffs.request_files(oid);
        let meta = self.meta(row);
        self.pane = Some(Pane {
            oid,
            meta,
            files: FilesState::Loading,
            file_sel: 0,
            file_top: 0,
            diff: None,
            req,
        });
    }

    fn open_file(&mut self, index: usize) {
        let Some(pane) = &mut self.pane else { return };
        let FilesState::Ready(files) = &pane.files else { return };
        let Some(file) = files.get(index) else { return };
        // Pour un renommage, l'ancien chemin permet à git de retrouver le lien.
        let mut paths = vec![file.path.clone()];
        paths.extend(file.old_path.clone());
        pane.file_sel = index;
        let req = self.diffs.request_file(pane.oid, paths);
        pane.diff = Some(DiffView { file: index, lines: Vec::new(), done: false, top: 0, x: 0, req });
    }

    fn close_diff(&mut self) {
        if let Some(pane) = &mut self.pane
            && pane.diff.take().is_some()
        {
            self.diffs.cancel();
        }
    }

    fn close_pane(&mut self) {
        self.close_diff();
        self.pane = None;
    }

    fn copy(&mut self, oid: Oid) {
        if self.clipboard.is_none() {
            self.clipboard = arboard::Clipboard::new().ok();
        }
        let copied = self
            .clipboard
            .as_mut()
            .is_some_and(|c| c.set_text(oid.to_hex()).is_ok());
        let text = if copied {
            format!("Copié : {}", oid.to_hex())
        } else {
            "Presse-papiers indisponible".to_string()
        };
        self.flash = Some((text, Instant::now()));
    }

    fn move_by(&mut self, delta: isize) {
        let step = |value: usize, len: usize| -> usize {
            value.saturating_add_signed(delta).min(len.saturating_sub(1))
        };
        match &mut self.pane {
            None => {
                self.selected = step(self.selected, self.store.len());
                self.ensure_visible();
            }
            Some(pane) => match &mut pane.diff {
                Some(diff) => diff.top = step(diff.top, diff.lines.len()),
                None => {
                    if let FilesState::Ready(files) = &pane.files {
                        pane.file_sel = step(pane.file_sel, files.len());
                    }
                }
            },
        }
    }

    pub fn apply(&mut self, action: Action) {
        match action {
            Action::Quit => self.should_quit = true,
            Action::Back { quit } => match self.depth() {
                3 => self.close_diff(),
                2 => self.close_pane(),
                _ => self.should_quit = quit,
            },
            Action::Move(delta) => self.move_by(delta),
            Action::Page(direction) => {
                let page = if self.pane.is_some() { self.pane_height } else { self.graph_height };
                self.move_by(direction * page.max(2) as isize - direction);
            }
            Action::Top => self.move_by(isize::MIN),
            Action::Bottom => {
                if self.pane.is_none() && !self.log_done {
                    // La fin n'est pas encore connue : on demande tout l'historique.
                    self.want_all = true;
                    self.goto_end = true;
                } else {
                    self.move_by(isize::MAX);
                }
            }
            Action::Open => match self.depth() {
                1 => self.open_commit(self.selected),
                2 => {
                    let index = self.pane.as_ref().map_or(0, |p| p.file_sel);
                    self.open_file(index);
                }
                _ => {}
            },
            Action::GoLevel(level) => match (level, self.depth()) {
                (1, _) => self.close_pane(),
                (2, 1) => self.open_commit(self.selected),
                (2, 3) => self.close_diff(),
                (3, 2) => self.apply(Action::Open),
                _ => {}
            },
            Action::ClickCommit(row) => {
                self.close_diff();
                self.open_commit(row);
            }
            Action::ClickFile(index) => self.open_file(index),
            Action::CopySelected => {
                let oid = match &self.pane {
                    Some(pane) => Some(pane.oid),
                    None => (self.selected < self.store.len()).then(|| self.store.oid(self.selected)),
                };
                if let Some(oid) = oid {
                    self.copy(oid);
                }
            }
            Action::CopyRow(row) => {
                if row < self.store.len() {
                    self.selected = row;
                    self.copy(self.store.oid(row));
                }
            }
            Action::ScrollGraph(delta) => {
                self.top = self
                    .top
                    .saturating_add_signed(delta)
                    .min(self.store.len().saturating_sub(1));
            }
            Action::ScrollPane(delta) => match &self.pane {
                Some(pane) if pane.diff.is_some() => self.move_by(delta),
                // Dans la liste des fichiers, la molette déplace la sélection d'un cran.
                Some(_) => self.move_by(delta.signum()),
                None => {}
            },
            Action::Horizontal(delta) => {
                if let Some(diff) = self.pane.as_mut().and_then(|p| p.diff.as_mut()) {
                    diff.x = diff.x.saturating_add_signed(delta);
                }
            }
        }
    }
}
