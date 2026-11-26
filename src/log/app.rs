//! État de l'application et actions qui le modifient.

use std::collections::{BTreeSet, HashMap};
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::layout::{Position, Rect};

use crate::kit::Tool;
use crate::kit::find::Needle;
use crate::kit::notice::{Clipboard, Flash};
use crate::log::diff::{DiffLine, FileEntry, LineKind, SideBySide};
use crate::log::git::{
    self, DiffMsg, DiffWorker, FileReq, Filter, Head, Label, LogMsg, LogStream, Meta, MetaReader, RefKind, Repo, Revs,
    SearchKind,
};
use crate::log::graph::{GraphStore, Oid};
use crate::log::hierarchy::BranchRow;

/// Lignes gardées d'avance sous le bas de l'écran avant de laisser git en pause.
const LOOKAHEAD: usize = 2000;
/// Lots absorbés au plus par tour de boucle, pour garder l'interface réactive.
const BATCHES_PER_TICK: usize = 64;
/// Entrées du cache de métadonnées avant remise à zéro.
const META_CACHE: usize = 4096;
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

/// Côté d'un diff. En une seule colonne, tout le code est à gauche.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// Code sélectionné à la souris dans un diff : d'un caractère à un autre, sur un
/// seul côté. Chaque borne donne la ligne de la vue et le rang du caractère.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub side: Side,
    /// Là où le bouton a été enfoncé, puis là où se trouve le pointeur.
    anchor: (usize, usize),
    head: (usize, usize),
}

impl Selection {
    /// Caractères sélectionnés sur une ligne de la vue : du premier, inclus, au
    /// dernier, exclu ; `usize::MAX` quand la sélection continue à la ligne suivante.
    pub fn columns(&self, side: Side, row: usize) -> Option<(usize, usize)> {
        let (start, end) = (self.anchor.min(self.head), self.anchor.max(self.head));
        // Un clic sans déplacement ne sélectionne rien.
        if side != self.side || start == end || row < start.0 || row > end.0 {
            return None;
        }
        let from = if row == start.0 { start.1 } else { 0 };
        let to = if row == end.0 { end.1 + 1 } else { usize::MAX };
        Some((from, to))
    }
}

/// Occurrence d'une recherche dans le code : la ligne de la vue, le côté
/// (`None` pour une ligne commune aux deux côtés ou pour la colonne unique) et
/// les caractères trouvés.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub row: usize,
    pub side: Option<Side>,
    pub start: usize,
    pub len: usize,
}

/// Recherche de texte dans le code d'un diff, ouverte par `/`.
#[derive(Clone, Debug)]
pub struct Find {
    pub query: String,
    /// Occurrences, dans l'ordre de lecture.
    pub hits: Vec<Hit>,
    /// Occurrence où l'on se trouve, parmi `hits`.
    pub current: Option<usize>,
}

impl Find {
    /// Occurrences d'une ligne de la vue, d'un côté : début, longueur, et si
    /// c'est l'occurrence courante.
    pub fn on(&self, row: usize, side: Side) -> impl Iterator<Item = (usize, usize, bool)> + '_ {
        let first = self.hits.partition_point(|hit| hit.row < row);
        self.hits[first..]
            .iter()
            .enumerate()
            .take_while(move |(_, hit)| hit.row == row)
            .filter(move |(_, hit)| hit.side.is_none_or(|s| s == side))
            .map(move |(i, hit)| (hit.start, hit.len, self.current == Some(first + i)))
    }
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
    pub select: Option<Selection>,
    pub find: Option<Find>,
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

    /// Ligne de code d'une ligne de la vue, du côté demandé : ni un en-tête de
    /// hunk, ni le vide laissé en face d'un ajout ou d'un retrait.
    fn code(&self, row: usize, side: Side) -> Option<&DiffLine> {
        let index = match &self.sides {
            Some(sides) => {
                let &(left, right) = sides.rows.get(row)?;
                if side == Side::Left { left } else { right }
            }
            None => row as u32,
        };
        let line = self.lines.get(index as usize)?;
        matches!(line.kind, LineKind::Ctx | LineKind::Add | LineKind::Del).then_some(line)
    }

    /// Texte de la sélection, ligne de code par ligne de code, sans les numéros.
    pub fn selected_text(&self) -> Option<String> {
        let select = self.select?;
        let (first, last) = (select.anchor.0.min(select.head.0), select.anchor.0.max(select.head.0));
        let mut out: Vec<String> = Vec::new();
        for row in first..=last {
            if let (Some((from, to)), Some(line)) = (select.columns(select.side, row), self.code(row, select.side)) {
                out.push(line.text.chars().skip(from).take(to.saturating_sub(from)).collect());
            }
        }
        out.iter().any(|line| !line.is_empty()).then(|| out.join("\n"))
    }

    /// Occurrences de `query` dans le code. Sans majuscule dans la recherche, la
    /// casse est ignorée.
    fn search(&self, query: &str) -> Vec<Hit> {
        let needle = Needle::new(query);
        let mut hits = Vec::new();
        if needle.is_empty() {
            return hits;
        }
        for row in 0..self.len() {
            // Une ligne commune aux deux côtés ne compte qu'une fois.
            let sides: &[Option<Side>] = match &self.sides {
                Some(sides) if sides.rows[row].0 != sides.rows[row].1 => &[Some(Side::Left), Some(Side::Right)],
                _ => &[None],
            };
            for &side in sides {
                let Some(line) = self.code(row, side.unwrap_or(Side::Left)) else { continue };
                hits.extend(needle.starts(&line.text).into_iter().map(|start| Hit { row, side, start, len: needle.len() }));
            }
        }
        hits
    }

    /// Pose la recherche, ou la retire si `query` est vide. L'occurrence courante
    /// est la première à partir du haut de la vue.
    fn set_find(&mut self, query: &str) {
        let hits = self.search(query);
        let current = hits.iter().position(|hit| hit.row >= self.top).or((!hits.is_empty()).then_some(0));
        self.find = (!query.is_empty()).then(|| Find { query: query.to_string(), hits, current });
    }

    /// Amène l'occurrence courante à l'écran : `height` lignes sont visibles,
    /// et `code` dit la largeur du code affiché.
    fn reveal(&mut self, height: usize, code: Option<&CodeHit>) {
        let Some(hit) = self.find.as_ref().and_then(|find| find.hits.get(find.current?)).copied() else { return };
        // La dernière ligne peut passer sous le champ de recherche.
        if hit.row < self.top || hit.row + 1 >= self.top + height.max(2) {
            self.top = hit.row.saturating_sub(CHANGE_MARGIN);
        }
        // Replié, le texte ne défile pas de côté : toute la ligne est à l'écran.
        let Some(code) = code.filter(|code| !code.wrap) else { return };
        let width = code.width(hit.side.unwrap_or(Side::Left));
        if hit.start < self.x || hit.start + hit.len > self.x + width {
            self.x = if hit.start + hit.len <= width { 0 } else { hit.start.saturating_sub(width / 3) };
        }
    }

    fn append(&mut self, lines: Vec<DiffLine>) {
        if let Some(sides) = &mut self.sides {
            for (i, line) in lines.iter().enumerate() {
                sides.push((self.lines.len() + i) as u32, line.kind);
            }
        }
        self.lines.extend(lines);
        // Le diff s'allonge : la recherche en cours suit, sans perdre sa place.
        if let Some(find) = self.find.take() {
            let at = find.current.and_then(|current| find.hits.get(current).copied());
            let hits = self.search(&find.query);
            let current = at.and_then(|at| hits.iter().position(|hit| *hit == at));
            self.find = Some(Find { query: find.query, hits, current });
        }
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

/// Code d'un diff à l'écran : de quoi traduire un point en position dans le texte.
#[derive(Clone, Debug)]
pub struct CodeHit {
    /// Lignes du diff affichées.
    pub area: Rect,
    /// Pour chaque rangée de l'écran : la ligne de la vue qu'elle montre, et son
    /// rang parmi les rangées de cette ligne quand le texte est replié.
    pub lines: Vec<(usize, usize)>,
    /// Le texte est replié à la largeur du code ; sinon il défile de `skip` caractères.
    pub wrap: bool,
    pub skip: usize,
    /// Première colonne du code, à gauche ou dans la colonne unique.
    pub left: u16,
    /// En deux colonnes : le séparateur, puis la première colonne du code à droite.
    pub right: Option<(u16, u16)>,
    /// Ligne de l'écran à partir de laquelle une sélection fait défiler vers le
    /// bas : la dernière du diff quand rien ne le suit à l'écran.
    pub lower_edge: u16,
}

impl CodeHit {
    fn side(&self, x: u16) -> Side {
        match self.right {
            Some((mid, _)) if x > mid => Side::Right,
            _ => Side::Left,
        }
    }

    /// Colonnes de l'écran occupées par le code d'un côté : la première, puis
    /// celle qui suit la dernière.
    fn columns(&self, side: Side) -> (u16, u16) {
        match (side, self.right) {
            (Side::Right, Some((_, code))) => (code, self.area.right().saturating_sub(1)),
            (_, Some((mid, _))) => (self.left, mid),
            (_, None) => (self.left, self.area.right().saturating_sub(1)),
        }
    }

    /// Nombre de caractères de code visibles d'un côté.
    fn width(&self, side: Side) -> usize {
        let (start, end) = self.columns(side);
        end.saturating_sub(start) as usize
    }

    /// Ligne de la vue et rang du caractère sous un point. Hors du code de ce
    /// côté, le point est ramené à son bord le plus proche.
    fn locate(&self, pos: Position, side: Side) -> (usize, usize) {
        let (start, end) = self.columns(side);
        let x = pos.x.min(end.saturating_sub(1)).max(start);
        let y = pos.y.min(self.area.bottom().saturating_sub(1)).max(self.area.y);
        // Sous la dernière rangée dessinée, le point vaut pour celle-ci.
        let line = ((y - self.area.y) as usize).min(self.lines.len().saturating_sub(1));
        let (row, part) = self.lines.get(line).copied().unwrap_or_default();
        // Une ligne repliée reprend, à chaque rangée, là où la précédente s'arrête.
        let skip = if self.wrap { part * self.width(side) } else { self.skip };
        (row, skip + (x - start) as usize)
    }
}

/// Branches dont part le graphe, selon leur nature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Les branches locales et les branches distantes.
    All,
    /// Sans les branches distantes (`origin/…`).
    Local,
    /// Les branches distantes seules.
    Remote,
}

/// Une branche : sa nature, locale ou distante, et son nom.
pub type Branch = (RefKind, String);

/// Liste des branches à cocher pour le graphe, ouverte par `b` `f`.
pub struct Picker {
    /// Toutes les branches : les locales, puis les distantes.
    pub items: Vec<Branch>,
    pub checked: Vec<bool>,
    pub sel: usize,
    pub top: usize,
    /// Lignes visibles, renseignées par le rendu.
    pub height: usize,
    /// Recherche dans la liste, ouverte par `/` : elle ne filtre pas, elle
    /// surligne les branches trouvées et y amène le curseur.
    pub query: String,
    /// La recherche est en cours de saisie.
    pub typing: bool,
    /// Curseur d'avant la saisie, d'où part la recherche.
    origin: usize,
}

impl Picker {
    /// La branche répond-elle à la recherche ? Sans majuscule dans celle-ci, la
    /// casse est ignorée.
    pub fn matches(&self, index: usize) -> bool {
        let name = &self.items[index].1;
        if self.query.is_empty() {
            false
        } else if self.query.chars().any(char::is_uppercase) {
            name.contains(&self.query)
        } else {
            name.to_lowercase().contains(&self.query)
        }
    }

    /// Branche trouvée la plus proche de `from`, dans le sens de `delta`, en boucle.
    fn seek(&self, from: usize, delta: isize) -> Option<usize> {
        let len = self.items.len() as isize;
        (0..len).map(|step| (from as isize + delta * step).rem_euclid(len) as usize).find(|&index| self.matches(index))
    }

    fn step(&mut self, delta: isize) {
        self.sel = self.sel.saturating_add_signed(delta).min(self.items.len().saturating_sub(1));
    }
}

/// Zones cliquables mémorisées au dernier rendu.
#[derive(Default)]
pub struct HitMap {
    pub graph: Rect,
    /// Ligne du graphe montrée par chaque rangée de l'écran : une ligne repliée en occupe plusieurs.
    pub graph_rows: Vec<usize>,
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
    /// Code du diff au premier plan, où la souris sélectionne du texte.
    pub code: Option<CodeHit>,
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
    /// Recherche dans le code du diff au premier plan, ouverte par `/`.
    Code { query: String },
    /// Sous-menu des branches, ouvert par `b` : la touche suivante choisit.
    Branches,
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
    /// Sélection de code à la souris dans un diff : bouton enfoncé, déplacé, relâché.
    SelectStart(Position),
    SelectDrag(Position),
    SelectEnd,
    ToggleHierarchy,
    CycleNodes,
    /// Relit le dépôt : branche courante, refs et historique.
    Refresh,
    /// Affiche ou masque les étiquettes des branches distantes.
    /// `w` : replie ou non les lignes trop longues, dans le graphe et dans le code.
    ToggleWrap,
    /// `h` : amène le curseur sur le commit de HEAD, sans l'ouvrir.
    GotoHead,
    /// `b` : ouvre le sous-menu des branches.
    BranchMenu,
    /// `b` `o` : montre ou cache les branches distantes dans le graphe.
    ToggleRemotes,
    /// `b` `O` : ne montre que les branches distantes, ou revient à toutes.
    OnlyRemotes,
    /// `b` `f` : ouvre la liste des branches à montrer dans le graphe.
    PickBranches,
    ToggleHelp,
    /// Ouvre le sous-menu de recherche.
    Search,
    /// Ouvre la recherche de texte dans le code d'un diff.
    Find,
    /// Touches reçues tant qu'une recherche est en cours de saisie.
    PromptKey(char),
    PromptBackspace,
    PromptSubmit,
    PromptCancel,
}

pub struct App {
    pub repo: Repo,
    /// Outil lancé : il décide de ce que montre l'écran et des touches qui valent.
    pub tool: Tool,
    pub plain: bool,
    /// Coloration syntaxique des diffs ; demande un terminal 24 bits.
    pub syntax: bool,
    pub node_style: NodeStyle,
    /// Les lignes trop longues sont repliées, dans le graphe comme dans le code.
    pub wrap: bool,
    /// Nature des branches dont part le graphe.
    pub scope: Scope,
    /// Branches choisies dans la liste (`b` `f`) ; `None` : toutes.
    pub picked: Option<BTreeSet<Branch>>,
    /// Liste des branches, quand elle est ouverte.
    pub picker: Option<Picker>,
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
    /// Vue d'avant l'ouverture de la recherche dans le code : position, défilement
    /// horizontal et recherche précédente, rétablis si la saisie est abandonnée.
    before_find: Option<(usize, usize, Option<Find>)>,
    pub hit: HitMap,
    /// Une sélection à la souris est en cours : le bouton est encore enfoncé.
    pub selecting: bool,
    pub should_quit: bool,
    flash: Flash,
    log: Option<LogStream>,
    goto_end: bool,
    /// Commit à atteindre dès qu'il sera chargé, et lignes déjà examinées.
    goto: Option<(Oid, usize)>,
    /// Écart entre la ligne sélectionnée et le haut de l'écran, à rétablir après un rafraîchissement.
    keep_offset: Option<usize>,
    meta_reader: MetaReader,
    meta_cache: HashMap<usize, Meta>,
    diffs: DiffWorker,
    clipboard: Clipboard,
}

impl App {
    /// L'application de `gitlog`.
    #[cfg(test)]
    pub fn new(repo: Repo, plain: bool) -> Result<App> {
        App::open(repo, plain, Tool::Log)
    }

    pub fn open(repo: Repo, plain: bool, tool: Tool) -> Result<App> {
        // Le flux démarre en premier : git parcourt pendant que les refs sont lues.
        // `gitchange` n'a pas de graphe : il ne lit pas l'historique.
        let log = (tool == Tool::Log).then(|| git::spawn_log(repo.root.clone(), None, Revs::default()));
        let labels = git::load_labels(&repo)?;
        let mut app = App {
            meta_reader: MetaReader::new(&repo.root)?,
            diffs: DiffWorker::new(repo.clone()),
            repo,
            tool,
            plain,
            syntax: true,
            node_style: NodeStyle::Dots,
            wrap: false,
            scope: Scope::All,
            picked: None,
            picker: None,
            store: GraphStore::default(),
            labels,
            log_done: tool != Tool::Log,
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
            selecting: false,
            before_find: None,
            flash: Flash::default(),
            log,
            goto_end: false,
            goto: None,
            keep_offset: None,
            meta_cache: HashMap::new(),
            clipboard: Clipboard::default(),
        };
        if tool == Tool::Changes {
            app.open_changes();
        }
        Ok(app)
    }

    /// `gitchange` : ouvre le détail de ce qui n'est pas commité. Il reste ouvert
    /// tant que l'outil tourne ; c'est le même que celui de la ligne « changes » du graphe.
    fn open_changes(&mut self) {
        // L'identifiant fictif prend la longueur de ceux du dépôt ; à défaut, celle de SHA-1.
        let like = self.labels.keys().next().copied().or_else(|| Oid::from_hex(&"0".repeat(40)));
        let Some(oid) = like.map(|like| Oid::worktree(&like)) else { return };
        let req = self.diffs.request_files(oid, None);
        self.pane = Some(Pane {
            oid,
            meta: Meta { subject: "Uncommitted changes".to_string(), ..Meta::default() },
            files: FilesState::Loading,
            hidden_files: 0,
            file_sel: 0,
            file_top: 0,
            diff: None,
            full: None,
            req,
        });
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

    /// Niveaux que l'outil n'a pas : `gitchange` commence à la liste des
    /// fichiers, son niveau 1, qui est le niveau 2 de `gitlog`.
    pub fn level_shift(&self) -> u8 {
        u8::from(self.tool == Tool::Changes)
    }

    pub fn flash(&self) -> Option<&str> {
        self.flash.text()
    }

    fn set_flash(&mut self, text: impl Into<String>) {
        self.flash.set(text);
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
        loading_log || loading_pane || loading_tree || self.flash.is_shown()
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
        changed | self.flash.expire()
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

    /// `h` : dans le graphe, amène le curseur sur le commit de HEAD et la vue
    /// jusqu'à lui, en chargeant l'historique s'il le faut. Le commit n'est pas ouvert.
    fn goto_head(&mut self) {
        let head = self.labels.iter().find(|(_, labels)| labels.iter().any(|label| label.is_head));
        let Some(oid) = head.map(|(oid, _)| *oid) else {
            return self.set_flash("No HEAD commit");
        };
        match self.store.find(&oid, 0) {
            Some(row) => self.show_row(row),
            // Une recherche ou le choix des branches peuvent l'avoir écarté.
            None if self.log_done => self.set_flash("HEAD is not in the graph"),
            None => {
                self.goto = Some((oid, self.store.len()));
                self.want_all = true;
            }
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
            select: None,
            find: None,
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
            select: None,
            find: None,
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
        let told = self.clipboard.copy_and_tell(&text);
        self.set_flash(told);
    }

    /// Toutes les branches du dépôt : les locales, puis les distantes.
    fn branches(&self) -> Vec<Branch> {
        let labels = self.labels.values().flatten();
        // HEAD détaché porte une étiquette locale sans être une branche.
        let branches = labels.filter(|label| label.kind != RefKind::Tag && !(label.is_head && label.name == "HEAD"));
        let mut branches: Vec<Branch> = branches.map(|label| (label.kind, label.name.clone())).collect();
        branches.sort();
        branches
    }

    /// Une branche est-elle montrée dans le graphe ? Il faut que sa nature soit
    /// visible, et qu'elle soit parmi les branches choisies.
    fn shows_branch(&self, kind: RefKind, name: &str) -> bool {
        let visible = match kind {
            RefKind::Local => self.scope != Scope::Remote,
            RefKind::Remote => self.scope != Scope::Local,
            RefKind::Tag => return true,
        };
        visible && self.picked.as_ref().is_none_or(|picked| picked.iter().any(|(k, n)| *k == kind && n == name))
    }

    /// Une étiquette est-elle à poser sur son commit ? Les tags toujours, les
    /// branches quand le graphe les montre.
    pub fn shows(&self, label: &Label) -> bool {
        if label.is_head && label.name == "HEAD" {
            return self.scope != Scope::Remote;
        }
        self.shows_branch(label.kind, &label.name)
    }

    /// Ce qui restreint le graphe, pour l'en-tête : la nature des branches, et
    /// le nombre de celles qui ont été choisies.
    pub fn branch_note(&self) -> Option<String> {
        let scope = match self.scope {
            Scope::All => None,
            Scope::Local => Some("no remotes".to_string()),
            Scope::Remote => Some("remotes only".to_string()),
        };
        let picked = self.picked.as_ref().map(|picked| match picked.len() {
            1 => "1 branch picked".to_string(),
            count => format!("{count} branches picked"),
        });
        match (scope, picked) {
            (Some(scope), Some(picked)) => Some(format!("{scope}, {picked}")),
            (scope, picked) => scope.or(picked),
        }
    }

    /// Références dont part `git log`, d'après la nature et le choix des branches.
    fn revs(&self) -> Revs {
        let Some(picked) = &self.picked else {
            return match self.scope {
                Scope::All => Revs::default(),
                Scope::Local => Revs { args: vec!["--exclude=refs/remotes/*".to_string(), "--all".to_string()], head: true },
                Scope::Remote => Revs { args: vec!["--remotes".to_string()], head: false },
            };
        };
        let shown = picked.iter().filter(|(kind, name)| self.shows_branch(*kind, name));
        let args = shown.map(|(kind, name)| match kind {
            RefKind::Remote => format!("refs/remotes/{name}"),
            _ => format!("refs/heads/{name}"),
        });
        let head = matches!(&self.repo.head, Head::Branch(name) if self.shows_branch(RefKind::Local, name));
        Revs { args: args.collect(), head }
    }

    /// Les branches montrées ont changé : le graphe est relu, et le commit
    /// sélectionné retrouvé s'il en fait toujours partie.
    fn regraph(&mut self) {
        let current = self.pane.as_ref().map(|pane| pane.oid).or_else(|| self.selected_oid());
        self.close_pane();
        self.restart_log(current, true);
    }

    /// `b` `o` montre ou cache les branches distantes ; `b` `O` ne garde
    /// qu'elles, ou revient à toutes les branches.
    fn set_scope(&mut self, scope: Scope) {
        self.scope = scope;
        self.set_flash(match scope {
            Scope::All => "Local and remote branches",
            Scope::Local => "Remote branches hidden",
            Scope::Remote => "Remote branches only",
        });
        self.regraph();
    }

    /// `b` `f` : ouvre la liste des branches, cochées quand le graphe les montre.
    fn open_picker(&mut self) {
        let items = self.branches();
        if items.is_empty() {
            return self.set_flash("No branch");
        }
        let checked = items.iter().map(|branch| self.picked.as_ref().is_none_or(|picked| picked.contains(branch)));
        let checked: Vec<bool> = checked.collect();
        self.picker = Some(Picker { items, checked, sel: 0, top: 0, height: 0, query: String::new(), typing: false, origin: 0 });
    }

    /// Touches de la liste des branches. Pendant la saisie d'une recherche, les
    /// lettres sont du texte ; sinon ce sont des raccourcis.
    fn apply_in_picker(&mut self, action: Action) {
        let Some(picker) = &mut self.picker else { return };
        if picker.typing {
            match action {
                Action::Quit => self.should_quit = true,
                Action::PromptKey(key) => picker.query.push(key),
                Action::PromptBackspace => {
                    picker.query.pop();
                }
                Action::PromptSubmit => picker.typing = false,
                Action::PromptCancel => {
                    picker.typing = false;
                    picker.query.clear();
                }
                _ => return,
            }
            // Le curseur suit la frappe, à partir de l'endroit où il était.
            picker.sel = picker.seek(picker.origin, 1).unwrap_or(picker.origin);
            return;
        }
        let page = picker.height.max(2) as isize - 1;
        match action {
            Action::Quit => self.should_quit = true,
            Action::Move(delta) | Action::ScrollGraph(delta) | Action::ScrollPane(delta) => picker.step(delta),
            Action::PromptKey('j') => picker.step(1),
            Action::PromptKey('k') => picker.step(-1),
            Action::Page(direction) => picker.step(direction * page),
            Action::Top | Action::PromptKey('g') => picker.sel = 0,
            Action::Bottom | Action::PromptKey('G') => picker.sel = picker.items.len() - 1,
            Action::PromptKey(' ') => picker.checked[picker.sel] = !picker.checked[picker.sel],
            Action::PromptKey('v') => {
                // Tout cocher ; ou tout décocher si tout l'est déjà.
                let all = picker.checked.iter().all(|&checked| checked);
                picker.checked.fill(!all);
            }
            Action::PromptKey('/') => {
                picker.typing = true;
                picker.query.clear();
                picker.origin = picker.sel;
            }
            Action::PromptKey(key @ ('n' | 'N')) => {
                let delta = if key == 'n' { 1 } else { -1 };
                let from = (picker.sel as isize + delta).rem_euclid(picker.items.len() as isize) as usize;
                picker.sel = picker.seek(from, delta).unwrap_or(picker.sel);
            }
            // Échap quitte d'abord la recherche, puis la liste, sans rien changer.
            Action::PromptCancel if !picker.query.is_empty() => picker.query.clear(),
            Action::PromptCancel | Action::PromptKey('q') => self.picker = None,
            Action::PromptSubmit => {
                let chosen = picker.items.iter().zip(&picker.checked).filter(|(_, checked)| **checked);
                let chosen: BTreeSet<Branch> = chosen.map(|(branch, _)| branch.clone()).collect();
                if chosen.is_empty() {
                    return self.set_flash("Pick at least one branch");
                }
                let picked = (chosen.len() < picker.items.len()).then_some(chosen);
                self.picker = None;
                if picked != self.picked {
                    self.picked = picked;
                    self.regraph();
                }
            }
            _ => {}
        }
    }

    /// Diff au premier plan : le fichier entier s'il est ouvert, sinon le diff.
    fn code_view(&mut self) -> Option<&mut DiffView> {
        self.pane.as_mut().and_then(|pane| pane.full.as_mut().or(pane.diff.as_mut()))
    }

    /// Texte sélectionné à la souris dans le diff au premier plan.
    pub fn selected_text(&self) -> Option<String> {
        let pane = self.pane.as_ref()?;
        pane.full.as_ref().or(pane.diff.as_ref())?.selected_text()
    }

    /// Recherche dans le code à montrer en bas de l'écran : son texte, si elle
    /// est en cours de saisie, le rang de l'occurrence courante et leur nombre.
    pub fn code_find(&self) -> Option<(&str, bool, Option<usize>, usize)> {
        let pane = self.pane.as_ref();
        let find = pane.and_then(|pane| pane.full.as_ref().or(pane.diff.as_ref())).and_then(|view| view.find.as_ref());
        let state = (find.and_then(|find| find.current), find.map_or(0, |find| find.hits.len()));
        match &self.prompt {
            Some(Prompt::Code { query }) => Some((query, true, state.0, state.1)),
            Some(_) => None,
            None => find.map(|find| (find.query.as_str(), false, state.0, state.1)),
        }
    }

    /// `/` : ouvre la saisie d'une recherche dans le code du diff au premier plan.
    fn open_find(&mut self) {
        if let Some(view) = self.code_view() {
            self.before_find = Some((view.top, view.x, view.find.take()));
            self.prompt = Some(Prompt::Code { query: String::new() });
        }
    }

    /// La saisie change : la recherche repart d'où la vue se trouvait à
    /// l'ouverture, et va à la première occurrence qui suit.
    fn find_live(&mut self, query: &str) {
        let Some((top, x)) = self.before_find.as_ref().map(|&(top, x, _)| (top, x)) else { return };
        let (height, code) = (self.pane_height, self.hit.code.clone());
        if let Some(view) = self.code_view() {
            (view.top, view.x) = (top, x);
            view.set_find(query);
            view.reveal(height, code.as_ref());
        }
    }

    /// Fin de la saisie. `keep` garde la recherche tapée, si elle a trouvé
    /// quelque chose ; sinon la vue revient où elle était, avec `restore` pour
    /// retrouver aussi la recherche d'avant.
    fn close_find(&mut self, keep: bool, restore: bool) -> bool {
        let Some((top, x, find)) = self.before_find.take() else { return false };
        let Some(view) = self.code_view() else { return false };
        if keep && view.find.as_ref().is_some_and(|find| !find.hits.is_empty()) {
            return true;
        }
        (view.top, view.x, view.find) = (top, x, find.filter(|_| restore));
        false
    }

    /// `n` / `N` pendant une recherche dans le code : occurrence suivante ou
    /// précédente, en boucle. Faux quand aucune recherche n'est en place.
    fn find_step(&mut self, delta: isize) -> bool {
        let (height, code) = (self.pane_height, self.hit.code.clone());
        let Some(view) = self.code_view() else { return false };
        let top = view.top;
        let Some(find) = &mut view.find else { return false };
        let count = find.hits.len();
        if count == 0 {
            self.set_flash("No match");
            return true;
        }
        find.current = Some(match find.current {
            Some(current) => (current as isize + delta).rem_euclid(count as isize) as usize,
            // Pas encore d'occurrence courante : la plus proche du haut de la vue.
            None if delta > 0 => find.hits.iter().position(|hit| hit.row >= top).unwrap_or(0),
            None => find.hits.iter().rposition(|hit| hit.row < top).unwrap_or(count - 1),
        });
        view.reveal(height, code.as_ref());
        true
    }

    /// Retire la recherche dans le code ; faux s'il n'y en avait pas.
    fn clear_find(&mut self) -> bool {
        self.code_view().and_then(|view| view.find.take()).is_some()
    }

    /// Bouton enfoncé sur le code d'un diff : une sélection commence du côté
    /// visé, la précédente s'efface.
    fn select_start(&mut self, pos: Position) {
        let Some(code) = self.hit.code.clone() else { return };
        let side = code.side(pos.x);
        let point = code.locate(pos, side);
        if let Some(view) = self.code_view() {
            view.select = Some(Selection { side, anchor: point, head: point });
            self.selecting = true;
        }
    }

    /// Le pointeur se déplace, bouton enfoncé : la sélection le suit. Passé le
    /// haut ou le bas du diff, la vue défile d'une ligne.
    fn select_drag(&mut self, pos: Position) {
        let Some(code) = self.hit.code.clone() else { return };
        let Some(view) = self.code_view() else { return };
        let Some(side) = view.select.map(|select| select.side) else { return };
        let last = view.len().saturating_sub(1);
        let (mut row, col) = code.locate(pos, side);
        // Après un défilement, la ligne visée est celle qui vient sous le pointeur.
        if pos.y < code.area.y {
            view.top = view.top.saturating_sub(1);
            row = view.top;
        } else if pos.y >= code.lower_edge {
            view.top = (view.top + 1).min(last);
            // La dernière ligne dessinée, décalée d'autant que la vue depuis ce rendu.
            let (first, shown) = (code.lines.first().map_or(view.top, |line| line.0), code.lines.last().map_or(view.top, |line| line.0));
            row = shown + view.top.saturating_sub(first);
        }
        if let Some(select) = &mut view.select {
            select.head = (row.min(last), col);
        }
    }

    /// Bouton relâché : le texte sélectionné part dans le presse-papier. Après un
    /// simple clic, il ne reste aucune sélection.
    fn select_end(&mut self) {
        self.selecting = false;
        match self.selected_text() {
            Some(text) => self.copy(text),
            None => {
                if let Some(view) = self.code_view() {
                    view.select = None;
                }
            }
        }
    }

    /// Saisie d'une recherche : le sous-menu choisit le type, puis le champ reçoit le texte.
    fn apply_in_prompt(&mut self, action: Action) {
        let Some(prompt) = self.prompt.take() else { return };
        // Sous-menu des branches : la touche choisit, toute autre le referme.
        if matches!(prompt, Prompt::Branches) {
            let next = match action {
                Action::Quit => Action::Quit,
                Action::PromptKey('h') => Action::ToggleHierarchy,
                Action::PromptKey('f') => Action::PickBranches,
                Action::PromptKey('o') => Action::ToggleRemotes,
                Action::PromptKey('O') => Action::OnlyRemotes,
                _ => return,
            };
            return self.apply(next);
        }
        // Les recherches rapides se relancent d'elles-mêmes, un instant après la dernière frappe.
        let live = |kind: SearchKind| kind.is_live().then(|| Instant::now() + LIVE_DELAY);
        self.prompt = match (prompt, action) {
            (_, Action::Quit) => {
                self.should_quit = true;
                None
            }
            (Prompt::Code { .. }, Action::PromptCancel) => {
                // Abandon : la vue et la recherche d'avant.
                self.close_find(false, true);
                None
            }
            (Prompt::Code { mut query }, Action::PromptKey(key)) => {
                query.push(key);
                self.find_live(&query);
                Some(Prompt::Code { query })
            }
            (Prompt::Code { mut query }, Action::PromptBackspace) => {
                query.pop();
                self.find_live(&query);
                Some(Prompt::Code { query })
            }
            (Prompt::Code { query }, Action::PromptSubmit) => {
                if !self.close_find(true, false) && !query.is_empty() {
                    self.set_flash(format!("No match: {query}"));
                }
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
        self.log = Some(git::spawn_log(self.repo.root.clone(), self.filter.clone(), self.revs()));
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
        // Une branche choisie a pu disparaître : git refuserait son nom. S'il n'en
        // reste aucune, le graphe revient à toutes les branches.
        if let Some(picked) = self.picked.take() {
            let branches: BTreeSet<Branch> = self.branches().into_iter().collect();
            let left: BTreeSet<Branch> = picked.intersection(&branches).cloned().collect();
            self.picked = Some(left).filter(|left| !left.is_empty());
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
        // `gitchange` n'a pas d'historique à relire.
        if self.tool == Tool::Log {
            let current = self.pane.as_ref().map(|pane| pane.oid).or_else(|| self.selected_oid());
            self.keep_offset = current.filter(|_| self.top > 0).map(|_| self.selected.saturating_sub(self.top));
            self.restart_log(current, true);
        }
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
            // Pas de commit voisin dans `gitchange` : la liste des fichiers est seule.
            2 if self.tool == Tool::Changes => {}
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
        if self.picker.is_some() {
            return self.apply_in_picker(action);
        }
        if self.prompt.is_some() {
            return self.apply_in_prompt(action);
        }
        if self.copy_from_header(&action) {
            return;
        }
        // Sans graphe, ce qui s'y rapporte n'a pas d'objet : recherche de commits,
        // branches, style des commits.
        let graph_only = matches!(
            action,
            Action::Search
                | Action::BranchMenu
                | Action::PickBranches
                | Action::ToggleRemotes
                | Action::OnlyRemotes
                | Action::ToggleHierarchy
                | Action::GotoHead
                | Action::CycleNodes
                | Action::ClickCommit(_)
        );
        if graph_only && self.tool == Tool::Changes {
            return;
        }
        // Valables dans toutes les vues, hiérarchie des branches comprise.
        match action {
            Action::Refresh => return self.refresh(),
            Action::ToggleWrap => {
                self.wrap = !self.wrap;
                return self.set_flash(if self.wrap { "Long lines wrapped" } else { "Long lines cut" });
            }
            Action::BranchMenu => {
                self.prompt = Some(Prompt::Branches);
                return;
            }
            Action::PickBranches => return self.open_picker(),
            // Cacher les branches distantes quand elles sont seules ramène aux locales.
            Action::ToggleRemotes => {
                return self.set_scope(if self.scope == Scope::Local { Scope::All } else { Scope::Local });
            }
            Action::OnlyRemotes => {
                return self.set_scope(if self.scope == Scope::Remote { Scope::All } else { Scope::Remote });
            }
            // Depuis la hiérarchie, `b` `h` la referme.
            Action::ToggleHierarchy if self.hierarchy.is_some() => {
                self.hierarchy = None;
                return;
            }
            _ => {}
        }
        if self.hierarchy.is_some() {
            return self.apply_in_hierarchy(action);
        }
        match action {
            Action::Quit => self.should_quit = true,
            // Une recherche dans le code est un niveau de plus : on en sort d'abord.
            Action::Back { .. } if self.clear_find() => {}
            Action::Back { quit } => match self.depth() {
                4 => self.close_full(),
                3 => self.close_diff(),
                // La liste des fichiers est le premier niveau de `gitchange` : `q` y quitte.
                2 if self.tool == Tool::Changes => self.should_quit = quit,
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
            Action::GoLevel(level) => match (level + self.level_shift(), self.depth()) {
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
            Action::Step(delta) => {
                // Pendant une recherche dans le code, `n` et `N` suivent ses occurrences.
                if !self.find_step(delta) {
                    self.step(delta);
                }
            }
            Action::Find => self.open_find(),
            Action::GotoHead => {
                if self.pane.is_none() {
                    self.goto_head();
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
                if let Some(diff) = self.pane.as_mut().and_then(|p| p.full.as_mut().or(p.diff.as_mut())) {
                    diff.x = diff.x.saturating_add_signed(delta);
                }
            }
            Action::SelectStart(pos) => self.select_start(pos),
            Action::SelectDrag(pos) => self.select_drag(pos),
            Action::SelectEnd => self.select_end(),
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
            Action::Refresh
            | Action::ToggleWrap
            | Action::BranchMenu
            | Action::PickBranches
            | Action::ToggleRemotes
            | Action::OnlyRemotes => {}
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
