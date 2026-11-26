//! Ce que `gitmerge` demande à git : les fichiers en conflit, les trois versions
//! de chacun telles que l'index les garde, et d'où vient ce qui arrive.

use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::kit::git::{Head, Oid, git, output};

/// Fichier en conflit : son chemin et, dans l'index, le contenu de la base, de
/// notre version et de celle qui arrive. Il en manque une quand un côté a
/// supprimé le fichier, ou quand les deux l'ont créé.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unmerged {
    pub path: String,
    pub stages: [Option<String>; 3],
    /// Un fichier ordinaire dans toutes ses versions : ni lien, ni sous-module.
    pub regular: bool,
}

/// Fichiers en conflit, dans l'ordre de l'index.
pub fn unmerged(root: &Path) -> Result<Vec<Unmerged>> {
    Ok(parse_unmerged(&output(root, &["ls-files", "-u", "-z"])?))
}

/// Lit la sortie de `git ls-files -u -z` : `mode oid étape<TAB>chemin`, une
/// entrée par version présente.
fn parse_unmerged(out: &str) -> Vec<Unmerged> {
    let mut files: Vec<Unmerged> = Vec::new();
    for entry in out.split('\0') {
        let Some((meta, path)) = entry.split_once('\t') else { continue };
        let mut fields = meta.split(' ');
        let (Some(mode), Some(oid), Some(stage)) = (fields.next(), fields.next(), fields.next().and_then(|stage| stage.parse::<usize>().ok())) else {
            continue;
        };
        if !(1..=3).contains(&stage) {
            continue;
        }
        if files.last().is_none_or(|file| file.path != path) {
            files.push(Unmerged { path: path.to_string(), stages: [None, None, None], regular: true });
        }
        if let Some(file) = files.last_mut() {
            file.stages[stage - 1] = Some(oid.to_string());
            file.regular &= mode.starts_with("100");
        }
    }
    files
}

/// Les trois textes d'un fichier en conflit.
pub struct Versions {
    pub base: String,
    pub ours: String,
    pub theirs: String,
    /// Le fichier de travail finit ses lignes par CR LF.
    pub crlf: bool,
    /// Le fichier de travail ne porte plus les marques de conflit de git : il a
    /// déjà été retouché à la main.
    pub edited: bool,
}

/// Lit les versions d'un fichier ; l'erreur dit pourquoi il ne peut pas être
/// fusionné ici.
pub fn versions(root: &Path, file: &Unmerged) -> Result<Versions, String> {
    if !file.regular {
        return Err("this is a link or a submodule, not a file".to_string());
    }
    let [base, ours, theirs] = &file.stages;
    let (Some(ours), Some(theirs)) = (ours, theirs) else {
        let who = if ours.is_none() { "our side" } else { "the incoming side" };
        return Err(format!("{who} deleted this file"));
    };
    let read = |oid: &str| -> Result<String, String> {
        let out = git(root).args(["cat-file", "blob", oid]).output().map_err(|err| format!("cannot run git: {err}"))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        if out.stdout.contains(&0) {
            return Err("this is a binary file".to_string());
        }
        String::from_utf8(out.stdout).map_err(|_| "this file is not UTF-8 text".to_string())
    };
    let work = std::fs::read(root.join(&file.path)).unwrap_or_default();
    let work = String::from_utf8_lossy(&work);
    Ok(Versions {
        // Sans base, les deux côtés ont créé le fichier : tout y est nouveau.
        base: base.as_deref().map(read).transpose()?.unwrap_or_default(),
        ours: read(ours)?,
        theirs: read(theirs)?,
        crlf: work.contains("\r\n"),
        edited: !work.lines().any(|line| line.starts_with("<<<<<<< ")),
    })
}

/// L'opération qui a laissé des conflits, et ce qu'elle apporte.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Incoming {
    /// `merge`, `rebase`, `cherry-pick`, `revert` ; vide quand rien ne le dit
    /// (un `stash pop`, par exemple).
    pub operation: &'static str,
    /// La branche qui arrive, ou à défaut son commit en abrégé.
    pub name: String,
}

impl Incoming {
    /// Ce qui reste à faire une fois tous les conflits réglés.
    pub fn conclusion(&self) -> &'static str {
        match self.operation {
            "merge" => "git commit concludes the merge",
            "rebase" => "git rebase --continue goes on",
            "cherry-pick" => "git cherry-pick --continue concludes",
            "revert" => "git revert --continue concludes",
            _ => "nothing is left to settle",
        }
    }
}

pub fn incoming(root: &Path) -> Incoming {
    let heads = [("MERGE_HEAD", "merge"), ("REBASE_HEAD", "rebase"), ("CHERRY_PICK_HEAD", "cherry-pick"), ("REVERT_HEAD", "revert")];
    for (head, operation) in heads {
        let Ok(hex) = output(root, &["rev-parse", "-q", "--verify", head]) else { continue };
        let hex = hex.trim();
        let branch = output(root, &["for-each-ref", "--points-at", hex, "--format=%(refname:short)", "refs/heads", "refs/remotes"])
            .ok()
            .and_then(|refs| refs.lines().next().map(str::to_string));
        let name = branch.or_else(|| Oid::from_hex(hex).map(Oid::short)).unwrap_or_else(|| hex.to_string());
        return Incoming { operation, name };
    }
    Incoming { operation: "", name: "theirs".to_string() }
}

/// Nom de notre côté : la branche courante.
pub fn ours_name(head: &Head) -> String {
    match head {
        Head::Branch(name) => name.clone(),
        Head::Detached(oid) => oid.short(),
        Head::Unborn => "ours".to_string(),
    }
}

/// Écrit le résultat dans le fichier de travail.
pub fn save(root: &Path, path: &str, text: &str) -> Result<()> {
    std::fs::write(root.join(path), text).with_context(|| format!("cannot write {path}"))
}

/// `git add` : le conflit du fichier est tenu pour réglé.
pub fn stage(root: &Path, path: &str) -> Result<()> {
    let out = git(root).args(["add", "--", path]).output().context("cannot run git")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmerged_entries() {
        let out = "100644 aaa 1\tsrc/a.rs\x00100644 bbb 2\tsrc/a.rs\x00100644 ccc 3\tsrc/a.rs\x00\
                   100644 ddd 2\tnew file.txt\x00100644 eee 3\tnew file.txt\x00100644 fff 1\tgone.rs\x00100644 abc 3\tgone.rs\x00";
        let files = parse_unmerged(out);
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].stages, [Some("aaa".to_string()), Some("bbb".to_string()), Some("ccc".to_string())]);
        assert_eq!((files[1].path.as_str(), &files[1].stages[0]), ("new file.txt", &None));
        assert_eq!(files[2].stages[1], None);
        assert!(files.iter().all(|file| file.regular));
        // Un lien symbolique n'est pas un texte à fusionner.
        assert!(!parse_unmerged("120000 aaa 2\tlink\x00100644 bbb 3\tlink\x00")[0].regular);
        assert!(parse_unmerged("").is_empty());
    }
}
