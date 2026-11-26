//! Accès au dépôt par le processus `git` de l'hôte, toujours en flux :
//! `git log` pour l'historique, `git cat-file --batch` pour les métadonnées
//! des lignes visibles, `git show` pour les fichiers et les diffs.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::thread;

use anyhow::{Context, Result, bail};

use crate::diff::{DiffLine, DiffParser, FileEntry, LineKind, parse_files};
use crate::syntax::Colorizer;
use crate::graph::{Batch, COMMIT, Cell, Layout, Oid};
use crate::hierarchy::{self, Branch, BranchRow, Remote};

/// Commits par lot envoyé à l'interface.
const BATCH_ROWS: usize = 256;
/// Lots en attente avant que le thread de lecture, puis git, ne se bloquent.
const BATCH_QUEUE: usize = 8;
/// Lignes de diff par tranche envoyée à l'interface.
const DIFF_CHUNK: usize = 1000;
/// Plafond de lignes conservées pour un diff.
pub const MAX_DIFF_LINES: usize = 50_000;

fn git(root: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(root)
        .args(["--no-pager", "-c", "core.quotePath=false"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null());
    cmd
}

/// Lance une commande git courte et renvoie sa sortie standard.
fn output(root: &Path, args: &[&str]) -> Result<String> {
    let out = git(root)
        .args(args)
        .output()
        .context("cannot run git: is it installed and on the PATH?")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Head {
    Branch(String),
    Detached(Oid),
    Unborn,
}

#[derive(Clone, Debug)]
pub struct Repo {
    pub root: PathBuf,
    /// Nom du dossier racine.
    pub name: String,
    /// Nom du dépôt distant, tiré de l'URL de `origin`.
    pub remote_name: Option<String>,
    /// URL du remote `origin`.
    pub remote_url: Option<String>,
    pub head: Head,
}

/// Nom du dépôt dans une URL de remote : `https://hôte/qui/nom.git`, `git@hôte:qui/nom.git`.
fn repo_name(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches(['/', '\\']);
    let url = url.strip_suffix(".git").unwrap_or(url);
    let name = url.rsplit(['/', '\\', ':']).next()?;
    (!name.is_empty()).then(|| name.to_string())
}

impl Repo {
    pub fn open(dir: &Path) -> Result<Repo> {
        let root = match output(dir, &["rev-parse", "--show-toplevel"]) {
            Ok(top) => PathBuf::from(top.trim()),
            Err(err) => {
                // Dépôt nu : pas de dossier de travail, mais un dossier git valide.
                if output(dir, &["rev-parse", "--git-dir"]).is_err() {
                    bail!("{} is not inside a git repository ({err})", dir.display());
                }
                dir.to_path_buf()
            }
        };
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string());
        let head_oid = output(&root, &["rev-parse", "--verify", "-q", "HEAD"])
            .ok()
            .and_then(|s| Oid::from_hex(s.trim()));
        let head = match (output(&root, &["symbolic-ref", "-q", "--short", "HEAD"]), head_oid) {
            (_, None) => Head::Unborn,
            (Ok(branch), Some(_)) => Head::Branch(branch.trim().to_string()),
            (Err(_), Some(oid)) => Head::Detached(oid),
        };
        let remote_url = output(&root, &["remote", "get-url", "origin"])
            .ok()
            .map(|url| url.trim().to_string())
            .filter(|url| !url.is_empty());
        let remote_name = remote_url.as_deref().and_then(repo_name);
        Ok(Repo { root, name, remote_name, remote_url, head })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RefKind {
    Local,
    Remote,
    Tag,
}

#[derive(Clone, Debug)]
pub struct Label {
    pub name: String,
    pub kind: RefKind,
    /// Branche courante, ou HEAD détaché.
    pub is_head: bool,
}

/// Branches et tags par commit, lus une seule fois au démarrage.
pub fn load_labels(repo: &Repo) -> Result<HashMap<Oid, Vec<Label>>> {
    let out = output(
        &repo.root,
        &["for-each-ref", "--format=%(objectname)%09%(*objectname)%09%(refname)"],
    )?;
    let mut labels: HashMap<Oid, Vec<Label>> = HashMap::new();
    for line in out.lines() {
        let mut parts = line.split('\t');
        let (Some(object), Some(peeled), Some(refname)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        // Un tag annoté pointe sur un objet tag : `*objectname` est le commit visé.
        let Some(oid) = Oid::from_hex(if peeled.is_empty() { object } else { peeled }) else {
            continue;
        };
        let (kind, name) = if let Some(n) = refname.strip_prefix("refs/heads/") {
            (RefKind::Local, n)
        } else if let Some(n) = refname.strip_prefix("refs/remotes/") {
            if n.ends_with("/HEAD") {
                continue;
            }
            (RefKind::Remote, n)
        } else if let Some(n) = refname.strip_prefix("refs/tags/") {
            (RefKind::Tag, n)
        } else {
            continue;
        };
        let is_head = kind == RefKind::Local && repo.head == Head::Branch(name.to_string());
        labels.entry(oid).or_default().push(Label { name: name.to_string(), kind, is_head });
    }
    if let Head::Detached(oid) = repo.head {
        labels.entry(oid).or_default().push(Label {
            name: "HEAD".to_string(),
            kind: RefKind::Local,
            is_head: true,
        });
    }
    for list in labels.values_mut() {
        list.sort_by(|a, b| (!a.is_head, a.kind, &a.name).cmp(&(!b.is_head, b.kind, &b.name)));
    }
    Ok(labels)
}

pub enum LogMsg {
    Batch(Batch),
    Error(String),
}

/// Ce qu'une recherche examine dans chaque commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchKind {
    /// Début du hash.
    Hash,
    Message,
    /// Nom d'un fichier touché par le commit.
    File,
    /// Ligne ajoutée ou supprimée par le commit.
    Hunk,
    HunkAdded,
    HunkRemoved,
}

impl SearchKind {
    pub fn label(self) -> &'static str {
        match self {
            SearchKind::Hash => "hash",
            SearchKind::Message => "message",
            SearchKind::File => "file",
            SearchKind::Hunk => "hunk",
            SearchKind::HunkAdded => "+hunk",
            SearchKind::HunkRemoved => "-hunk",
        }
    }
}

impl SearchKind {
    /// Assez rapide pour être relancée pendant la frappe. Une recherche dans les
    /// hunks recalcule tous les diffs de l'historique : elle attend Entrée.
    pub fn is_live(self) -> bool {
        matches!(self, SearchKind::Hash | SearchKind::Message | SearchKind::File)
    }
}

/// Références dont part le graphe : toutes, ou celles que l'on a choisi de voir.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Revs {
    /// Arguments de `git log` qui les désignent ; aucun quand il n'y a rien à montrer.
    pub args: Vec<String>,
    /// HEAD en fait partie : ce qui n'est pas encore commité peut s'y poser.
    pub head: bool,
}

impl Default for Revs {
    fn default() -> Revs {
        Revs { args: vec!["--all".to_string()], head: true }
    }
}

/// Recherche active : seuls les commits qui y répondent sont listés, sans casse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filter {
    pub kind: SearchKind,
    pub query: String,
}

impl Filter {
    /// Recherche du texte saisi, ou aucune s'il est vide.
    pub fn new(kind: SearchKind, query: &str) -> Option<Filter> {
        let query = query.trim();
        (!query.is_empty()).then(|| Filter { kind, query: query.to_string() })
    }

    /// Vrai si la recherche porte sur le contenu d'un commit : elle restreint alors
    /// aussi le détail du commit, fichiers et hunks.
    pub fn narrows_detail(&self) -> bool {
        self.kind == SearchKind::File || self.hunk_signs().is_some()
    }

    /// Signes des lignes de diff où une recherche dans les hunks lit le texte.
    fn hunk_signs(&self) -> Option<&'static [u8]> {
        match self.kind {
            SearchKind::Hunk => Some(b"+-"),
            SearchKind::HunkAdded => Some(b"+"),
            SearchKind::HunkRemoved => Some(b"-"),
            SearchKind::Hash | SearchKind::Message | SearchKind::File => None,
        }
    }
}

/// Flux de `git log` en cours. Le lâcher arrête git, même s'il cherche encore.
pub struct LogStream {
    pub rx: Receiver<LogMsg>,
    child: Arc<Mutex<Option<Child>>>,
}

impl Drop for LogStream {
    fn drop(&mut self) {
        if let Ok(mut slot) = self.child.lock()
            && let Some(child) = slot.as_mut()
        {
            let _ = child.kill();
        }
    }
}

/// Lit `git log` dans un thread et envoie les lignes du graphe par lots sur un
/// canal borné. Quand l'interface ne consomme plus, le thread se bloque sur le
/// canal, puis git sur son tube : rien n'est lu d'avance au-delà de la file.
pub fn spawn_log(root: PathBuf, filter: Option<Filter>, revs: Revs) -> LogStream {
    let (tx, rx) = mpsc::sync_channel(BATCH_QUEUE);
    let child = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&child);
    thread::spawn(move || {
        if let Err(err) = read_log(&root, filter.as_ref(), &revs, &tx, &slot) {
            let _ = tx.send(LogMsg::Error(format!("{err:#}")));
        }
    });
    LogStream { rx, child }
}

/// Commit de HEAD quand le dossier de travail ou l'index s'en écartent : fichier
/// modifié, indexé ou non suivi. `None` quand `git status` ne signale rien, et
/// dans un dépôt sans commit ou sans dossier de travail.
fn dirty_head(root: &Path) -> Option<Oid> {
    let status = output(root, &["status", "--porcelain=v2", "--branch", "-z"]).ok()?;
    let mut head = None;
    let mut dirty = false;
    // Les en-têtes finissent par un saut de ligne, les entrées par un octet nul.
    for entry in status.split(['\0', '\n']).filter(|entry| !entry.is_empty()) {
        match entry.strip_prefix("# ") {
            Some(header) => {
                if let Some(hex) = header.strip_prefix("branch.oid ") {
                    head = Oid::from_hex(hex);
                }
            }
            None => dirty = true,
        }
    }
    head.filter(|_| dirty)
}

/// Neutralise les caractères spéciaux d'une expression régulière de base.
fn regex_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | '.' | '[' | '*' | '^' | '$') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Options de `git log` qui restreignent l'historique aux commits recherchés.
/// Le hash et le signe d'un hunk sont vérifiés à la lecture, ligne par ligne.
fn log_command(root: &Path, filter: Option<&Filter>, revs: &Revs) -> Command {
    let mut cmd = git(root);
    cmd.arg("log").args(&revs.args).arg("--date-order");
    let Some(Filter { kind, query }) = filter else {
        cmd.arg("--format=%H %P");
        return cmd;
    };
    match kind {
        SearchKind::Hash => cmd.arg("--format=%H"),
        SearchKind::Message => cmd.args(["--format=%H", "-i", "--fixed-strings"]).arg(format!("--grep={query}")),
        SearchKind::Hunk => cmd.args(["--format=%H", "-i"]).arg(format!("-G{}", regex_escape(query))),
        // Le patch suit chaque hash, annoncé par un octet nul, pour y lire le signe des lignes.
        SearchKind::HunkAdded | SearchKind::HunkRemoved => cmd
            .args(["--format=%x00%H", "-i", "-p", "-U0", "--no-color", "--no-ext-diff"])
            .arg(format!("-G{}", regex_escape(query))),
        SearchKind::File => cmd
            .args(["--format=%H", "--full-history", "--no-merges", "--"])
            .arg(format!(":(icase)*{query}*")),
    };
    cmd
}

/// Lignes du graphe en cours d'envoi à l'interface.
struct Sink<'a> {
    tx: &'a SyncSender<LogMsg>,
    layout: Layout,
    batch: Batch,
    /// Premier lot réduit pour afficher l'écran au plus tôt.
    limit: usize,
}

impl Sink<'_> {
    /// Ajoute un commit ; sans `parents`, c'est un résultat de recherche, hors graphe.
    /// Renvoie faux quand l'interface n'écoute plus.
    fn push(&mut self, oid: &Oid, parents: Option<&[Oid]>) -> bool {
        match parents {
            Some(parents) => {
                let (col, cells) = self.layout.push(oid, parents);
                self.batch.push(oid, col, cells);
            }
            None => self.batch.push(oid, 0, &[Cell { bits: COMMIT, color: 0 }]),
        }
        if self.batch.rows.len() < self.limit {
            return true;
        }
        self.limit = BATCH_ROWS;
        self.tx.send(LogMsg::Batch(std::mem::take(&mut self.batch))).is_ok()
    }
}

fn read_log(
    root: &Path,
    filter: Option<&Filter>,
    revs: &Revs,
    tx: &SyncSender<LogMsg>,
    slot: &Mutex<Option<Child>>,
) -> Result<()> {
    // Sans référence, `git log` partirait de HEAD : il n'y a rien à lire.
    if revs.args.is_empty() {
        return Ok(());
    }
    let mut child = log_command(root, filter, revs)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("cannot run git log")?;
    let mut stdout = BufReader::new(child.stdout.take().context("git log has no output")?);
    let mut stderr = child.stderr.take();
    // L'interface garde la main sur le processus pour pouvoir l'arrêter.
    *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(child);

    let needle = filter.map(|f| f.query.to_lowercase()).unwrap_or_default();
    let kind = filter.map(|f| f.kind);
    let sign = match kind {
        Some(SearchKind::HunkAdded) => Some(b'+'),
        Some(SearchKind::HunkRemoved) => Some(b'-'),
        _ => None,
    };
    let mut sink = Sink { tx, layout: Layout::default(), batch: Batch::default(), limit: 64 };
    // Le graphe complet commence par ce qui n'est pas encore commité, posé sur HEAD.
    if filter.is_none()
        && revs.head
        && let Some(head) = dirty_head(root)
    {
        sink.push(&Oid::worktree(&head), Some(&[head]));
    }
    let mut parents: Vec<Oid> = Vec::new();
    // Recherche par signe : commit dont le patch est en cours de lecture.
    let mut candidate: Option<Oid> = None;
    let mut in_hunk = false;
    let mut raw: Vec<u8> = Vec::new();
    let mut listening = true;
    while listening {
        raw.clear();
        if stdout.read_until(b'\n', &mut raw)? == 0 {
            break;
        }
        while matches!(raw.last(), Some(b'\n' | b'\r')) {
            raw.pop();
        }
        if let Some(sign) = sign {
            match raw.first() {
                Some(0) => {
                    candidate = std::str::from_utf8(&raw[1..]).ok().and_then(Oid::from_hex);
                    in_hunk = false;
                }
                _ if raw.starts_with(b"diff --git ") => in_hunk = false,
                Some(b'@') => in_hunk = true,
                Some(first) if in_hunk && *first == sign && candidate.is_some() => {
                    if String::from_utf8_lossy(&raw[1..]).to_lowercase().contains(&needle)
                        && let Some(oid) = candidate.take()
                    {
                        listening = sink.push(&oid, None);
                    }
                }
                _ => {}
            }
            continue;
        }
        let line = String::from_utf8_lossy(&raw);
        let mut hashes = line.split(' ').filter(|s| !s.is_empty());
        let Some(hex) = hashes.next() else { continue };
        let Some(oid) = Oid::from_hex(hex) else { continue };
        listening = match kind {
            None => {
                parents.clear();
                parents.extend(hashes.filter_map(Oid::from_hex));
                sink.push(&oid, Some(&parents))
            }
            Some(SearchKind::Hash) if !hex.starts_with(&needle) => true,
            Some(_) => sink.push(&oid, None),
        };
    }

    let child = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
    let Some(mut child) = child else { return Ok(()) };
    if !listening {
        // L'interface est partie : on arrête git.
        let _ = child.kill();
        let _ = child.wait();
        return Ok(());
    }
    if !sink.batch.rows.is_empty() {
        let _ = tx.send(LogMsg::Batch(sink.batch));
    }
    let mut message = String::new();
    if let Some(err) = stderr.as_mut() {
        let _ = err.read_to_string(&mut message);
    }
    if !child.wait()?.success() && !message.trim().is_empty() {
        bail!("git log: {}", message.trim());
    }
    Ok(())
}

#[derive(Clone, Debug, Default)]
pub struct Meta {
    pub author: String,
    /// Date du commit, en secondes Unix.
    pub time: i64,
    pub subject: String,
}

/// Processus `git cat-file --batch` gardé ouvert : il rend le texte d'un
/// commit à la demande, pour les seules lignes à l'écran.
pub struct MetaReader {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl MetaReader {
    pub fn new(root: &Path) -> Result<MetaReader> {
        let mut child = git(root)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("cannot run git cat-file")?;
        let stdin = child.stdin.take().context("git cat-file has no input")?;
        let stdout = BufReader::new(child.stdout.take().context("git cat-file has no output")?);
        Ok(MetaReader { child, stdin, stdout })
    }

    pub fn get(&mut self, oid: &Oid) -> Meta {
        self.read(oid).unwrap_or_default()
    }

    fn read(&mut self, oid: &Oid) -> Result<Meta> {
        writeln!(self.stdin, "{}", oid.to_hex())?;
        self.stdin.flush()?;
        let mut header = String::new();
        self.stdout.read_line(&mut header)?;
        let mut parts = header.split_whitespace();
        let (_, kind, size) = (parts.next(), parts.next(), parts.next());
        let Some(size) = size.and_then(|s| s.parse::<usize>().ok()) else {
            bail!("object not found");
        };
        // Le contenu est suivi d'un saut de ligne.
        let mut body = vec![0u8; size + 1];
        self.stdout.read_exact(&mut body)?;
        body.truncate(size);
        if kind != Some("commit") {
            bail!("not a commit");
        }
        Ok(parse_commit(&String::from_utf8_lossy(&body)))
    }
}

impl Drop for MetaReader {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Analyse le contenu brut d'un objet commit.
fn parse_commit(raw: &str) -> Meta {
    let mut meta = Meta::default();
    let (headers, message) = raw.split_once("\n\n").unwrap_or((raw, ""));
    for line in headers.lines() {
        if let Some(rest) = line.strip_prefix("author ") {
            meta.author = rest.split(" <").next().unwrap_or("").to_string();
        } else if let Some(rest) = line.strip_prefix("committer ") {
            // « Nom <mail> horodatage fuseau »
            let mut tail = rest.rsplit(' ');
            tail.next();
            meta.time = tail.next().and_then(|t| t.parse().ok()).unwrap_or(0);
        }
    }
    meta.subject = message.lines().next().unwrap_or("").chars().take(400).collect();
    meta
}

/// Demande du diff d'un fichier.
pub struct FileReq {
    pub oid: Oid,
    /// Chemin du fichier dans ce commit, puis l'ancien s'il a été renommé.
    pub paths: Vec<String>,
    /// Le fichier entier, contexte compris, et non les seuls hunks.
    pub full: bool,
    /// Avec la coloration syntaxique.
    pub syntax: bool,
    /// Recherche active : seuls les hunks qui y répondent sont gardés.
    pub filter: Option<Filter>,
    /// Fichier non suivi du dossier de travail : il n'a pas d'ancienne version.
    pub untracked: bool,
}

pub enum DiffReq {
    /// `filter` : recherche active, qui ne garde que les fichiers concernés.
    Files { id: u64, oid: Oid, filter: Option<Filter> },
    File { id: u64, req: FileReq },
    Hierarchy { id: u64 },
}

pub enum DiffMsg {
    /// Fichiers du commit et nombre de ceux qu'une recherche a écartés.
    Files { id: u64, result: Result<(Vec<FileEntry>, usize), String> },
    Lines { id: u64, lines: Vec<DiffLine>, done: bool },
    /// Lignes de l'arbre des branches et nombre de branches laissées de côté.
    Hierarchy { id: u64, result: Result<(Vec<BranchRow>, usize), String> },
}

/// Thread qui calcule, à la demande, la liste des fichiers d'un commit, le diff
/// d'un fichier et la hiérarchie des branches. Une demande plus récente annule
/// le diff en cours.
pub struct DiffWorker {
    tx: Sender<DiffReq>,
    pub rx: Receiver<DiffMsg>,
    latest: Arc<AtomicU64>,
}

impl DiffWorker {
    pub fn new(repo: Repo) -> DiffWorker {
        let root = repo.root.clone();
        let (tx, req_rx) = mpsc::channel::<DiffReq>();
        let (msg_tx, rx) = mpsc::channel::<DiffMsg>();
        let latest = Arc::new(AtomicU64::new(0));
        let latest_in_thread = Arc::clone(&latest);
        thread::spawn(move || {
            while let Ok(mut req) = req_rx.recv() {
                // Seule la dernière demande compte.
                while let Ok(newer) = req_rx.try_recv() {
                    req = newer;
                }
                let sent = match req {
                    DiffReq::Files { id, oid, filter } => {
                        let result = list_files(&root, &oid, filter.as_ref()).map_err(|e| format!("{e:#}"));
                        msg_tx.send(DiffMsg::Files { id, result }).is_ok()
                    }
                    DiffReq::File { id, req } => stream_diff(&root, id, &req, &latest_in_thread, &msg_tx),
                    DiffReq::Hierarchy { id } => {
                        // Le dépôt est relu : la branche courante a pu changer hors de gitlog.
                        let result = Repo::open(&repo.root)
                            .and_then(|repo| load_hierarchy(&repo))
                            .map_err(|e| format!("{e:#}"));
                        msg_tx.send(DiffMsg::Hierarchy { id, result }).is_ok()
                    }
                };
                if !sent {
                    break;
                }
            }
        });
        DiffWorker { tx, rx, latest }
    }

    fn next_id(&self) -> u64 {
        self.latest.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn request_files(&self, oid: Oid, filter: Option<Filter>) -> u64 {
        let id = self.next_id();
        let _ = self.tx.send(DiffReq::Files { id, oid, filter });
        id
    }

    pub fn request_file(&self, req: FileReq) -> u64 {
        let id = self.next_id();
        let _ = self.tx.send(DiffReq::File { id, req });
        id
    }

    pub fn request_hierarchy(&self) -> u64 {
        let id = self.next_id();
        let _ = self.tx.send(DiffReq::Hierarchy { id });
        id
    }

    /// Annule le diff en cours de lecture.
    pub fn cancel(&self) {
        self.next_id();
    }
}

/// Options communes : un merge est comparé à son premier parent.
const SHOW_ARGS: [&str; 5] = ["show", "--format=", "--diff-merges=first-parent", "-M", "--no-color"];

/// Fichiers d'un commit. Une recherche sur les fichiers ou les hunks ne garde que
/// ceux qui y répondent ; le second nombre compte les fichiers écartés.
fn list_files(root: &Path, oid: &Oid, filter: Option<&Filter>) -> Result<(Vec<FileEntry>, usize)> {
    if oid.is_worktree() {
        return Ok((list_changes(root)?, 0));
    }
    let hex = oid.to_hex();
    let mut args = SHOW_ARGS.to_vec();
    args.extend(["--raw", "--numstat", "--no-abbrev", hex.as_str()]);
    let mut files = parse_files(&output(root, &args)?);
    let Some(filter) = filter.filter(|f| f.narrows_detail()) else {
        return Ok((files, 0));
    };
    let needle = filter.query.to_lowercase();
    let keep: Vec<bool> = match filter.hunk_signs() {
        None => {
            let named = |path: &String| path.to_lowercase().contains(&needle);
            files.iter().map(|file| named(&file.path) || file.old_path.as_ref().is_some_and(named)).collect()
        }
        Some(signs) => {
            let matching = files_with_hunk(root, &hex, &filter.query, &needle, signs)?;
            files.iter().map(|file| matching.contains(&file.path)).collect()
        }
    };
    // Aucun fichier retenu (un merge, comparé à son seul premier parent) : la liste reste entière.
    if !keep.contains(&true) {
        return Ok((files, 0));
    }
    let total = files.len();
    let mut keep = keep.into_iter();
    files.retain(|_| keep.next().unwrap_or(false));
    let hidden = total - files.len();
    Ok((files, hidden))
}

/// Fichiers non suivis listés au plus : au-delà, un dossier oublié dans
/// `.gitignore` noierait le reste.
const MAX_UNTRACKED: usize = 300;
/// Taille au-delà de laquelle les lignes d'un fichier non suivi ne sont pas comptées.
const MAX_COUNTED_BYTES: u64 = 2_000_000;

/// Ce à quoi se compare ce qui n'est pas commité : HEAD, ou l'arbre vide tant
/// que le dépôt n'a aucun commit.
fn changes_base(root: &Path) -> String {
    if output(root, &["rev-parse", "--verify", "-q", "HEAD"]).is_ok() {
        return "HEAD".to_string();
    }
    let empty = output(root, &["hash-object", "-t", "tree", "/dev/null"]);
    empty.map_or_else(|_| "HEAD".to_string(), |hash| hash.trim().to_string())
}

/// Ce qui n'est pas encore commité : les fichiers suivis qui diffèrent de HEAD,
/// indexés ou non, puis les fichiers non suivis, marqués `?`.
fn list_changes(root: &Path) -> Result<Vec<FileEntry>> {
    let base = changes_base(root);
    let tracked = output(root, &["diff", "-M", "--raw", "--numstat", "--no-abbrev", base.as_str()])?;
    let mut files = parse_files(&tracked);
    let untracked = output(root, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    for path in untracked.split('\0').filter(|path| !path.is_empty()).take(MAX_UNTRACKED) {
        // Un fichier texte compte pour autant de lignes ajoutées ; un binaire n'en a pas.
        let lines = std::fs::metadata(root.join(path))
            .ok()
            .filter(|meta| meta.len() <= MAX_COUNTED_BYTES)
            .and_then(|_| std::fs::read(root.join(path)).ok())
            .filter(|bytes| !bytes.contains(&0))
            .map(|bytes| bytes.split_inclusive(|&byte| byte == b'\n').count() as u32);
        files.push(FileEntry {
            status: '?',
            path: path.to_string(),
            old_path: None,
            added: lines,
            deleted: lines.map(|_| 0),
        });
    }
    Ok(files)
}

/// Chemins des fichiers d'un commit dont une ligne ajoutée ou retirée, selon
/// `signs`, contient `needle`. git désigne d'abord les fichiers candidats, puis
/// leur patch, dans le même ordre, donne le signe de chaque ligne.
fn files_with_hunk(root: &Path, hex: &str, query: &str, needle: &str, signs: &[u8]) -> Result<Vec<String>> {
    let pickaxe = format!("-G{}", regex_escape(query));
    let mut common = SHOW_ARGS.to_vec();
    common.extend(["--no-ext-diff", "-i", pickaxe.as_str()]);

    let mut args = common.clone();
    args.extend(["--name-only", "-z", hex]);
    let names = output(root, &args)?;
    let names: Vec<&str> = names.split('\0').filter(|name| !name.is_empty()).collect();
    if signs.len() > 1 {
        return Ok(names.into_iter().map(String::from).collect());
    }

    let mut args = common;
    args.extend(["-p", "-U0", hex]);
    let patch = git(root).args(&args).stderr(Stdio::null()).output()?;
    let mut matching: Vec<String> = Vec::new();
    // Rang du fichier dont le patch est en cours de lecture.
    let mut file = 0usize;
    let mut in_hunk = false;
    for line in patch.stdout.split(|&byte| byte == b'\n') {
        if line.starts_with(b"diff --git ") {
            file += 1;
            in_hunk = false;
        } else if line.first() == Some(&b'@') {
            in_hunk = true;
        } else if in_hunk
            && line.first().is_some_and(|sign| signs.contains(sign))
            && String::from_utf8_lossy(&line[1..]).to_lowercase().contains(needle)
            && let Some(name) = names.get(file.wrapping_sub(1))
            && matching.last().map(String::as_str) != Some(name)
        {
            matching.push(name.to_string());
        }
    }
    Ok(matching)
}

/// Tri des hunks d'un diff selon une recherche : un hunk n'est transmis que si
/// l'une de ses lignes y répond.
struct HunkSieve {
    needle: String,
    signs: &'static [u8],
    in_hunk: bool,
    /// Hunk en cours de lecture, et s'il répond déjà à la recherche.
    pending: Vec<DiffLine>,
    matched: bool,
    kept: usize,
    hidden: usize,
    /// Hunks écartés, gardés de côté tant qu'aucun n'a répondu : sans aucun
    /// hunk retenu, le diff est montré en entier.
    spare: Vec<DiffLine>,
}

impl HunkSieve {
    fn new(filter: &Filter) -> Option<HunkSieve> {
        Some(HunkSieve {
            needle: filter.query.to_lowercase(),
            signs: filter.hunk_signs()?,
            in_hunk: false,
            pending: Vec::new(),
            matched: false,
            kept: 0,
            hidden: 0,
            spare: Vec::new(),
        })
    }

    /// Reçoit une ligne du diff, `raw` étant son texte brut, signe compris ;
    /// ce qui est bon à afficher rejoint `out`.
    fn push(&mut self, line: DiffLine, raw: &str, out: &mut Vec<DiffLine>) {
        if line.kind == LineKind::Hunk {
            self.close(out);
            self.in_hunk = true;
        }
        if !self.in_hunk {
            // Avant le premier hunk : renommage, fichier binaire.
            return out.push(line);
        }
        let searched = matches!(line.kind, LineKind::Add | LineKind::Del)
            && raw.as_bytes().first().is_some_and(|sign| self.signs.contains(sign));
        if searched && raw.get(1..).is_some_and(|body| body.to_lowercase().contains(&self.needle)) {
            self.matched = true;
        }
        self.pending.push(line);
        if self.matched {
            out.append(&mut self.pending);
        }
    }

    /// Fin du hunk en cours : transmis s'il répond à la recherche, écarté sinon.
    fn close(&mut self, out: &mut Vec<DiffLine>) {
        if !self.in_hunk {
            return;
        }
        if std::mem::take(&mut self.matched) {
            out.append(&mut self.pending);
            self.kept += 1;
            self.spare = Vec::new();
        } else {
            self.hidden += 1;
            if self.kept == 0 {
                self.spare.append(&mut self.pending);
            }
            self.pending.clear();
        }
    }

    fn finish(&mut self, out: &mut Vec<DiffLine>) {
        self.close(out);
        if self.kept == 0 {
            out.append(&mut self.spare);
        } else if self.hidden > 0 {
            let plural = if self.hidden > 1 { "s" } else { "" };
            let text = format!("— {} other hunk{plural} hidden by the search —", self.hidden);
            out.push(DiffLine { kind: LineKind::Meta, old: 0, new: 0, text, tints: Vec::new() });
        }
    }
}

/// Lit le diff d'un fichier ligne à ligne et l'envoie par tranches.
/// Renvoie `false` si l'interface n'écoute plus.
fn stream_diff(root: &Path, id: u64, req: &FileReq, latest: &AtomicU64, tx: &Sender<DiffMsg>) -> bool {
    let meta = |text: String| DiffLine { kind: LineKind::Meta, old: 0, new: 0, text, tints: Vec::new() };
    // Un contexte plus long que tout fichier : le diff porte alors le fichier entier.
    let context = if req.full { "-U1000000" } else { "-U3" };
    let hex = req.oid.to_hex();
    let base = if req.oid.is_worktree() { changes_base(root) } else { String::new() };
    let mut cmd = git(root);
    if !req.oid.is_worktree() {
        cmd.args(SHOW_ARGS).args(["--no-ext-diff", context, hex.as_str(), "--"]).args(&req.paths);
    } else if req.untracked {
        // Un fichier non suivi se compare au vide, hors de l'index.
        cmd.args(["diff", "--no-index", "--no-color", "--no-ext-diff", context, "--", "/dev/null"]).args(&req.paths);
    } else {
        // Ce qui n'est pas commité : le dossier de travail, index compris, face à HEAD.
        cmd.args(["diff", "-M", "--no-color", "--no-ext-diff", context, base.as_str(), "--"]).args(&req.paths);
    }
    let spawned = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(err) => {
            let lines = vec![meta(format!("git show failed: {err}"))];
            return tx.send(DiffMsg::Lines { id, lines, done: true }).is_ok();
        }
    };
    let mut reader = BufReader::new(child.stdout.take().expect("stdout is piped"));
    let mut parser = DiffParser::default();
    // Le premier chemin est celui du fichier dans ce commit : il donne la grammaire.
    let mut colors = req.paths.first().filter(|_| req.syntax).and_then(|path| Colorizer::for_path(path));
    // Le fichier entier se lit sans tri : il sert à voir les hunks dans leur contexte.
    let mut sieve = req.filter.as_ref().filter(|_| !req.full).and_then(HunkSieve::new);
    let mut chunk: Vec<DiffLine> = Vec::new();
    let mut total = 0usize;
    let mut truncated = false;
    let mut raw: Vec<u8> = Vec::new();
    let mut alive = true;
    loop {
        raw.clear();
        match reader.read_until(b'\n', &mut raw) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let text = String::from_utf8_lossy(&raw);
        if let Some(mut line) = parser.feed(&text) {
            let text = text.trim_end_matches(['\n', '\r']);
            if let Some(colors) = &mut colors {
                match line.kind {
                    LineKind::Hunk => colors.reset(),
                    LineKind::Meta => {}
                    // Le contenu brut, sans le signe du diff.
                    kind => line.tints = colors.line(kind, text.get(1..).unwrap_or("")),
                }
            }
            match &mut sieve {
                Some(sieve) => sieve.push(line, text, &mut chunk),
                None => chunk.push(line),
            }
            total += 1;
        }
        if total >= MAX_DIFF_LINES {
            truncated = true;
            let _ = child.kill();
            break;
        }
        if chunk.len() >= DIFF_CHUNK {
            if latest.load(Ordering::SeqCst) != id {
                let _ = child.kill();
                let _ = child.wait();
                return true;
            }
            let lines = std::mem::take(&mut chunk);
            alive = tx.send(DiffMsg::Lines { id, lines, done: false }).is_ok();
            if !alive {
                let _ = child.kill();
                break;
            }
        }
    }
    let _ = child.wait();
    if let Some(sieve) = &mut sieve {
        sieve.finish(&mut chunk);
    }
    if truncated {
        chunk.push(meta(format!("— diff truncated at {MAX_DIFF_LINES} lines —")));
    }
    if total == 0 && chunk.is_empty() {
        chunk.push(meta("No differences to show.".to_string()));
    }
    alive && tx.send(DiffMsg::Lines { id, lines: chunk, done: true }).is_ok()
}

/// Branches prises en compte dans la hiérarchie, les plus récentes d'abord.
const MAX_HIERARCHY_BRANCHES: usize = 150;

/// Lit les branches locales et leurs écarts deux à deux, puis construit
/// l'arbre. Tous les écarts viennent d'un seul `git for-each-ref`, grâce au
/// champ `ahead-behind` (git 2.41 ou plus récent).
fn load_hierarchy(repo: &Repo) -> Result<(Vec<BranchRow>, usize)> {
    let list = output(
        &repo.root,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "refs/heads",
            "--format=%(refname:short)%09%(objectname)%09%(upstream:short)%09%(upstream:track)",
        ],
    )?;
    let mut branches: Vec<Branch> = Vec::new();
    for line in list.lines() {
        let mut parts = line.split('\t');
        let (Some(name), Some(hex)) = (parts.next(), parts.next()) else { continue };
        let Some(oid) = Oid::from_hex(hex) else { continue };
        let upstream = parts.next().unwrap_or("");
        let track = parts.next().unwrap_or("");
        let remote = if upstream.is_empty() {
            Remote::Untracked
        } else if track.contains("gone") {
            Remote::Gone
        } else {
            match (track.contains("ahead"), track.contains("behind")) {
                (true, true) => Remote::Diverged,
                (true, false) => Remote::Ahead,
                (false, true) => Remote::Behind,
                (false, false) => Remote::InSync,
            }
        };
        let is_head = repo.head == Head::Branch(name.to_string());
        branches.push(Branch { name: name.to_string(), oid, is_head, remote });
    }
    let hidden = branches.len().saturating_sub(MAX_HIERARCHY_BRANCHES);
    branches.truncate(MAX_HIERARCHY_BRANCHES);
    if branches.is_empty() {
        return Ok((Vec::new(), 0));
    }

    // Une colonne par commit de tête distinct : les branches au même commit la partagent.
    let mut tips: Vec<Oid> = Vec::new();
    for branch in &branches {
        if !tips.contains(&branch.oid) {
            tips.push(branch.oid);
        }
    }
    let mut format = String::from("--format=%(objectname)");
    for tip in &tips {
        format.push_str(&format!("%09%(ahead-behind:{})", tip.to_hex()));
    }
    let table = output(&repo.root, &["for-each-ref", "refs/heads", &format]).map_err(|err| {
        if err.to_string().contains("ahead-behind") {
            anyhow::anyhow!("the hierarchy view requires git 2.41 or newer")
        } else {
            err
        }
    })?;
    let mut by_tip: HashMap<Oid, Vec<(u32, u32)>> = HashMap::new();
    for line in table.lines() {
        let mut parts = line.split('\t');
        let Some(oid) = parts.next().and_then(Oid::from_hex) else { continue };
        let pairs = parts
            .map(|pair| {
                let mut numbers = pair.split(' ').map(|n| n.parse().unwrap_or(0));
                (numbers.next().unwrap_or(0), numbers.next().unwrap_or(0))
            })
            .collect();
        by_tip.insert(oid, pairs);
    }
    let column = |oid: &Oid| tips.iter().position(|tip| tip == oid).unwrap_or(0);
    let ab: Vec<Vec<(u32, u32)>> = branches
        .iter()
        .map(|b| {
            let row = by_tip.get(&b.oid);
            branches
                .iter()
                .map(|p| row.and_then(|r| r.get(column(&p.oid)).copied()).unwrap_or((0, 0)))
                .collect()
        })
        .collect();
    Ok((hierarchy::build(&branches, &ab), hidden))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_name_from_remote_url() {
        assert_eq!(repo_name("https://example.com/alice/gitlog-tui.git\n").as_deref(), Some("gitlog-tui"));
        assert_eq!(repo_name("git@example.com:alice/gitlog-tui.git").as_deref(), Some("gitlog-tui"));
        assert_eq!(repo_name("ssh://example.com/alice/tools/").as_deref(), Some("tools"));
        assert_eq!(repo_name("C:\\repos\\bare.git").as_deref(), Some("bare"));
        assert_eq!(repo_name(""), None);
    }

    #[test]
    fn commit_object_is_parsed() {
        let raw = "tree 1111\nparent 2222\nauthor Alice Martin <alice@example.com> 1700000000 +0100\ncommitter Bob <bob@example.com> 1700000500 +0100\ngpgsig -----BEGIN-----\n abc\n -----END-----\n\nIndex de profondeur\n\nCorps du message.\n";
        let meta = parse_commit(raw);
        assert_eq!(meta.author, "Alice Martin");
        assert_eq!(meta.time, 1700000500);
        assert_eq!(meta.subject, "Index de profondeur");
    }
}
