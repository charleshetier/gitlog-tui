//! Analyse des sorties de `git show` : liste des fichiers d'un commit et diff unifié.

/// Longueur maximale conservée pour une ligne de diff (fichiers minifiés).
const MAX_LINE_CHARS: usize = 2000;
/// Largeur d'une tabulation à l'affichage.
pub const TAB_WIDTH: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    /// Lettre de statut git : M, A, D, R, C, T.
    pub status: char,
    pub path: String,
    pub old_path: Option<String>,
    /// Lignes ajoutées et retirées ; `None` pour un fichier binaire.
    pub added: Option<u32>,
    pub deleted: Option<u32>,
}

/// Analyse la sortie de `git show --raw --numstat` : les lignes `:…` donnent
/// statut et chemin, les lignes numstat suivent dans le même ordre.
pub fn parse_files(output: &str) -> Vec<FileEntry> {
    let mut files: Vec<FileEntry> = Vec::new();
    let mut stat_index = 0;
    for line in output.lines() {
        if let Some(raw) = line.strip_prefix(':') {
            let mut parts = raw.split('\t');
            let meta = parts.next().unwrap_or("");
            let status = meta
                .rsplit(' ')
                .next()
                .and_then(|s| s.chars().next())
                .unwrap_or('M');
            let first = parts.next().unwrap_or("").to_string();
            let (path, old_path) = match parts.next() {
                Some(new) => (new.to_string(), Some(first)),
                None => (first, None),
            };
            files.push(FileEntry { status, path, old_path, added: None, deleted: None });
        } else if !line.is_empty() {
            let mut parts = line.split('\t');
            let (Some(a), Some(d)) = (parts.next(), parts.next()) else { continue };
            if let Some(file) = files.get_mut(stat_index) {
                file.added = a.parse().ok();
                file.deleted = d.parse().ok();
            }
            stat_index += 1;
        }
    }
    files
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Hunk,
    Ctx,
    Add,
    Del,
    Meta,
}

/// Segment d'une ligne colorée : nombre de caractères et teinte RVB (`None` : la
/// couleur de texte du terminal).
pub type Tint = (u16, Option<[u8; 3]>);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    /// Numéros de ligne ancien et nouveau ; 0 quand il n'y en a pas.
    pub old: u32,
    pub new: u32,
    pub text: String,
    /// Coloration syntaxique : segments couvrant `text` dans l'ordre ; vide sans coloration.
    pub tints: Vec<Tint>,
}

/// Analyseur de diff unifié alimenté ligne à ligne, pour suivre le flux de git.
#[derive(Default)]
pub struct DiffParser {
    old: u32,
    new: u32,
    in_hunk: bool,
}

impl DiffParser {
    pub fn feed(&mut self, raw: &str) -> Option<DiffLine> {
        let raw = raw.trim_end_matches(['\n', '\r']);
        let line = |kind, old, new, text: &str| Some(DiffLine { kind, old, new, text: clean(text), tints: Vec::new() });

        if raw.starts_with("diff --") {
            self.in_hunk = false;
            return None;
        }
        if raw.starts_with("@@")
            && let Some((old, new)) = parse_hunk_header(raw)
        {
            self.old = old;
            self.new = new;
            self.in_hunk = true;
            return line(LineKind::Hunk, 0, 0, raw);
        }
        if !self.in_hunk {
            let keep = raw.starts_with("Binary files")
                || raw.starts_with("rename from")
                || raw.starts_with("rename to");
            return if keep { line(LineKind::Meta, 0, 0, raw) } else { None };
        }
        match raw.as_bytes().first() {
            Some(b'+') => {
                self.new += 1;
                line(LineKind::Add, 0, self.new - 1, &raw[1..])
            }
            Some(b'-') => {
                self.old += 1;
                line(LineKind::Del, self.old - 1, 0, &raw[1..])
            }
            Some(b'\\') => line(LineKind::Meta, 0, 0, raw[1..].trim_start()),
            _ => {
                self.old += 1;
                self.new += 1;
                line(LineKind::Ctx, self.old - 1, self.new - 1, raw.get(1..).unwrap_or(""))
            }
        }
    }
}

/// Marque l'absence de ligne d'un côté dans la vue en deux colonnes.
pub const NO_LINE: u32 = u32::MAX;

/// Mise en regard ancien / nouveau, construite au fil du flux : chaque paire
/// désigne la ligne de gauche et celle de droite par leur indice dans le diff.
#[derive(Default)]
pub struct SideBySide {
    pub rows: Vec<(u32, u32)>,
    /// Première ligne retirée qui attend encore son vis-à-vis.
    cursor: usize,
    after_add: bool,
}

impl SideBySide {
    pub fn push(&mut self, index: u32, kind: LineKind) {
        match kind {
            LineKind::Del => {
                // Un retrait après des ajouts ouvre un nouveau bloc.
                if self.after_add {
                    self.cursor = self.rows.len();
                }
                self.after_add = false;
                self.rows.push((index, NO_LINE));
            }
            LineKind::Add => {
                self.after_add = true;
                while self.cursor < self.rows.len() && self.rows[self.cursor].1 != NO_LINE {
                    self.cursor += 1;
                }
                if self.cursor < self.rows.len() {
                    self.rows[self.cursor].1 = index;
                    self.cursor += 1;
                } else {
                    self.rows.push((NO_LINE, index));
                    self.cursor = self.rows.len();
                }
            }
            // « No newline at end of file » ne sépare pas un retrait de son ajout.
            LineKind::Meta => self.rows.push((index, index)),
            LineKind::Hunk | LineKind::Ctx => {
                self.after_add = false;
                self.rows.push((index, index));
                self.cursor = self.rows.len();
            }
        }
    }
}

/// Extrait les numéros de départ de `@@ -a,b +c,d @@`.
fn parse_hunk_header(raw: &str) -> Option<(u32, u32)> {
    let mut parts = raw.split(' ');
    parts.next()?;
    let start = |s: &str, sign: char| -> Option<u32> {
        s.strip_prefix(sign)?.split(',').next()?.parse().ok()
    };
    let old = start(parts.next()?, '-')?;
    let new = start(parts.next()?, '+')?;
    Some((old, new))
}

/// Rend une ligne affichable : tabulations étendues, caractères de contrôle neutralisés.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (n, c) in text.chars().enumerate() {
        if n >= MAX_LINE_CHARS {
            out.push('…');
            break;
        }
        match c {
            '\t' => out.extend([' '; TAB_WIDTH]),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_with_rename_and_binary() {
        let out = "\
:100644 100644 1111111 2222222 M\tsrc/app.rs
:000000 100644 0000000 3333333 A\tsrc/ui/depth_index.rs
:100644 100644 4444444 4444444 R100\told name.rs\tnew name.rs
:100644 100644 5555555 6666666 M\tlogo.png
12\t4\tsrc/app.rs
5\t0\tsrc/ui/depth_index.rs
0\t0\told name.rs => new name.rs
-\t-\tlogo.png
";
        let files = parse_files(out);
        assert_eq!(files.len(), 4);
        assert_eq!((files[0].status, files[0].added, files[0].deleted), ('M', Some(12), Some(4)));
        assert_eq!((files[1].status, files[1].path.as_str()), ('A', "src/ui/depth_index.rs"));
        assert_eq!(files[2].status, 'R');
        assert_eq!(files[2].path, "new name.rs");
        assert_eq!(files[2].old_path.as_deref(), Some("old name.rs"));
        assert_eq!((files[3].added, files[3].deleted), (None, None));
    }

    #[test]
    fn unified_diff_line_numbers() {
        let diff = "\
diff --git a/src/a.rs b/src/a.rs
index 1111111..2222222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -42,3 +42,4 @@ fn render_header
 let title = 1;
-let area = 2;
+let head = 3;
+\tlet depth = 4;
 done
\\ No newline at end of file
";
        let mut parser = DiffParser::default();
        let lines: Vec<DiffLine> = diff.lines().filter_map(|l| parser.feed(l)).collect();
        let summary: Vec<(LineKind, u32, u32)> = lines.iter().map(|l| (l.kind, l.old, l.new)).collect();
        assert_eq!(
            summary,
            [
                (LineKind::Hunk, 0, 0),
                (LineKind::Ctx, 42, 42),
                (LineKind::Del, 43, 0),
                (LineKind::Add, 0, 43),
                (LineKind::Add, 0, 44),
                (LineKind::Ctx, 44, 45),
                (LineKind::Meta, 0, 0),
            ]
        );
        assert_eq!(lines[4].text, "    let depth = 4;");
        assert_eq!(lines[6].text, "No newline at end of file");
    }

    #[test]
    fn binary_file_is_summarised() {
        let diff = "diff --git a/logo.png b/logo.png\nindex 1..2 100644\nBinary files a/logo.png and b/logo.png differ\n";
        let mut parser = DiffParser::default();
        let lines: Vec<DiffLine> = diff.lines().filter_map(|l| parser.feed(l)).collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].kind, LineKind::Meta);
    }
}

#[cfg(test)]
mod side_by_side_tests {
    use super::*;

    fn pair(kinds: &[LineKind]) -> Vec<(u32, u32)> {
        let mut sides = SideBySide::default();
        for (i, kind) in kinds.iter().enumerate() {
            sides.push(i as u32, *kind);
        }
        sides.rows
    }

    #[test]
    fn removed_lines_face_added_lines() {
        use LineKind::*;
        // contexte, 2 retraits, 3 ajouts, contexte
        assert_eq!(
            pair(&[Ctx, Del, Del, Add, Add, Add, Ctx]),
            [(0, 0), (1, 3), (2, 4), (NO_LINE, 5), (6, 6)]
        );
    }

    #[test]
    fn separate_blocks_do_not_mix() {
        use LineKind::*;
        // un retrait seul, un contexte, un ajout seul ; puis retrait, ajout, retrait
        assert_eq!(
            pair(&[Del, Ctx, Add, Del, Add, Del]),
            [(0, NO_LINE), (1, 1), (NO_LINE, 2), (3, 4), (5, NO_LINE)]
        );
    }

    #[test]
    fn no_newline_marker_keeps_the_pair() {
        use LineKind::*;
        assert_eq!(pair(&[Del, Meta, Add]), [(0, 2), (1, 1)]);
    }
}
