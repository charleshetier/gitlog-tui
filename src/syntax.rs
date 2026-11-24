//! Coloration syntaxique des lignes de diff : syntect analyse le texte selon la
//! grammaire du fichier, le thème donne la teinte de chaque segment.

use std::sync::LazyLock;

use syntect::highlighting::{HighlightIterator, HighlightState, Highlighter, Theme};
use syntect::parsing::{ParseState, ScopeStack, SyntaxSet};
use two_face::theme::EmbeddedThemeName;

use crate::diff::{LineKind, TAB_WIDTH, Tint};

/// Au-delà, la ligne (fichier minifié) reste sans couleur : l'analyser coûterait trop.
const MAX_BYTES: usize = 1000;

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_newlines);
/// Thème sombre aux teintes pastel : chacune reste lisible sur les fonds du diff.
/// see https://docs.rs/two-face/latest/two_face/theme/enum.EmbeddedThemeName.html
static THEME: LazyLock<Theme> =
    LazyLock::new(|| two_face::theme::extra().get(EmbeddedThemeName::Base16EightiesDark).clone());

/// Où en est l'analyse d'une version du fichier : elle se poursuit d'une ligne à l'autre.
#[derive(Clone, PartialEq)]
struct State {
    parse: ParseState,
    highlight: HighlightState,
}

/// Colore un diff ligne à ligne. L'ancienne et la nouvelle version sont suivies
/// séparément : une ligne retirée ne doit pas fausser celles qui la remplacent.
pub struct Colorizer {
    highlighter: Highlighter<'static>,
    start: State,
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
        };
        Some(Colorizer { highlighter, old: start.clone(), new: start.clone(), start })
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
        let source = format!("{text}\n");
        match kind {
            LineKind::Add => paint(&mut self.new, &self.highlighter, &source),
            LineKind::Del => paint(&mut self.old, &self.highlighter, &source),
            LineKind::Ctx => {
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
