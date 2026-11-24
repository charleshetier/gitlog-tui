//! Accès au dépôt par le processus `git` de l'hôte, toujours en flux :
//! `git log` pour l'historique, `git cat-file --batch` pour les métadonnées
//! des lignes visibles, `git show` pour les fichiers et les diffs.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::thread;

use anyhow::{Context, Result, bail};

use crate::diff::{DiffLine, DiffParser, FileEntry, LineKind, parse_files};
use crate::graph::{Batch, Layout, Oid};
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
    pub name: String,
    pub head: Head,
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
        Ok(Repo { root, name, head })
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

/// Lit `git log` dans un thread et envoie les lignes du graphe par lots sur un
/// canal borné. Quand l'interface ne consomme plus, le thread se bloque sur le
/// canal, puis git sur son tube : rien n'est lu d'avance au-delà de la file.
pub fn spawn_log(root: PathBuf) -> Receiver<LogMsg> {
    let (tx, rx) = mpsc::sync_channel(BATCH_QUEUE);
    thread::spawn(move || {
        if let Err(err) = read_log(&root, &tx) {
            let _ = tx.send(LogMsg::Error(format!("{err:#}")));
        }
    });
    rx
}

fn read_log(root: &Path, tx: &SyncSender<LogMsg>) -> Result<()> {
    let mut child = git(root)
        .args(["log", "--all", "--date-order", "--format=%H %P"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("cannot run git log")?;
    let stdout = BufReader::new(child.stdout.take().context("git log has no output")?);

    let mut layout = Layout::default();
    let mut batch = Batch::default();
    let mut parents: Vec<Oid> = Vec::new();
    // Premier lot réduit pour afficher l'écran au plus tôt.
    let mut limit = 64;
    for line in stdout.lines() {
        let line = line?;
        let mut hashes = line.split(' ').filter(|s| !s.is_empty());
        let Some(oid) = hashes.next().and_then(Oid::from_hex) else { continue };
        parents.clear();
        parents.extend(hashes.filter_map(Oid::from_hex));
        let (col, cells) = layout.push(&oid, &parents);
        batch.push(&oid, col, cells);
        if batch.rows.len() >= limit {
            limit = BATCH_ROWS;
            if tx.send(LogMsg::Batch(std::mem::take(&mut batch))).is_err() {
                // L'interface est partie : on arrête git.
                let _ = child.kill();
                let _ = child.wait();
                return Ok(());
            }
        }
    }
    if !batch.rows.is_empty() {
        let _ = tx.send(LogMsg::Batch(batch));
    }
    let mut stderr = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr);
    }
    if !child.wait()?.success() && !stderr.trim().is_empty() {
        bail!("git log: {}", stderr.trim());
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

pub enum DiffReq {
    Files { id: u64, oid: Oid },
    File { id: u64, oid: Oid, paths: Vec<String> },
    Hierarchy { id: u64 },
}

pub enum DiffMsg {
    Files { id: u64, result: Result<Vec<FileEntry>, String> },
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
                    DiffReq::Files { id, oid } => {
                        let result = list_files(&root, &oid).map_err(|e| format!("{e:#}"));
                        msg_tx.send(DiffMsg::Files { id, result }).is_ok()
                    }
                    DiffReq::File { id, oid, paths } => {
                        stream_diff(&root, id, &oid, &paths, &latest_in_thread, &msg_tx)
                    }
                    DiffReq::Hierarchy { id } => {
                        let result = load_hierarchy(&repo).map_err(|e| format!("{e:#}"));
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

    pub fn request_files(&self, oid: Oid) -> u64 {
        let id = self.next_id();
        let _ = self.tx.send(DiffReq::Files { id, oid });
        id
    }

    pub fn request_file(&self, oid: Oid, paths: Vec<String>) -> u64 {
        let id = self.next_id();
        let _ = self.tx.send(DiffReq::File { id, oid, paths });
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

fn list_files(root: &Path, oid: &Oid) -> Result<Vec<FileEntry>> {
    let hex = oid.to_hex();
    let mut args = SHOW_ARGS.to_vec();
    args.extend(["--raw", "--numstat", "--no-abbrev", hex.as_str()]);
    Ok(parse_files(&output(root, &args)?))
}

/// Lit le diff d'un fichier ligne à ligne et l'envoie par tranches.
/// Renvoie `false` si l'interface n'écoute plus.
fn stream_diff(
    root: &Path,
    id: u64,
    oid: &Oid,
    paths: &[String],
    latest: &AtomicU64,
    tx: &Sender<DiffMsg>,
) -> bool {
    let meta = |text: String| DiffLine { kind: LineKind::Meta, old: 0, new: 0, text };
    let spawned = git(root)
        .args(SHOW_ARGS)
        .args(["--no-ext-diff", oid.to_hex().as_str(), "--"])
        .args(paths)
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
    let mut chunk: Vec<DiffLine> = Vec::new();
    let mut total = 0usize;
    let mut raw: Vec<u8> = Vec::new();
    let mut alive = true;
    loop {
        raw.clear();
        match reader.read_until(b'\n', &mut raw) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if let Some(line) = parser.feed(&String::from_utf8_lossy(&raw)) {
            chunk.push(line);
            total += 1;
        }
        if total >= MAX_DIFF_LINES {
            chunk.push(meta(format!("— diff truncated at {MAX_DIFF_LINES} lines —")));
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
    fn commit_object_is_parsed() {
        let raw = "tree 1111\nparent 2222\nauthor Alice Martin <alice@example.com> 1700000000 +0100\ncommitter Bob <bob@example.com> 1700000500 +0100\ngpgsig -----BEGIN-----\n abc\n -----END-----\n\nIndex de profondeur\n\nCorps du message.\n";
        let meta = parse_commit(raw);
        assert_eq!(meta.author, "Alice Martin");
        assert_eq!(meta.time, 1700000500);
        assert_eq!(meta.subject, "Index de profondeur");
    }
}
