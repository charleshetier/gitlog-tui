//! État de l'application et actions qui le modifient.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::layout::Rect;

use crate::diff::{DiffLine, FileEntry, SideBySide};
use crate::git::{self, DiffMsg, DiffWorker, Label, LogMsg, Meta, MetaReader, Repo};
use crate::graph::{GraphStore, Oid};
use crate::hierarchy::BranchRow;

/// Lignes gardées d'avance sous le bas de l'écran avant de laisser git en pause.
const LOOKAHEAD: usize = 2000;
/// Lots absorbés au plus par tour de boucle, pour garder l'interface réactive.
const BATCHES_PER_TICK: usize = 64;
/// Entrées du cache de métadonnées avant remise à zéro.
const META_CACHE: usize = 4096;
const FLASH: Duration = Duration::from_millis(1500);

/// Dessin d'un commit dans le graphe. Un terminal ne superpose pas une puce
/// et une ligne dans la même cellule : le rail est l'autre façon de les relier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeStyle {
    /// Puce seule dans la colonne, sans ligne dans sa cellule.
    Dots,
    /// Ligne continue à gauche, puce accrochée à droite.
    Rail,
}

impl NodeStyle {
    pub fn label(self) -> &'static str {
        match self {
            NodeStyle::Rail => "rail",
            NodeStyle::Dots => "dots",
        }
    }
}

pub enum FilesState {
    Loading,
    Ready(Vec<FileEntry>),
    Failed(String),
}

/// Niveau 3 : le diff d'un fichier.
pub struct DiffView {
    pub file: usize,
    pub lines: Vec<DiffLine>,
    /// Mise en regard ancien / nouveau, pour un fichier modifié.
    pub sides: Option<SideBySide>,
    pub done: bool,
    pub top: usize,
    pub x: usize,
    req: u64,
}

impl DiffView {
    /// Nombre de lignes affichables dans la vue courante.
    pub fn len(&self) -> usize {
        self.sides.as_ref().map_or(self.lines.len(), |s| s.rows.len())
    }

    fn append(&mut self, lines: Vec<DiffLine>) {
        if let Some(sides) = &mut self.sides {
            for (i, line) in lines.iter().enumerate() {
                sides.push((self.lines.len() + i) as u32, line.kind);
            }
        }
        self.lines.extend(lines);
    }
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

pub enum HierState {
    Loading,
    Ready { rows: Vec<BranchRow>, hidden: usize },
    Failed(String),
}

/// Vue plein écran de la hiérarchie des branches.
pub struct Hierarchy {
    pub state: HierState,
    pub sel: usize,
    pub top: usize,
    req: u64,
}

/// Zones cliquables mémorisées au dernier rendu.
#[derive(Default)]
pub struct HitMap {
    pub graph: Rect,
    pub graph_top: usize,
    pub pane: Rect,
    pub files: Rect,
    pub file_top: usize,
    pub chips: Vec<(Rect, u8)>,
    pub pane_copy: Option<Rect>,
    /// Rappel « ? » de la barre d'état, qui ouvre le panneau des raccourcis.
    pub help: Option<Rect>,
    /// Nom du dossier et nom du dépôt dans l'en-tête : un clic copie le chemin ou l'URL.
    pub folder: Option<Rect>,
    pub remote: Option<Rect>,
    pub branches: Rect,
    pub branch_top: usize,
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
    ClickBranch(usize),
    /// Copie ce qui est sous le curseur : le chemin d'un fichier, sinon le hash du commit.
    CopySelected,
    /// Copie le hash du commit affiché, quel que soit le niveau.
    CopyHash,
    /// Copie le chemin du dossier du dépôt.
    CopyFolder,
    /// Copie l'URL du remote `origin`.
    CopyRemote,
    /// Élément voisin : fichier dans un diff, commit dans la liste des fichiers.
    Step(isize),
    ScrollGraph(isize),
    ScrollPane(isize),
    Horizontal(isize),
    ToggleHierarchy,
    CycleNodes,
    ToggleHelp,
}

pub struct App {
    pub repo: Repo,
    pub plain: bool,
    pub node_style: NodeStyle,
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
    pub hierarchy: Option<Hierarchy>,
    /// Panneau flottant des raccourcis.
    pub help: bool,
    pub hit: HitMap,
    pub should_quit: bool,
    flash: Option<(String, Instant)>,
    log_rx: Option<Receiver<LogMsg>>,
    goto_end: bool,
    /// Commit à atteindre dès qu'il sera chargé, et lignes déjà examinées.
    goto: Option<(Oid, usize)>,
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
            diffs: DiffWorker::new(repo.clone()),
            repo,
            plain,
            node_style: NodeStyle::Dots,
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
            hierarchy: None,
            help: false,
            hit: HitMap::default(),
            should_quit: false,
            flash: None,
            log_rx,
            goto_end: false,
            goto: None,
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

    fn set_flash(&mut self, text: impl Into<String>) {
        self.flash = Some((text.into(), Instant::now()));
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
        let loading_tree = self
            .hierarchy
            .as_ref()
            .is_some_and(|h| matches!(h.state, HierState::Loading));
        loading_log || loading_pane || loading_tree || self.flash.is_some()
    }

    /// Absorbe ce que les threads de fond ont produit. Renvoie vrai s'il faut redessiner.
    pub fn tick(&mut self) -> bool {
        let mut changed = self.pump_log();
        changed |= self.pump_worker();
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
        if let Some((oid, scanned)) = self.goto {
            if let Some(row) = self.store.find(&oid, scanned) {
                self.goto = None;
                self.show_row(row);
                changed = true;
            } else if self.log_done {
                self.goto = None;
                self.set_flash("This commit is not in the graph");
                changed = true;
            } else {
                self.goto = Some((oid, self.store.len()));
            }
        }
        if self.goto_end && self.log_done {
            self.goto_end = false;
            self.selected = self.store.len().saturating_sub(1);
            self.ensure_visible();
            changed = true;
        }
        self.want_all = !self.log_done && (self.goto.is_some() || self.goto_end);
        changed
    }

    fn pump_worker(&mut self) -> bool {
        let mut changed = false;
        while let Ok(msg) = self.diffs.rx.try_recv() {
            match msg {
                DiffMsg::Files { id, result } => {
                    if let Some(pane) = self.pane.as_mut().filter(|p| p.req == id) {
                        pane.files = match result {
                            Ok(files) => FilesState::Ready(files),
                            Err(err) => FilesState::Failed(err),
                        };
                        changed = true;
                    }
                }
                DiffMsg::Lines { id, lines, done } => {
                    let diff = self.pane.as_mut().and_then(|p| p.diff.as_mut());
                    if let Some(diff) = diff.filter(|d| d.req == id) {
                        diff.append(lines);
                        diff.done = done;
                        changed = true;
                    }
                }
                DiffMsg::Hierarchy { id, result } => {
                    if let Some(view) = self.hierarchy.as_mut().filter(|h| h.req == id) {
                        view.state = match result {
                            Ok((rows, hidden)) => {
                                // La sélection démarre sur la branche courante.
                                view.sel = rows.iter().position(|r| r.branch.is_head).unwrap_or(0);
                                HierState::Ready { rows, hidden }
                            }
                            Err(err) => HierState::Failed(err),
                        };
                        changed = true;
                    }
                }
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

    /// Sélectionne une ligne et la place dans le tiers haut de l'écran.
    fn show_row(&mut self, row: usize) {
        self.selected = row;
        if row < self.top || row >= self.top + self.graph_height.max(1) {
            self.top = row.saturating_sub(self.graph_height / 3);
        }
    }

    /// Va au commit donné, en chargeant l'historique jusqu'à lui s'il le faut.
    fn goto_commit(&mut self, oid: Oid) {
        self.hierarchy = None;
        self.close_pane();
        match self.store.find(&oid, 0) {
            Some(row) => self.show_row(row),
            None if self.log_done => self.set_flash("This commit is not in the graph"),
            None => {
                self.goto = Some((oid, self.store.len()));
                self.want_all = true;
            }
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
        // Deux colonnes quand les deux versions existent ; une seule pour un ajout ou une suppression.
        let two_way = !matches!(file.status, 'A' | 'D');
        pane.file_sel = index;
        let req = self.diffs.request_file(pane.oid, paths);
        pane.diff = Some(DiffView {
            file: index,
            lines: Vec::new(),
            sides: two_way.then(SideBySide::default),
            done: false,
            top: 0,
            x: 0,
            req,
        });
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

    fn copy(&mut self, text: String) {
        if self.clipboard.is_none() {
            self.clipboard = arboard::Clipboard::new().ok();
        }
        let copied = self
            .clipboard
            .as_mut()
            .is_some_and(|c| c.set_text(text.as_str()).is_ok());
        if copied {
            self.set_flash(format!("Copied: {text}"));
        } else {
            self.set_flash("Clipboard unavailable");
        }
    }

    /// Chemin du dossier du dépôt, avec les séparateurs du système.
    pub fn folder_path(&self) -> String {
        let path = self.repo.root.display().to_string();
        if cfg!(windows) { path.replace('/', "\\") } else { path }
    }

    /// Copies déclenchées depuis l'en-tête, valables dans toutes les vues.
    fn copy_from_header(&mut self, action: &Action) -> bool {
        let text = match action {
            Action::CopyFolder => Some(self.folder_path()),
            Action::CopyRemote => self.repo.remote_url.clone(),
            _ => return false,
        };
        if let Some(text) = text {
            self.copy(text);
        }
        true
    }

    /// Hash du commit affiché : celui du volet, sinon celui de la ligne sélectionnée.
    fn current_hash(&self) -> Option<String> {
        match &self.pane {
            Some(pane) => Some(pane.oid.to_hex()),
            None => (self.selected < self.store.len()).then(|| self.store.oid(self.selected).to_hex()),
        }
    }

    /// Texte copié par `y` : le chemin du fichier sous le curseur, sinon le hash du commit.
    pub fn copy_target(&self) -> Option<String> {
        let file = self.pane.as_ref().and_then(|pane| {
            let FilesState::Ready(files) = &pane.files else { return None };
            files.get(pane.diff.as_ref().map_or(pane.file_sel, |d| d.file))
        });
        match file {
            Some(file) => Some(file.path.clone()),
            None => self.current_hash(),
        }
    }

    /// `n` / `p` : fichier voisin dans un diff, commit voisin dans la liste des fichiers.
    fn step(&mut self, delta: isize) {
        let neighbour = |value: usize, len: usize| value.saturating_add_signed(delta).min(len.saturating_sub(1));
        match self.depth() {
            3 => {
                let Some(pane) = &self.pane else { return };
                let FilesState::Ready(files) = &pane.files else { return };
                let current = pane.diff.as_ref().map_or(0, |d| d.file);
                let next = neighbour(current, files.len());
                if next != current {
                    self.open_file(next);
                } else {
                    self.set_flash(if delta < 0 { "First file of the commit" } else { "Last file of the commit" });
                }
            }
            2 => {
                let next = neighbour(self.selected, self.store.len());
                if next != self.selected {
                    self.open_commit(next);
                    self.ensure_visible();
                } else if delta < 0 || self.log_done {
                    self.set_flash(if delta < 0 { "First commit" } else { "Last commit" });
                }
            }
            _ => self.move_by(delta),
        }
    }

    fn move_by(&mut self, delta: isize) {
        let step = |value: usize, len: usize| -> usize {
            value.saturating_add_signed(delta).min(len.saturating_sub(1))
        };
        if let Some(view) = &mut self.hierarchy {
            if let HierState::Ready { rows, .. } = &view.state {
                view.sel = step(view.sel, rows.len());
            }
            return;
        }
        match &mut self.pane {
            None => {
                self.selected = step(self.selected, self.store.len());
                self.ensure_visible();
            }
            Some(pane) => match &mut pane.diff {
                Some(diff) => diff.top = step(diff.top, diff.len()),
                None => {
                    if let FilesState::Ready(files) = &pane.files {
                        pane.file_sel = step(pane.file_sel, files.len());
                    }
                }
            },
        }
    }

    fn toggle_hierarchy(&mut self) {
        if self.hierarchy.take().is_none() {
            // Recalculée à chaque ouverture : les branches bougent hors de gitlog.
            let req = self.diffs.request_hierarchy();
            self.hierarchy = Some(Hierarchy { state: HierState::Loading, sel: 0, top: 0, req });
        }
    }

    /// Actions de la vue hiérarchie, qui recouvre le reste de l'écran.
    fn apply_in_hierarchy(&mut self, action: Action) {
        match action {
            Action::Quit => self.should_quit = true,
            Action::Back { .. } | Action::ToggleHierarchy => self.hierarchy = None,
            Action::Move(delta) | Action::ScrollGraph(delta) | Action::Step(delta) => self.move_by(delta),
            Action::Page(direction) => self.move_by(direction * self.pane_height.max(2) as isize - direction),
            Action::Top => self.move_by(isize::MIN),
            Action::Bottom => self.move_by(isize::MAX),
            Action::ClickBranch(index) => {
                if let Some(view) = &mut self.hierarchy {
                    view.sel = index;
                }
            }
            Action::Open => {
                let oid = self.hierarchy.as_ref().and_then(|view| match &view.state {
                    HierState::Ready { rows, .. } => rows.get(view.sel).map(|r| r.branch.oid),
                    _ => None,
                });
                if let Some(oid) = oid {
                    self.goto_commit(oid);
                }
            }
            _ => {}
        }
    }

    pub fn apply(&mut self, action: Action) {
        // Le panneau des raccourcis recouvre tout : il ne laisse passer que sa fermeture.
        if self.help || matches!(action, Action::ToggleHelp) {
            match action {
                Action::Quit => self.should_quit = true,
                Action::ToggleHelp => self.help = !self.help,
                Action::Back { .. } => self.help = false,
                _ => {}
            }
            return;
        }
        if self.copy_from_header(&action) {
            return;
        }
        if self.hierarchy.is_some() {
            return self.apply_in_hierarchy(action);
        }
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
            // Déjà traitées plus haut, quelle que soit la vue.
            Action::ClickBranch(_) | Action::ToggleHelp | Action::CopyFolder | Action::CopyRemote => {}
            Action::CopySelected => {
                if let Some(text) = self.copy_target() {
                    self.copy(text);
                }
            }
            Action::CopyHash => {
                if let Some(hash) = self.current_hash() {
                    self.copy(hash);
                }
            }
            Action::Step(delta) => self.step(delta),
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
            Action::ToggleHierarchy => self.toggle_hierarchy(),
            Action::CycleNodes => {
                self.node_style = match self.node_style {
                    NodeStyle::Dots => NodeStyle::Rail,
                    NodeStyle::Rail => NodeStyle::Dots,
                };
                self.set_flash(format!("Commit style: {}", self.node_style.label()));
            }
        }
    }
}
