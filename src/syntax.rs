//! Coloration syntaxique des lignes de diff : syntect analyse le texte selon la
//! grammaire du fichier, le thème donne la teinte de chaque segment.

use std::sync::LazyLock;

use syntect::highlighting::{HighlightIterator, HighlightState, Highlighter, Theme};
use syntect::parsing::{ParseState, ScopeStack, SyntaxSet};

use crate::diff::{LineKind, TAB_WIDTH, Tint};

/// Au-delà, la ligne (fichier minifié) reste sans couleur : l'analyser coûterait trop.
const MAX_BYTES: usize = 1000;

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_newlines);
/// Les teintes du thème sombre de VS Code : chacune reste lisible sur les fonds du diff.
static THEME: LazyLock<Theme> = LazyLock::new(crate::theme::vscode_dark);

/// Où en est l'analyse d'une version du fichier : elle se poursuit d'une ligne à l'autre.
#[derive(Clone, PartialEq)]
struct State {
    parse: ParseState,
    highlight: HighlightState,
    /// Aucune ligne de code n'a encore été lue depuis le début du hunk.
    fresh: bool,
}

/// Colore un diff ligne à ligne. L'ancienne et la nouvelle version sont suivies
/// séparément : une ligne retirée ne doit pas fausser celles qui la remplacent.
pub struct Colorizer {
    highlighter: Highlighter<'static>,
    start: State,
    /// PHP : l'état une fois passée la balise `<?php`. La grammaire commence
    /// en HTML, or un hunk pris au milieu d'un fichier est presque toujours du PHP.
    inside: Option<State>,
    old: State,
    new: State,
}

impl Colorizer {
    /// Grammaire d'après le nom du fichier (`Makefile`) puis son extension ;
    /// `None` quand aucune ne convient.
    pub fn for_path(path: &str) -> Option<Colorizer> {
        let name = path.rsplit('/').next().unwrap_or(path);
        let extension = name.rsplit_once('.').map(|(_, extension)| extension);
        let syntax = SYNTAXES
            .find_syntax_by_extension(name)
            .or_else(|| extension.and_then(|e| SYNTAXES.find_syntax_by_extension(e)))
            .filter(|syntax| syntax.name != "Plain Text")?;
        let highlighter = Highlighter::new(&THEME);
        let start = State {
            parse: ParseState::new(syntax),
            highlight: HighlightState::new(&highlighter, ScopeStack::new()),
            fresh: true,
        };
        let inside = (syntax.name == "PHP").then(|| {
            let mut state = start.clone();
            paint(&mut state, &highlighter, "<?php\n");
            state
        });
        Some(Colorizer { highlighter, old: start.clone(), new: start.clone(), start, inside })
    }

    /// Un nouveau hunk : les lignes ne suivent plus les précédentes.
    pub fn reset(&mut self) {
        self.old = self.start.clone();
        self.new = self.start.clone();
    }

    /// Teintes d'une ligne du diff, `text` étant son contenu brut, sans son signe.
    pub fn line(&mut self, kind: LineKind, text: &str) -> Vec<Tint> {
        if text.len() > MAX_BYTES {
            return Vec::new();
        }
        // Les grammaires attendent la fin de ligne.
        let mut source = format!("{text}\n");
        // PHP 8 : `#[` ouvre un attribut, que la grammaire prend encore pour un
        // commentaire. Sans son `#`, elle le lit comme du code, caractère pour caractère.
        if self.inside.is_some() && text.trim_start().starts_with("#[") {
            source = source.replacen('#', " ", 1);
        }
        let inside = self.inside.as_ref();
        match kind {
            LineKind::Add => {
                settle(&mut self.new, inside, text);
                paint(&mut self.new, &self.highlighter, &source)
            }
            LineKind::Del => {
                settle(&mut self.old, inside, text);
                paint(&mut self.old, &self.highlighter, &source)
            }
            LineKind::Ctx => {
                settle(&mut self.old, inside, text);
                settle(&mut self.new, inside, text);
                // Une ligne commune fait avancer les deux versions.
                let same = self.old == self.new;
                let tints = paint(&mut self.new, &self.highlighter, &source);
                if same {
                    self.old = self.new.clone();
                } else {
                    paint(&mut self.old, &self.highlighter, &source);
                }
                tints
            }
            LineKind::Hunk | LineKind::Meta => Vec::new(),
        }
    }
}

/// Première ligne de code d'un hunk : elle dit si l'on est déjà dans le PHP. Une
/// ligne qui s'ouvre par une balise (`<?php`, `<div>`) part du HTML, comme le
/// début du fichier ; toute autre est tenue pour du PHP.
fn settle(state: &mut State, inside: Option<&State>, text: &str) {
    if !state.fresh || text.trim().is_empty() {
        return;
    }
    if let Some(inside) = inside
        && !text.trim_start().starts_with('<')
    {
        *state = inside.clone();
    }
    state.fresh = false;
}

/// Analyse une ligne et la découpe en segments de même teinte, comptés en
/// caractères affichés (tabulations étendues comme dans le texte du diff).
fn paint(state: &mut State, highlighter: &Highlighter<'_>, line: &str) -> Vec<Tint> {
    let Ok(ops) = state.parse.parse_line(line, &SYNTAXES) else {
        return Vec::new();
    };
    let plain = THEME.settings.foreground;
    let mut tints: Vec<Tint> = Vec::new();
    for (style, piece) in HighlightIterator::new(&mut state.highlight, &ops, line, highlighter) {
        let len: usize = piece
            .chars()
            .map(|c| match c {
                '\n' => 0,
                '\t' => TAB_WIDTH,
                _ => 1,
            })
            .sum();
        if len == 0 {
            continue;
        }
        let fg = style.foreground;
        // La teinte par défaut du thème laisse la couleur de texte du terminal.
        let color = (Some(fg) != plain).then_some([fg.r, fg.g, fg.b]);
        match tints.last_mut() {
            Some((run, last)) if *last == color => *run += len as u16,
            _ => tints.push((len as u16, color)),
        }
    }
    if matches!(tints.as_slice(), [(_, None)]) {
        tints.clear();
    }
    tints
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(tints: &[Tint]) -> usize {
        tints.iter().map(|&(len, _)| len as usize).sum()
    }

    #[test]
    fn grammar_follows_the_file_name() {
        assert!(Colorizer::for_path("src/main.rs").is_some());
        assert!(Colorizer::for_path("Cargo.toml").is_some());
        assert!(Colorizer::for_path("web/app.ts").is_some());
        assert!(Colorizer::for_path("Makefile").is_some());
        assert!(Colorizer::for_path("notes.txt").is_none());
        assert!(Colorizer::for_path("LICENSE").is_none());
    }

    #[test]
    fn tints_cover_the_displayed_text() {
        let mut colors = Colorizer::for_path("a.rs").unwrap();
        let tints = colors.line(LineKind::Ctx, "\tlet answer = \"forty-two\"; // why");
        // La tabulation compte pour quatre caractères, comme à l'affichage.
        assert_eq!(chars(&tints), 4 + "let answer = \"forty-two\"; // why".chars().count());
        let distinct: std::collections::HashSet<_> = tints.iter().filter_map(|&(_, color)| color).collect();
        assert!(distinct.len() >= 3, "keyword, string and comment have their own tint: {tints:?}");
    }

    #[test]
    fn a_comment_continues_on_the_next_line() {
        let mut colors = Colorizer::for_path("a.rs").unwrap();
        let code = colors.line(LineKind::Ctx, "let x = 1;");
        colors.line(LineKind::Ctx, "/* opened");
        let inside = colors.line(LineKind::Ctx, "let x = 1;");
        assert!(code.len() > 1);
        assert_eq!(inside.len(), 1, "one tint, the comment's: {inside:?}");
        // Un nouveau hunk repart d'un état neuf.
        colors.reset();
        assert_eq!(colors.line(LineKind::Ctx, "let x = 1;"), code);
    }

    #[test]
    fn removed_lines_do_not_leak_into_added_ones() {
        let mut colors = Colorizer::for_path("a.rs").unwrap();
        let code = colors.line(LineKind::Ctx, "let x = 1;");
        colors.line(LineKind::Del, "/* removed opening");
        assert_eq!(colors.line(LineKind::Add, "let x = 1;"), code);
        // L'ancienne version, elle, est toujours dans le commentaire.
        assert_eq!(colors.line(LineKind::Del, "let x = 1;").len(), 1);
    }

    #[test]
    fn a_php_hunk_starts_inside_the_php() {
        let tinted = |tints: &[Tint]| tints.iter().filter_map(|&(_, color)| color).collect::<std::collections::HashSet<_>>();
        // Au milieu d'un fichier, sans la balise `<?php` : du PHP quand même.
        let mut colors = Colorizer::for_path("src/Entity/DataSet.php").unwrap();
        let code = colors.line(LineKind::Ctx, "        return $this->nature === null ? 'none' : 42;");
        assert!(tinted(&code).len() >= 4, "keyword, variable, string, number: {code:?}");
        // Une ligne vide en tête de hunk ne décide de rien ; les deux versions suivent.
        colors.reset();
        assert!(colors.line(LineKind::Ctx, "").is_empty());
        assert_eq!(colors.line(LineKind::Del, "        return $this->nature === null ? 'none' : 42;"), code);
        assert_eq!(colors.line(LineKind::Add, "        return $this->nature === null ? 'none' : 42;"), code);
        // Le début du fichier : la balise ouvre le PHP, sans être lue comme du code.
        colors.reset();
        let tag = colors.line(LineKind::Ctx, "<?php");
        assert_eq!(tag.len(), 1, "{tag:?}");
        assert_eq!(colors.line(LineKind::Ctx, "        return $this->nature === null ? 'none' : 42;"), code);
        // Un hunk de gabarit qui s'ouvre sur une balise reste du HTML.
        colors.reset();
        let mut html = Colorizer::for_path("page.html").unwrap();
        let line = "<div class=\"card\">";
        assert_eq!(colors.line(LineKind::Ctx, line), html.line(LineKind::Ctx, line));
        // Les autres langages ne changent pas.
        assert!(Colorizer::for_path("a.rs").unwrap().inside.is_none());
    }

    #[test]
    fn a_php_attribute_is_not_a_comment() {
        let mut colors = Colorizer::for_path("a.php").unwrap();
        let comment = colors.line(LineKind::Ctx, "    # a note about ['default' => false]");
        assert_eq!(comment.iter().filter(|(_, color)| color.is_some()).count(), 1, "one tint: {comment:?}");
        // Un attribut est coloré comme du code, sur toute la longueur de la ligne.
        let line = "    #[ORM\\Column(options: ['default' => false])]";
        let attribute = colors.line(LineKind::Ctx, line);
        assert_eq!(chars(&attribute), line.chars().count());
        let tinted: std::collections::HashSet<_> = attribute.iter().filter_map(|&(_, color)| color).collect();
        assert!(tinted.len() >= 3 && !tinted.contains(&comment[1].1.unwrap()), "{attribute:?}");
        // La ligne suivante se lit comme si l'attribut n'était pas là.
        let after = colors.line(LineKind::Ctx, "    private bool $mvt = false;");
        colors.reset();
        assert_eq!(colors.line(LineKind::Ctx, "    private bool $mvt = false;"), after);
        // Ailleurs, `#[` reste ce qu'en dit la grammaire.
        let mut rust = Colorizer::for_path("a.rs").unwrap();
        assert!(rust.line(LineKind::Ctx, "#[derive(Debug)]").len() > 1);
    }

    /// Débit de la coloration : `cargo test --release bench_colouring -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn bench_colouring() {
        let source = include_str!("ui.rs");
        let start = std::time::Instant::now();
        let mut colors = Colorizer::for_path("ui.rs").unwrap();
        let mut seen = std::collections::BTreeSet::new();
        let mut lines = 0;
        for _ in 0..20 {
            colors.reset();
            for line in source.lines() {
                seen.extend(colors.line(LineKind::Ctx, line).into_iter().filter_map(|(_, color)| color));
                lines += 1;
            }
        }
        let elapsed = start.elapsed();
        println!("{lines} lines in {elapsed:?}: {:.0} lines/s", lines as f64 / elapsed.as_secs_f64());
        println!("tints: {seen:?}");
    }

    #[test]
    fn minified_lines_stay_plain() {
        let mut colors = Colorizer::for_path("a.js").unwrap();
        assert!(colors.line(LineKind::Add, &"a=1;".repeat(1000)).is_empty());
    }
}
