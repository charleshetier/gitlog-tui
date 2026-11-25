//! Thème de la coloration syntaxique : les teintes de « Dark+ », le thème sombre
//! de VS Code (`dark_vs.json` et `dark_plus.json`). VS Code colore d'après des
//! grammaires TextMate, syntect d'après celles de Sublime Text : les portées n'y
//! portent pas toujours le même nom (`variable.function` pour un appel, là où
//! VS Code dit `entity.name.function`). Les règles de VS Code sont donc reprises
//! telles quelles quand la portée existe des deux côtés, traduites sinon.

use std::str::FromStr;

use syntect::highlighting::{Color, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings};

/// Texte courant : la coloration le laisse à la couleur de texte du terminal.
const PLAIN: u32 = 0xD4D4D4;
const COMMENT: u32 = 0x6A9955;
const STRING: u32 = 0xCE9178;
const REGEXP: u32 = 0xD16969;
const NUMBER: u32 = 0xB5CEA8;
/// Mots-clés de déclaration, constantes du langage, balises.
const BLUE: u32 = 0x569CD6;
/// Mots-clés de contrôle : `if`, `return`, `import`.
const PURPLE: u32 = 0xC586C0;
const VARIABLE: u32 = 0x9CDCFE;
const CONSTANT: u32 = 0x4FC1FF;
const TYPE: u32 = 0x4EC9B0;
const FUNCTION: u32 = 0xDCDCAA;
/// Séquences d'échappement, sélecteurs CSS.
const GOLD: u32 = 0xD7BA7D;
const TAG_MARK: u32 = 0x808080;
const LIST_MARK: u32 = 0x6796E6;
const LABEL: u32 = 0xC8C8C8;
const INVALID: u32 = 0xF44747;

/// Une teinte et les sélecteurs qui la reçoivent. Entre deux règles qui
/// conviennent, syntect retient la plus précise : l'ordre ne compte pas.
const RULES: &[(u32, &str)] = &[
    (COMMENT, "comment, punctuation.definition.blockquote"),
    (STRING, "string, markup.raw.inline, markup.inline.raw, markup.deleted"),
    // Une docstring est une chaîne pour VS Code, un commentaire pour Sublime Text.
    (STRING, "source.python comment.block.documentation"),
    (STRING, "support.constant.property-value, support.constant.font-name, support.constant.color"),
    (STRING, "support.constant.media, constant.other.color, constant.other.rgb-value"),
    (STRING, "string.regexp punctuation.definition.group"),
    (REGEXP, "string.regexp, constant.character.character-class.regexp, constant.other.character-class"),
    (NUMBER, "constant.numeric, keyword.other.unit, storage.type.numeric, markup.inserted"),
    (BLUE, "keyword, storage, constant.language, constant.character, variable.language"),
    (BLUE, "keyword.operator.word, keyword.operator.new, keyword.operator.expression, keyword.operator.cast"),
    (BLUE, "keyword.operator.sizeof, keyword.operator.instanceof, keyword.operator.wordlike"),
    // `and`, `or`, `not` : des mots, que ces grammaires rangent parmi les opérateurs.
    (BLUE, "source.python keyword.operator.logical, source.sql keyword.operator.logical"),
    (BLUE, "source.sql keyword.operator.assignment.alias, source.java keyword.control.import"),
    (BLUE, "entity.name.tag, entity.name.function.preprocessor, entity.name.constant.preprocessor"),
    (BLUE, "punctuation.section.interpolation, punctuation.definition.template-expression"),
    (BLUE, "punctuation.section.embedded, storage.modifier.lifetime, entity.name.lifetime"),
    (BLUE, "variable.parameter.option, constant.other.option"),
    (BLUE, "markup.heading, markup.bold, markup.changed, meta.diff.header"),
    (PURPLE, "keyword.control, keyword.other.using, keyword.other.operator, keyword.operator.delete"),
    (PURPLE, "entity.name.operator, markup.italic"),
    (PLAIN, "keyword.operator, meta.interpolation, meta.template.expression, storage.type.nullable"),
    (PLAIN, "support.function.test, constant.other.language-name"),
    (PLAIN, "source.sql constant.other, source.sql variable.language"),
    // Chemins d'import et de paquet de Java : VS Code les laisse sans couleur.
    (PLAIN, "source.java meta.import support.type.package, source.java meta.import support.class"),
    (PLAIN, "source.java meta.package-declaration entity.name.namespace"),
    (VARIABLE, "variable, support.variable, entity.name.variable, meta.definition.variable.name"),
    (VARIABLE, "constant.other.placeholder, meta.object-literal.key, meta.structure.dictionary.key.python"),
    (VARIABLE, "entity.other.attribute-name, support.type.property-name, support.type.custom-property"),
    (VARIABLE, "source.shell punctuation.definition.variable"),
    // Clés de JSON et de TOML : des noms de propriété.
    (VARIABLE, "meta.mapping.key string, source.toml entity.name.tag, source.toml entity.name.table"),
    (CONSTANT, "variable.other.constant, variable.other.enummember, constant.other, support.constant"),
    (CONSTANT, "entity.name.constant"),
    (TYPE, "support.class, support.type, entity.name.type, entity.name.class, entity.name.struct"),
    (TYPE, "entity.name.enum, entity.name.union, entity.name.trait, entity.name.interface, entity.name.impl"),
    (TYPE, "entity.name.namespace, entity.name.scope-resolution, entity.other.inherited-class"),
    (TYPE, "entity.other.attribute, meta.type.cast.expr, meta.type.new.expr, entity.name.other.anchor"),
    (TYPE, "support.constant.math, support.constant.dom, support.constant.json"),
    // Types que ces grammaires rangent sous `storage.type`, avec les mots-clés.
    (TYPE, "storage.type.go, storage.type.primitive.java, source.java variable.annotation"),
    (FUNCTION, "entity.name.function, support.function, support.macro, variable.function, variable.annotation"),
    (FUNCTION, "string.regexp keyword.operator.or, keyword.control.anchor.regexp"),
    (GOLD, "constant.character.escape, string.regexp keyword.operator.quantifier"),
    (GOLD, "entity.name.tag.css, entity.other.pseudo-class, entity.other.pseudo-element"),
    (GOLD, "source.css entity.other.attribute-name.class, source.css entity.other.attribute-name.id"),
    (GOLD, "source.scss entity.other.attribute-name.class, source.scss entity.other.attribute-name.id"),
    (GOLD, "source.less entity.other.attribute-name.class, source.less entity.other.attribute-name.id"),
    (GOLD, "source.scss entity.name.tag, source.sass entity.name.tag, source.less entity.name.tag"),
    (TAG_MARK, "punctuation.definition.tag"),
    (LIST_MARK, "punctuation.definition.list_item, punctuation.definition.list.begin"),
    (LABEL, "entity.name.label"),
    (INVALID, "invalid"),
];

/// Le thème, prêt pour syntect. Seule la couleur du texte est fixée : les fonds
/// sont ceux du diff, et le terminal n'affiche ni gras ni italique ici.
pub fn vscode_dark() -> Theme {
    let color = |rgb: u32| Color { r: (rgb >> 16) as u8, g: (rgb >> 8) as u8, b: rgb as u8, a: 0xFF };
    let scopes = RULES
        .iter()
        .map(|&(rgb, selectors)| ThemeItem {
            scope: ScopeSelectors::from_str(selectors).unwrap_or_else(|err| panic!("« {selectors} » : {err:?}")),
            style: StyleModifier { foreground: Some(color(rgb)), background: None, font_style: None },
        })
        .collect();
    Theme {
        name: Some("VS Code Dark+".to_string()),
        author: None,
        settings: ThemeSettings { foreground: Some(color(PLAIN)), ..ThemeSettings::default() },
        scopes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::LineKind;
    use crate::syntax::Colorizer;

    /// Teinte de `word` dans la dernière ligne de `source` ; `PLAIN` sans teinte.
    fn tint(file: &str, source: &str, word: &str) -> u32 {
        let mut colors = Colorizer::for_path(file).unwrap();
        let (mut tints, mut last) = (Vec::new(), "");
        for line in source.lines() {
            (tints, last) = (colors.line(LineKind::Ctx, line), line);
        }
        let byte = last.find(word).unwrap_or_else(|| panic!("« {word} » absent de « {last} »"));
        let at = last[..byte].chars().count();
        let mut end = 0;
        for (len, color) in tints {
            end += len as usize;
            if at < end {
                return color.map_or(PLAIN, |[r, g, b]| u32::from_be_bytes([0, r, g, b]));
            }
        }
        PLAIN
    }

    #[test]
    fn every_selector_parses() {
        assert_eq!(vscode_dark().scopes.len(), RULES.len());
    }

    #[test]
    fn php_takes_the_tints_of_vscode() {
        let source = "<?php\nreturn $this->nature === null ? strlen('abc') : new Foo(1.5, PHP_EOL); // why";
        let php = |word: &str| tint("a.php", source, word);
        assert_eq!((php("return"), php("new")), (PURPLE, BLUE));
        assert_eq!((php("$this"), php("nature"), php("null")), (BLUE, VARIABLE, BLUE));
        assert_eq!((php("strlen"), php("Foo"), php("PHP_EOL")), (FUNCTION, TYPE, CONSTANT));
        assert_eq!((php("'abc'"), php("1.5"), php("// why")), (STRING, NUMBER, COMMENT));
        // Un opérateur garde la couleur du texte, bien que `keyword` soit bleu.
        assert_eq!(php("==="), PLAIN);
    }

    #[test]
    fn sublime_scopes_are_translated() {
        // Un appel : `variable.function`, que la règle `variable` ne doit pas emporter.
        let rust = "let total = compute(MAX, \"a\\n\");";
        assert_eq!((tint("a.rs", rust, "let"), tint("a.rs", rust, "compute")), (BLUE, FUNCTION));
        assert_eq!((tint("a.rs", rust, "MAX"), tint("a.rs", rust, "\\n")), (CONSTANT, GOLD));
        // Une clé JSON est une chaîne dans une portée de clé.
        let json = "{ \"name\": \"demo\", \"ok\": true }";
        assert_eq!((tint("a.json", json, "\"name\""), tint("a.json", json, "\"demo\"")), (VARIABLE, STRING));
        assert_eq!(tint("a.json", json, "true"), BLUE);
        // `class` nomme un attribut en HTML, un sélecteur en CSS.
        let css = ".card > a:hover { color: #ff0000; }";
        assert_eq!((tint("a.css", css, ".card"), tint("a.css", css, "color")), (GOLD, VARIABLE));
        assert_eq!(tint("a.css", css, "#ff0000"), STRING);
        assert_eq!(tint("a.html", "<div class=\"card\">", "class"), VARIABLE);
    }
}
