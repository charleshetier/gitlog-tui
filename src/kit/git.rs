//! Socle git de la suite : le dépôt, ses identifiants d'objet, et de quoi lancer
//! une commande. Chaque outil y ajoute les lectures qui lui sont propres.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

/// Identifiant d'objet git (SHA-1 sur 20 octets ou SHA-256 sur 32).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Oid {
    bytes: [u8; 32],
    len: u8,
}

impl Oid {
    /// Identifiant réservé à la ligne des modifications non commitées : tout à
    /// zéro, celui que git emploie pour « aucun objet ». Il prend la longueur
    /// de `like`, car le graphe range des identifiants de taille fixe.
    pub fn worktree(like: &Oid) -> Oid {
        Oid { bytes: [0; 32], len: like.len }
    }

    pub fn is_worktree(self) -> bool {
        self.bytes == [0; 32]
    }

    pub fn from_hex(hex: &str) -> Option<Oid> {
        let h = hex.as_bytes();
        if (h.len() != 40 && h.len() != 64) || !h.iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        let val = |c: u8| (c as char).to_digit(16).unwrap() as u8;
        let mut bytes = [0u8; 32];
        for (i, pair) in h.chunks(2).enumerate() {
            bytes[i] = val(pair[0]) << 4 | val(pair[1]);
        }
        Some(Oid { bytes, len: (h.len() / 2) as u8 })
    }

    pub fn from_bytes(b: &[u8]) -> Oid {
        let mut bytes = [0u8; 32];
        bytes[..b.len()].copy_from_slice(b);
        Oid { bytes, len: b.len() as u8 }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }

    pub fn to_hex(self) -> String {
        self.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn short(self) -> String {
        let mut s = self.to_hex();
        s.truncate(7);
        s
    }
}

pub fn git(root: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(root)
        .args(["--no-pager", "-c", "core.quotePath=false"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null());
    cmd
}

/// Lance une commande git courte et renvoie sa sortie standard.
pub fn output(root: &Path, args: &[&str]) -> Result<String> {
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
}
