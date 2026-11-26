//! Briques de dessin communes aux outils de la suite : les teintes, le texte
//! coupé ou replié, une ligne de code colorée, les panneaux, les menus, l'aide et
//! les messages. Un outil compose son écran avec elles : c'est ce qui donne à la
//! suite un seul visage.

use std::ops::Range;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use crate::kit::git::Repo;
use crate::kit::syntax::Tint;

/// Ligne sous le curseur, dans une liste comme dans le graphe.
pub const SEL_BG: Color = Color::Indexed(237);

/// Code sélectionné à la souris dans un diff.
pub const SELECT_BG: Color = Color::Indexed(18);

/// Occurrences d'une recherche dans le code, en noir sur ces fonds : la
/// courante, puis les autres.
pub const FIND_CURRENT_BG: Color = Color::Indexed(208);
pub const FIND_BG: Color = Color::Indexed(179);

/// Encadré du libellé du commit, en tête du volet.
pub const CARD_BG: Color = Color::Indexed(236);

/// Fond du panneau flottant des raccourcis.
pub const PANEL_BG: Color = Color::Indexed(235);

/// Teinte d'accent : touches du panneau des raccourcis, marque de sélection.
pub const ACCENT: Color = Color::LightBlue;

/// Pastille d'un niveau ou d'un compteur, dans un en-tête.
pub const CHIP_BG: Color = Color::Indexed(238);

/// Texte en retrait : numéros de ligne, rappels, ce qui s'efface derrière le contenu.
pub const DIM: Color = Color::DarkGray;

/// Fond du côté sans vis-à-vis dans le diff en deux colonnes.
pub const VOID_BG: Color = Color::Indexed(235);

/// Fonds d'un diff : les lignes ajoutées et retirées et, plus soutenus, les
/// passages modifiés au sein d'une ligne.
#[derive(Clone, Copy)]
pub struct Tones {
    pub add: Color,
    pub del: Color,
    pub add_mark: Color,
    pub del_mark: Color,
}

pub const TONES: Tones = Tones {
    add: Color::Indexed(22),
    del: Color::Indexed(52),
    add_mark: Color::Indexed(28),
    del_mark: Color::Indexed(88),
};

/// Les mêmes fonds sous la coloration syntaxique : plus sombres, pour que chaque
/// teinte du thème y reste lisible. Aucune couleur de la palette n'est aussi sombre.
/// Un terminal qui ramène le 24 bits à 256 couleurs doit pourtant y retrouver du
/// vert et du rouge : trop sombre, un fond y devient un gris proche du noir.
pub const SYNTAX_TONES: Tones = Tones {
    add: Color::Rgb(0, 56, 0),
    del: Color::Rgb(63, 0, 1),
    add_mark: Color::Rgb(0, 88, 0),
    del_mark: Color::Rgb(128, 0, 8),
};

/// Largeur de code en deçà de laquelle le retour à la ligne est abandonné.
pub const MIN_WRAP: usize = 8;

pub fn width(text: &str) -> u16 {
    Span::raw(text).width() as u16
}

/// Écrit `text` à partir de `x` sans dépasser `end` ; renvoie la colonne suivante.
pub fn put(buf: &mut Buffer, x: u16, y: u16, end: u16, text: &str, style: Style) -> u16 {
    if x >= end {
        return x;
    }
    buf.set_stringn(x, y, text, (end - x) as usize, style).0
}

/// Écrit `text` aligné à droite sur `end` ; renvoie sa colonne de départ.
pub fn put_right(buf: &mut Buffer, start: u16, y: u16, end: u16, text: &str, style: Style) -> u16 {
    let x = end.saturating_sub(width(text)).max(start);
    put(buf, x, y, end, text, style);
    x
}

/// Garde la fin d'un chemin trop long : c'est le nom du fichier qui compte.
pub fn tail(text: &str, max: u16) -> String {
    let count = text.chars().count();
    if width(text) <= max || max < 2 {
        return text.to_string();
    }
    let keep = (max as usize - 1).min(count);
    let kept: String = text.chars().skip(count - keep).collect();
    format!("…{kept}")
}

pub fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Coupe `text` à ce qui tient dans `room` cellules, de préférence sur une
/// espace. Renvoie ce qui tient, puis la suite ; il tient toujours un caractère.
pub fn split_words(text: &str, room: u16) -> (&str, &str) {
    let mut used = 0;
    for (at, c) in text.char_indices() {
        used += width(c.encode_utf8(&mut [0; 4]));
        if used > room && at > 0 {
            // Un mot plus long que la moitié de la place est coupé où il se trouve.
            return match text[..at].rfind(' ').filter(|&space| space * 2 >= at) {
                Some(space) => (&text[..space], text[space..].trim_start()),
                None => text.split_at(at),
            };
        }
    }
    (text, "")
}

/// Applique un style à un passage d'une ligne de code dessinée en `x` : `from`
/// et `to` sont des rangs de caractères dans le texte, `skip` le défilement.
pub fn mark_span(buf: &mut Buffer, (x, y, end): (u16, u16, u16), skip: usize, (from, to): (usize, usize), style: Style) {
    let room = end.saturating_sub(x) as usize;
    let (start, stop) = (from.saturating_sub(skip).min(room), to.saturating_sub(skip).min(room));
    if start < stop {
        buf.set_style(Rect::new(x + start as u16, y, (stop - start) as u16, 1), style);
    }
}

/// Écrit une ligne de code à partir du caractère `skip`, de `x` jusqu'à `end`. Le
/// texte est découpé là où change la teinte syntaxique ou le fond : `marks`
/// désigne, en rangs de caractères, des passages qui reçoivent le fond `mark_bg`.
pub fn put_tinted(
    buf: &mut Buffer,
    (mut x, y, end): (u16, u16, u16),
    text: &str,
    tints: &[Tint],
    skip: usize,
    (marks, mark_bg): (&[Range<usize>], Color),
) {
    // Octet de chaque caractère, puis la fin du texte.
    let offsets: Vec<usize> = text.char_indices().map(|(at, _)| at).chain([text.len()]).collect();
    let count = offsets.len() - 1;
    let mut cuts = vec![skip.min(count), count];
    let mut tint_end = 0;
    for &(len, _) in tints {
        tint_end += len as usize;
        cuts.push(tint_end.min(count));
    }
    cuts.extend(marks.iter().flat_map(|mark| [mark.start.min(count), mark.end.min(count)]));
    cuts.sort_unstable();
    cuts.dedup();

    let mut tints = tints.iter();
    let (mut tint_end, mut color) = (0, None);
    for cut in cuts.windows(2) {
        let (from, to) = (cut[0], cut[1]);
        while from >= tint_end {
            // Sans coloration, ou au-delà de ses segments : la couleur de texte du terminal.
            (tint_end, color) = match tints.next() {
                Some(&(len, color)) => (tint_end + len as usize, color),
                None => (usize::MAX, None),
            };
        }
        if from < skip {
            continue;
        }
        let mut style = color.map_or(Style::new(), |[r, g, b]| Style::new().fg(Color::Rgb(r, g, b)));
        if marks.iter().any(|mark| mark.contains(&from)) {
            style = style.bg(mark_bg);
        }
        x = put(buf, x, y, end, &text[offsets[from]..offsets[to]], style);
        if x >= end {
            return;
        }
    }
}

/// Cadre d'un panneau flottant : fond uni, titre en haut, rappel de fermeture en bas.
pub fn draw_panel(buf: &mut Buffer, panel: Rect, title: &str, close: &str) {
    if panel.is_empty() {
        return;
    }
    let base = Style::new().bg(PANEL_BG);
    let frame = base.fg(ACCENT);
    let (left, right, top, bottom) = (panel.x, panel.right() - 1, panel.y, panel.bottom() - 1);
    for y in top..=bottom {
        for x in left..=right {
            let symbol = match (x == left, x == right, y == top, y == bottom) {
                (true, _, true, _) => "╭",
                (_, true, true, _) => "╮",
                (true, _, _, true) => "╰",
                (_, true, _, true) => "╯",
                (true, ..) | (_, true, ..) => "│",
                (_, _, true, _) | (_, _, _, true) => "─",
                _ => " ",
            };
            buf[(x, y)].reset();
            buf[(x, y)].set_symbol(symbol).set_style(frame);
        }
    }
    put(buf, left + 2, top, right, title, frame.add_modifier(Modifier::BOLD));
    put_right(buf, left + 2, bottom, right.saturating_sub(1), close, base.fg(DIM));
}

/// Sous-menu d'une touche à deux temps, posé en bas à gauche : celui de la
/// recherche, au-dessus du champ de saisie, ou celui des branches. Après un `+`
/// ou un `-`, seule la ligne qui peut encore suivre reste en évidence.
pub fn draw_menu(buf: &mut Buffer, area: Rect, title: &str, entries: &[(&str, &str)], sign: Option<char>) {
    let key_w = 2;
    let text_w = entries.iter().map(|(_, text)| width(text)).max().unwrap_or(0);
    let w = (key_w + 2 + text_w + 6).min(area.width);
    let h = (entries.len() as u16 + 2).min(area.height);
    let panel = Rect::new(area.x + 1.min(area.width - w), area.bottom() - h, w, h);
    draw_panel(buf, panel, title, " Esc to cancel ");
    let base = Style::new().bg(PANEL_BG);
    let (x0, right) = (panel.x + 3, panel.right() - 1);
    for (i, (keys, text)) in entries.iter().enumerate().take(h.saturating_sub(2) as usize) {
        let y = panel.y + 1 + i as u16;
        let reachable = sign.is_none_or(|sign| keys.starts_with(sign));
        let (key_style, text_style) = if reachable {
            (base.fg(ACCENT).add_modifier(Modifier::BOLD), base)
        } else {
            (base.fg(DIM), base.fg(DIM))
        };
        put(buf, x0, y, right, keys, key_style);
        put(buf, x0 + key_w + 2, y, right, text, text_style);
    }
}

/// Panneau flottant des raccourcis, centré par-dessus la vue courante.
pub fn draw_help(buf: &mut Buffer, area: Rect, shortcuts: &[(&str, &str)]) {
    let key_w = shortcuts.iter().map(|(keys, _)| width(keys)).max().unwrap_or(0);
    let text_w = shortcuts
        .iter()
        .map(|(keys, text)| if keys.is_empty() { width(text) } else { key_w + 2 + width(text) })
        .max()
        .unwrap_or(0);
    let w = (text_w + 6).min(area.width);
    let h = (shortcuts.len() as u16 + 2).min(area.height);
    let panel = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    draw_panel(buf, panel, " Keyboard shortcuts ", " ? or Esc to close ");
    let base = Style::new().bg(PANEL_BG);
    let (left, right, top) = (panel.x, panel.right() - 1, panel.y);

    let x0 = left + 3;
    for (i, (keys, text)) in shortcuts.iter().enumerate().take(h.saturating_sub(2) as usize) {
        let y = top + 1 + i as u16;
        if keys.is_empty() {
            put(buf, x0, y, right, text, base.fg(Color::White).add_modifier(Modifier::BOLD));
        } else {
            put(buf, x0, y, right, keys, base.fg(ACCENT));
            put(buf, x0 + key_w + 2, y, right, text, base);
        }
    }
}

/// Message éphémère (copie, butée) : une pastille en bas à droite, par-dessus la vue.
pub fn draw_flash(buf: &mut Buffer, area: Rect, text: Option<&str>) {
    let Some(text) = text else { return };
    let room = area.width.saturating_sub(4) as usize;
    let text: String = text.chars().take(room).collect();
    let text = format!(" {text} ");
    let style = Style::new().fg(Color::LightGreen).bg(PANEL_BG).add_modifier(Modifier::BOLD);
    let (y, end) = (area.bottom() - 1, area.right() - 1);
    let x = end.saturating_sub(width(&text)).max(area.x);
    for cell_x in x..end {
        buf[(cell_x, y)].reset();
    }
    put(buf, x, y, end, &text, style);
}

/// Glyphe « code fork » de Font Awesome (Nerd Font), de la même famille que l'icône dossier.
const REPO: &str = "\u{f126}";
const REPO_PLAIN: &str = "◆";
const FOLDER: &str = "\u{f07b}";
const FOLDER_PLAIN: &str = "⌂";

/// Titre de l'en-tête d'un outil : le dossier puis le dépôt distant, chacun
/// derrière son icône. Sans dépôt distant, le dossier tient lieu de titre.
/// Renvoie la colonne suivante et la place de chacun, pour les clics.
pub fn draw_repo_title(buf: &mut Buffer, repo: &Repo, plain: bool, (x, y, end): (u16, u16, u16)) -> (u16, Rect, Option<Rect>) {
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let (repo_icon, folder_icon) = if plain { (REPO_PLAIN, FOLDER_PLAIN) } else { (REPO, FOLDER) };
    let folder_x = x;
    let x = put(buf, x, y, end, folder_icon, Style::new().fg(DIM));
    let x = put(buf, x + 1, y, end, &repo.name, bold);
    let folder = Rect::new(folder_x, y, x - folder_x, 1);
    let Some(remote) = &repo.remote_name else { return (x, folder, None) };
    let remote_x = x + 2;
    let x = put(buf, remote_x, y, end, repo_icon, Style::new().fg(ACCENT));
    let x = put(buf, x + 1, y, end, remote, bold);
    (x, folder, Some(Rect::new(remote_x, y, x.saturating_sub(remote_x), 1)))
}

/// Ce que dit le champ de recherche dans du code : l'occurrence où l'on se
/// trouve et leur nombre.
pub fn find_status(query: &str, current: Option<usize>, count: usize) -> String {
    match (current, count) {
        (_, 0) if query.is_empty() => String::new(),
        (_, 0) => "no match".to_string(),
        (Some(current), count) => format!("match {}/{}", current + 1, thousands(count)),
        (None, 1) => "1 match".to_string(),
        (None, count) => format!("{} matches", thousands(count)),
    }
}

/// Champ de recherche, sur une ligne : ce qui est cherché derrière son libellé,
/// un curseur tant que la saisie dure, et à droite ce qui a été trouvé.
pub fn draw_find_bar(buf: &mut Buffer, area: Rect, label: &str, query: &str, editing: bool, status: &str) {
    let base = Style::new().bg(PANEL_BG);
    for x in area.left()..area.right() {
        buf[(x, area.y)].reset();
    }
    buf.set_style(area, base);
    let end = area.right() - 1;
    let text_end = put_right(buf, area.x + 1, area.y, end, status, base.fg(DIM)).saturating_sub(2);
    let x = put(buf, area.x + 1, area.y, text_end, label, base.fg(ACCENT).add_modifier(Modifier::BOLD)) + 1;
    // Une saisie plus longue que le champ garde sa fin visible, curseur compris.
    let room = text_end.saturating_sub(x + 1) as usize;
    let length = query.chars().count();
    let shown: String = query.chars().skip(length.saturating_sub(room)).collect();
    let x = put(buf, x, area.y, text_end, &shown, base.fg(Color::White));
    if editing {
        put(buf, x, area.y, text_end, " ", Style::new().bg(Color::Gray));
    }
}
