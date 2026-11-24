//! État de l'application et actions qui le modifient.

use std::collections::HashMap;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::layout::Rect;

use crate::diff::{DiffLine, FileEntry, LineKind, SideBySide};
use crate::git::{self, DiffMsg, DiffWorker, FileReq, Filter, Label, LogMsg, LogStream, Meta, MetaReader, Repo, SearchKind};
use crate::graph::{GraphStore, Oid};
use crate::hierarchy::BranchRow;

/// Lignes gardées d'avance sous le bas de l'écran avant de laisser git en pause.
const LOOKAHEAD: usize = 2000;
/// Lots absorbés au plus par tour de boucle, pour garder l'interface réactive.
const BATCHES_PER_TICK: usize = 64;
/// Entrées du cache de métadonnées avant remise à zéro.
const META_CACHE: usize = 4096;
const FLASH: Duration = Duration::from_millis(1500);
/// Lignes de contexte gardées au-dessus d'une modification atteinte par `n` ou `p`.
const CHANGE_MARGIN: usize = 3;
/// Pause dans la frappe avant de lancer une recherche en direct : un seul git par mot tapé.
const LIVE_DELAY: Duration = Duration::from_millis(200);

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
    /// Fichier entier : se placer sur la première modification dès qu'elle arrive.
    seek: bool,
    req: u64,
}

impl DiffView {
    /// Nombre de lignes affichables dans la vue courante.
    pub fn len(&self) -> usize {
        self.sides.as_ref().map_or(self.lines.len(), |s| s.rows.len())
    }

    /// Vrai si la ligne affichée porte une modification.
    fn changed(&self, row: usize) -> bool {
        match &self.sides {
            Some(sides) => sides.rows.get(row).is_some_and(|(left, right)| left != right),
            None => self.lines.get(row).is_some_and(|l| matches!(l.kind, LineKind::Add | LineKind::Del)),
        }
    }

    /// Position de défilement de chaque bloc de modifications, dans l'ordre :
    /// le bloc s'affiche sous quelques lignes de contexte.
    pub fn change_tops(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.len())
            .filter(|&row| self.changed(row) && (row == 0 || !self.changed(row - 1)))
            .map(|row| row.saturating_sub(CHANGE_MARGIN))
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
    /// Fichiers du commit que la recherche active a écartés de la liste.
    pub hidden_files: usize,
    pub file_sel: usize,
    pub file_top: usize,
    pub diff: Option<DiffView>,
    /// Niveau 4 : le fichier entier du diff ouvert, dans une fenêtre flottante.
    pub full: Option<DiffView>,
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
    /// Nom du dossier et nom du dépôt dans l'en-tête : un clic copie le chemin ou l'URL.
    pub folder: Option<Rect>,
    pub remote: Option<Rect>,
    pub branches: Rect,
    pub branch_top: usize,
}

/// Chemins à donner à git pour un fichier : pour un renommage, l'ancien chemin
/// lui permet de retrouver le lien.
fn file_paths(file: &FileEntry) -> Vec<String> {
    let mut paths = vec![file.path.clone()];
    paths.extend(file.old_path.clone());
    paths
}

/// Recherche en cours de saisie.
pub enum Prompt {
    /// Sous-menu du type de recherche ; `sign` retient un `+` ou un `-` déjà tapé.
    Kind { sign: Option<char> },
    /// Champ de saisie, tout en bas de l'écran.
    Input { kind: SearchKind, query: String },
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
    /// Relit le dépôt : branche courante, refs et historique.
    Refresh,
    /// Affiche ou masque les étiquettes des branches distantes.
    ToggleRemotes,
    ToggleHelp,
    /// Ouvre le sous-menu de recherche.
    Search,
    /// Touches reçues tant qu'une recherche est en cours de saisie.
    PromptKey(char),
    PromptBackspace,
    PromptSubmit,
    PromptCancel,
}

pub struct App {
    pub repo: Repo,
    pub plain: bool,
    /// Coloration syntaxique des diffs ; demande un terminal 24 bits.
    pub syntax: bool,
    pub node_style: NodeStyle,
    /// Étiquettes des branches distantes (`origin/…`) visibles dans le graphe.
    pub show_remotes: bool,
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
    pub prompt: Option<Prompt>,
    /// Recherche appliquée : le graphe cède la place à la liste des commits trouvés.
    pub filter: Option<Filter>,
    /// La liste affichée est encore celle du flux précédent, en attendant les résultats.
    pub stale: bool,
    /// Instant où la recherche en cours de frappe sera lancée.
    live_at: Option<Instant>,
    /// Recherche et commit d'avant l'ouverture du menu, rétablis si la saisie est abandonnée.
    before_search: Option<(Option<Filter>, Option<Oid>)>,
    pub hit: HitMap,
    pub should_quit: bool,
    flash: Option<(String, Instant)>,
    log: Option<LogStream>,
    goto_end: bool,
    /// Commit à atteindre dès qu'il sera chargé, et lignes déjà examinées.
    goto: Option<(Oid, usize)>,
    /// Écart entre la ligne sélectionnée et le haut de l'écran, à rétablir après un rafraîchissement.
    keep_offset: Option<usize>,
    meta_reader: MetaReader,
    meta_cache: HashMap<usize, Meta>,
    diffs: DiffWorker,
    clipboard: Option<arboard::Clipboard>,
}

impl App {
    pub fn new(repo: Repo, plain: bool) -> Result<App> {
        // Le flux démarre en premier : git parcourt pendant que les refs sont lues.
        let log = Some(git::spawn_log(repo.root.clone(), None));
        let labels = git::load_labels(&repo)?;
        Ok(App {
            meta_reader: MetaReader::new(&repo.root)?,
            diffs: DiffWorker::new(repo.clone()),
            repo,
            plain,
            syntax: true,
            node_style: NodeStyle::Dots,
            show_remotes: true,
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
            prompt: None,
            filter: None,
            stale: false,
            live_at: None,
            before_search: None,
            hit: HitMap::default(),
            should_quit: false,
            flash: None,
            log,
            goto_end: false,
            goto: None,
            keep_offset: None,
            meta_cache: HashMap::new(),
            clipboard: None,
        })
    }

    /// Niveau de profondeur courant : 1 graphe, 2 fichiers, 3 diff.
    pub fn depth(&self) -> u8 {
        match &self.pane {
            None => 1,
            Some(pane) if pane.full.is_some() => 4,
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
        let oid = self.store.oid(row);
        let meta = if oid.is_worktree() {
            // Ce qui n'est pas commité n'a ni auteur ni date : seulement un nom.
            Meta { subject: "- changes -".to_string(), ..Meta::default() }
        } else {
            self.meta_reader.get(&oid)
        };
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

    /// Lignes reçues du flux en cours ; celles d'un flux remplacé ne comptent pas.
    fn loaded_rows(&self) -> usize {
        if self.stale { 0 } else { self.store.len() }
    }

    /// Au premier signe du nouveau flux, la liste de l'ancien est enfin vidée.
    fn drop_stale(&mut self) {
        if std::mem::take(&mut self.stale) {
            self.store = GraphStore::default();
            self.meta_cache.clear();
            self.selected = 0;
            self.top = 0;
        }
    }

    /// Vrai tant qu'un travail de fond peut encore changer l'écran.
    pub fn busy(&self) -> bool {
        if self.live_at.is_some() {
            return true;
        }
        let loading_log = !self.log_done && self.loaded_rows() < self.wanted_rows();
        let loading_pane = self.pane.as_ref().is_some_and(|p| {
            matches!(p.files, FilesState::Loading)
                || p.diff.as_ref().is_some_and(|d| !d.done)
                || p.full.as_ref().is_some_and(|d| !d.done)
        });
        let loading_tree = self
            .hierarchy
            .as_ref()
            .is_some_and(|h| matches!(h.state, HierState::Loading));
        loading_log || loading_pane || loading_tree || self.flash.is_some()
    }

    /// Absorbe ce que les threads de fond ont produit. Renvoie vrai s'il faut redessiner.
    pub fn tick(&mut self) -> bool {
        let mut changed = false;
        // La saisie s'est calmée : la recherche en cours de frappe est lancée.
        if self.live_at.is_some_and(|at| at <= Instant::now()) {
            self.live_at = None;
            if let Some(Prompt::Input { kind, query }) = &self.prompt {
                let filter = Filter::new(*kind, query);
                self.set_filter(filter, None, true);
                changed = true;
            }
        }
        changed |= self.pump_log();
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
            if self.log_done || self.loaded_rows() >= wanted {
                break;
            }
            let Some(log) = &self.log else { break };
            match log.rx.try_recv() {
                Ok(LogMsg::Batch(batch)) => {
                    self.drop_stale();
                    self.store.append(batch);
                    changed = true;
                }
                Ok(LogMsg::Error(err)) => {
                    self.drop_stale();
                    self.log_error = Some(err);
                    changed = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.drop_stale();
                    self.log_done = true;
                    self.log = None;
                    changed = true;
                }
            }
        }
        // Tant que l'ancienne liste est à l'écran, le commit n'est pas à y chercher.
        if let Some((oid, scanned)) = self.goto.filter(|_| !self.stale) {
            if let Some(row) = self.store.find(&oid, scanned) {
                self.goto = None;
                self.show_row(row);
                // Après un rafraîchissement, la ligne revient à la hauteur où elle était.
                if let Some(offset) = self.keep_offset.take() {
                    self.top = row.saturating_sub(offset);
                }
                changed = true;
            } else if self.log_done {
                self.goto = None;
                self.keep_offset = None;
                // Le volet montrait ce commit : il n'a plus de ligne à laquelle se rattacher.
                self.close_pane();
                // Plus rien à commiter : la ligne des modifications s'efface sans commentaire.
                if !oid.is_worktree() {
                    self.set_flash("This commit is not in the graph");
                }
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
                            Ok((files, hidden)) => {
                                pane.hidden_files = hidden;
                                FilesState::Ready(files)
                            }
                            Err(err) => FilesState::Failed(err),
                        };
                        changed = true;
                    }
                }
                DiffMsg::Lines { id, lines, done } => {
                    let Some(pane) = &mut self.pane else { continue };
                    let views = [pane.full.as_mut(), pane.diff.as_mut()];
                    if let Some(view) = views.into_iter().flatten().find(|d| d.req == id) {
                        view.append(lines);
                        view.done = done;
                        let first = if view.seek { view.change_tops().next() } else { None };
                        if let Some(first) = first {
                            view.top = first;
                            view.seek = false;
                        }
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
        // Le commit est à chercher dans le graphe complet, pas parmi les résultats d'une recherche.
        self.set_filter(None, None, false);
        match self.store.find(&oid, 0) {
            Some(row) => self.show_row(row),
            None if self.log_done => self.set_flash("This commit is not in the graph"),
            None => {
                self.goto = Some((oid, self.store.len()));
                self.want_all = true;
            }
        }
    }

    /// Recherche sur les fichiers ou les hunks : elle restreint aussi le détail
    /// d'un commit à ce qui y répond.
    fn detail_filter(&self) -> Option<Filter> {
        self.filter.clone().filter(Filter::narrows_detail)
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
        let req = self.diffs.request_files(oid, self.detail_filter());
        let meta = self.meta(row);
        self.pane = Some(Pane {
            oid,
            meta,
            files: FilesState::Loading,
            hidden_files: 0,
            file_sel: 0,
            file_top: 0,
            diff: None,
            full: None,
            req,
        });
    }

    fn open_file(&mut self, index: usize) {
        let filter = self.detail_filter();
        let Some(pane) = &mut self.pane else { return };
        let FilesState::Ready(files) = &pane.files else { return };
        let Some(file) = files.get(index) else { return };
        // Deux colonnes quand les deux versions existent ; une seule pour un ajout ou une suppression.
        let two_way = !matches!(file.status, 'A' | 'D' | '?');
        pane.file_sel = index;
        let req = self.diffs.request_file(FileReq {
            oid: pane.oid,
            paths: file_paths(file),
            full: false,
            syntax: self.syntax,
            filter,
            untracked: file.status == '?',
        });
        pane.full = None;
        pane.diff = Some(DiffView {
            file: index,
            lines: Vec::new(),
            sides: two_way.then(SideBySide::default),
            done: false,
            top: 0,
            x: 0,
            seek: false,
            req,
        });
    }

    /// Niveau 4 : le fichier entier du diff ouvert, modifications en place.
    fn open_full(&mut self) {
        let Some(pane) = &mut self.pane else { return };
        let FilesState::Ready(files) = &pane.files else { return };
        // Les deux lectures passent par le même fil : le diff doit d'abord être complet.
        let Some(diff) = pane.diff.as_ref().filter(|d| d.done) else { return };
        let Some(file) = files.get(diff.file) else { return };
        // Le fichier entier se lit sans tri, même pendant une recherche.
        let req = self.diffs.request_file(FileReq {
            oid: pane.oid,
            paths: file_paths(file),
            full: true,
            syntax: self.syntax,
            filter: None,
            untracked: file.status == '?',
        });
        pane.full = Some(DiffView {
            file: diff.file,
            lines: Vec::new(),
            sides: diff.sides.is_some().then(SideBySide::default),
            done: false,
            top: 0,
            x: 0,
            seek: true,
            req,
        });
    }

    fn close_full(&mut self) {
        if let Some(pane) = &mut self.pane
            && let Some(full) = pane.full.take()
            && !full.done
        {
            self.diffs.cancel();
        }
    }

    /// `n` / `N` dans le fichier entier : modification suivante ou précédente.
    fn step_change(&mut self, delta: isize) {
        let Some(view) = self.pane.as_mut().and_then(|p| p.full.as_mut()) else { return };
        let top = view.top;
        let target = if delta > 0 {
            view.change_tops().find(|&t| t > top)
        } else {
            view.change_tops().take_while(|&t| t < top).last()
        };
        match target {
            Some(target) => view.top = target,
            None if delta < 0 => self.set_flash("First change"),
            // Tant que le fichier arrive, la suite peut encore en contenir.
            None if view.done => self.set_flash("Last change"),
            None => {}
        }
    }

    fn close_diff(&mut self) {
        if let Some(pane) = &mut self.pane {
            let full = pane.full.take().is_some();
            if pane.diff.take().is_some() || full {
                self.diffs.cancel();
            }
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

    /// Saisie d'une recherche : le sous-menu choisit le type, puis le champ reçoit le texte.
    fn apply_in_prompt(&mut self, action: Action) {
        let Some(prompt) = self.prompt.take() else { return };
        // Les recherches rapides se relancent d'elles-mêmes, un instant après la dernière frappe.
        let live = |kind: SearchKind| kind.is_live().then(|| Instant::now() + LIVE_DELAY);
        self.prompt = match (prompt, action) {
            (_, Action::Quit) => {
                self.should_quit = true;
                None
            }
            (_, Action::PromptCancel) => {
                // Abandon : retour à la recherche, et au commit, d'avant l'ouverture du menu.
                self.live_at = None;
                if let Some((filter, oid)) = self.before_search.take() {
                    self.set_filter(filter, oid, false);
                }
                None
            }
            (Prompt::Kind { sign }, Action::PromptKey(key)) => {
                let kind = match (sign, key) {
                    (None, 'c') => Some(SearchKind::Hash),
                    (None, 'm') => Some(SearchKind::Message),
                    (None, 'f') => Some(SearchKind::File),
                    (None, 'h') => Some(SearchKind::Hunk),
                    (Some('+'), 'h') => Some(SearchKind::HunkAdded),
                    (Some('-'), 'h') => Some(SearchKind::HunkRemoved),
                    _ => None,
                };
                match kind {
                    Some(kind) => Some(Prompt::Input { kind, query: String::new() }),
                    // `+` ou `-` attend son `h` ; toute autre touche laisse le menu en place.
                    None if matches!(key, '+' | '-') => Some(Prompt::Kind { sign: Some(key) }),
                    None => Some(Prompt::Kind { sign }),
                }
            }
            (Prompt::Kind { .. }, Action::PromptBackspace) => Some(Prompt::Kind { sign: None }),
            (Prompt::Input { kind, mut query }, Action::PromptKey(key)) => {
                query.push(key);
                self.live_at = live(kind);
                Some(Prompt::Input { kind, query })
            }
            (Prompt::Input { kind, mut query }, Action::PromptBackspace) => {
                query.pop();
                self.live_at = live(kind);
                Some(Prompt::Input { kind, query })
            }
            (Prompt::Input { kind, query }, Action::PromptSubmit) => {
                // Entrée lance la recherche sans attendre ; un champ vide revient au graphe complet.
                self.live_at = None;
                self.before_search = None;
                self.set_filter(Filter::new(kind, &query), None, kind.is_live());
                None
            }
            (prompt, _) => Some(prompt),
        };
    }

    /// Commit sélectionné, s'il y en a un.
    fn selected_oid(&self) -> Option<Oid> {
        (self.selected < self.store.len()).then(|| self.store.oid(self.selected))
    }

    /// Applique ou retire une recherche : l'historique est relu depuis le début.
    /// `goto` est le commit à retrouver dans la nouvelle liste. `soft` garde l'ancienne
    /// liste à l'écran jusqu'aux premiers résultats, pour une saisie sans clignotement.
    fn set_filter(&mut self, filter: Option<Filter>, goto: Option<Oid>, soft: bool) {
        if self.filter == filter {
            return;
        }
        self.close_pane();
        self.filter = filter;
        // Un commit à retrouver passe avant le confort de la saisie : la liste repart de zéro.
        self.restart_log(goto, soft && goto.is_none());
    }

    /// Relit l'historique depuis le début, avec la recherche en place. `goto` est le
    /// commit à retrouver ; `soft` garde l'ancienne liste à l'écran jusqu'aux
    /// premières lignes de la nouvelle.
    fn restart_log(&mut self, goto: Option<Oid>, soft: bool) {
        self.stale = soft;
        if !self.stale {
            self.store = GraphStore::default();
            self.meta_cache.clear();
            self.selected = 0;
            self.top = 0;
        }
        self.log_done = false;
        self.log_error = None;
        self.goto_end = false;
        self.goto = goto.map(|oid| (oid, 0));
        self.want_all = self.goto.is_some();
        // L'ancien flux est lâché en premier : son git s'arrête, même en pleine recherche.
        self.log = None;
        self.log = Some(git::spawn_log(self.repo.root.clone(), self.filter.clone()));
    }

    /// `R` : relit le dépôt, que d'autres commandes ont pu faire bouger. La branche
    /// courante, les refs et l'historique sont rechargés ; la recherche, le commit
    /// sélectionné et le volet ouvert restent en place.
    fn refresh(&mut self) {
        let reloaded = Repo::open(&self.repo.root).and_then(|repo| Ok((git::load_labels(&repo)?, repo)));
        match reloaded {
            Ok((labels, repo)) => {
                self.repo = repo;
                self.labels = labels;
            }
            Err(err) => return self.set_flash(format!("Refresh failed: {err:#}")),
        }
        if self.hierarchy.is_some() {
            self.hierarchy = None;
            self.toggle_hierarchy();
        }
        // Le commit affiché garde sa place à l'écran, même si sa ligne change. En haut de
        // l'historique, la vue y reste : c'est là qu'arrivent les nouveaux commits.
        // Le détail d'un commit ne change pas, celui du dossier de travail si : il est relu.
        if let Some(oid) = self.pane.as_ref().map(|pane| pane.oid).filter(|oid| oid.is_worktree()) {
            self.close_diff();
            let req = self.diffs.request_files(oid, None);
            if let Some(pane) = &mut self.pane {
                pane.files = FilesState::Loading;
                pane.req = req;
            }
        }
        let current = self.pane.as_ref().map(|pane| pane.oid).or_else(|| self.selected_oid());
        self.keep_offset = current.filter(|_| self.top > 0).map(|_| self.selected.saturating_sub(self.top));
        self.restart_log(current, true);
        self.set_flash("Refreshed");
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
        let oid = match &self.pane {
            Some(pane) => Some(pane.oid),
            None => self.selected_oid(),
        };
        // La ligne des modifications non commitées n'a pas de hash.
        oid.filter(|oid| !oid.is_worktree()).map(Oid::to_hex)
    }

    /// Nombre de commits chargés, sans la ligne des modifications non commitées.
    pub fn commit_count(&self) -> usize {
        let changes = self.store.len() > 0 && self.store.oid(0).is_worktree();
        self.store.len() - usize::from(changes)
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

    /// `n` / `N` : fichier voisin dans un diff, commit voisin dans la liste des fichiers.
    fn step(&mut self, delta: isize) {
        let neighbour = |value: usize, len: usize| value.saturating_add_signed(delta).min(len.saturating_sub(1));
        match self.depth() {
            4 => self.step_change(delta),
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
            Some(pane) => match pane.full.as_mut().or(pane.diff.as_mut()) {
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
        if self.prompt.is_some() {
            return self.apply_in_prompt(action);
        }
        if self.copy_from_header(&action) {
            return;
        }
        // Valable dans toutes les vues, hiérarchie des branches comprise.
        if matches!(action, Action::Refresh) {
            return self.refresh();
        }
        if matches!(action, Action::ToggleRemotes) {
            self.show_remotes = !self.show_remotes;
            let state = if self.show_remotes { "shown" } else { "hidden" };
            return self.set_flash(format!("Remote branches {state}"));
        }
        if self.hierarchy.is_some() {
            return self.apply_in_hierarchy(action);
        }
        match action {
            Action::Quit => self.should_quit = true,
            Action::Back { quit } => match self.depth() {
                4 => self.close_full(),
                3 => self.close_diff(),
                2 => self.close_pane(),
                // Une recherche active est un niveau de plus : on en sort d'abord.
                // En la quittant, on retrouve le commit sélectionné dans le graphe complet.
                _ if self.filter.is_some() => self.set_filter(None, self.selected_oid(), false),
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
                3 => self.open_full(),
                _ => {}
            },
            Action::GoLevel(level) => match (level, self.depth()) {
                (1, _) => self.close_pane(),
                (2, 1) => self.open_commit(self.selected),
                (2, 3 | 4) => self.close_diff(),
                (3, 4) => self.close_full(),
                (3, 2) | (4, 3) => self.apply(Action::Open),
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
                if let Some(diff) = self.pane.as_mut().and_then(|p| p.full.as_mut().or(p.diff.as_mut())) {
                    diff.x = diff.x.saturating_add_signed(delta);
                }
            }
            Action::ToggleHierarchy => self.toggle_hierarchy(),
            Action::Search => {
                if self.pane.is_none() {
                    self.before_search = Some((self.filter.clone(), self.selected_oid()));
                    self.prompt = Some(Prompt::Kind { sign: None });
                }
            }
            // Hors saisie, ces touches n'arrivent pas jusqu'ici.
            Action::PromptKey(_) | Action::PromptBackspace | Action::PromptSubmit | Action::PromptCancel => {}
            // Traitées plus haut, dans toutes les vues.
            Action::Refresh | Action::ToggleRemotes => {}
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
