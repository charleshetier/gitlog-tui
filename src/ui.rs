//! Rendu : seules les lignes visibles sont lues et dessinées, et chaque zone
//! cliquable est mémorisée pour traduire les clics en actions.

use std::ops::Range;
use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use crate::app::{App, CodeHit, DiffView, FilesState, Find, HierState, HitMap, NodeStyle, Prompt, Scope, Side};
use crate::diff::{DiffLine, FileEntry, LineKind, NO_LINE, inline_changes};
use crate::git::{Head, Label, RefKind};
use crate::graph::{COMMIT, Cell, DOWN, LEFT, RIGHT, UP};
use crate::hierarchy::{Remote, Sync};

/// Une teinte par branche, attribuée à l'ouverture de sa colonne.
const PALETTE: [Color; 8] = [
    Color::LightBlue,
    Color::LightRed,
    Color::LightGreen,
    Color::LightMagenta,
    Color::LightCyan,
    Color::Red,
    Color::Green,
    Color::Magenta,
];
const TAG_COLOR: Color = Color::Yellow;
const SEL_BG: Color = Color::Indexed(237);
/// Bande d'une colonne du graphe : elle relie les puces, qu'un trait ne peut pas traverser.
const LANE_BG: Color = Color::Indexed(236);
const LANE_SEL_BG: Color = Color::Indexed(240);
/// Code sélectionné à la souris dans un diff.
const SELECT_BG: Color = Color::Indexed(18);
/// Occurrences d'une recherche dans le code, en noir sur ces fonds : la
/// courante, puis les autres.
const FIND_CURRENT_BG: Color = Color::Indexed(208);
const FIND_BG: Color = Color::Indexed(179);
/// Ligne du commit de HEAD : un fond à peine relevé, en deçà de la sélection.
const HEAD_BG: Color = Color::Indexed(235);
const LANE_HEAD_BG: Color = Color::Indexed(238);
/// Encadré du libellé du commit, en tête du volet.
const CARD_BG: Color = Color::Indexed(236);
/// Fond du panneau flottant des raccourcis.
const PANEL_BG: Color = Color::Indexed(235);
/// Teinte d'accent : touches du panneau des raccourcis, marque de sélection.
const ACCENT: Color = Color::LightBlue;
const REMOTE_BG: Color = Color::Indexed(240);
const CHIP_BG: Color = Color::Indexed(238);
/// Fonds d'un diff : les lignes ajoutées et retirées et, plus soutenus, les
/// passages modifiés au sein d'une ligne.
#[derive(Clone, Copy)]
struct Tones {
    add: Color,
    del: Color,
    add_mark: Color,
    del_mark: Color,
}
const TONES: Tones = Tones {
    add: Color::Indexed(22),
    del: Color::Indexed(52),
    add_mark: Color::Indexed(28),
    del_mark: Color::Indexed(88),
};
/// Les mêmes fonds sous la coloration syntaxique : plus sombres, pour que chaque
/// teinte du thème y reste lisible. Aucune couleur de la palette n'est aussi sombre.
/// Un terminal qui ramène le 24 bits à 256 couleurs doit pourtant y retrouver du
/// vert et du rouge : trop sombre, un fond y devient un gris proche du noir.
const SYNTAX_TONES: Tones = Tones {
    add: Color::Rgb(0, 56, 0),
    del: Color::Rgb(63, 0, 1),
    add_mark: Color::Rgb(0, 88, 0),
    del_mark: Color::Rgb(128, 0, 8),
};
const DIM: Color = Color::DarkGray;
/// Fond du côté sans vis-à-vis dans le diff en deux colonnes.
const VOID_BG: Color = Color::Indexed(235);

const ARROW: &str = "\u{e0b2}";
const ARROW_PLAIN: &str = "◀";
/// Puce du commit de HEAD, à la place de « ● ».
const HEAD_NODE: &str = "◉";
const COPY: &str = "\u{f0c5}";
const COPY_PLAIN: &str = "⧉";
/// Glyphe « code fork » de Font Awesome (Nerd Font), de la même famille que l'icône dossier.
const REPO: &str = "\u{f126}";
const REPO_PLAIN: &str = "◆";
const FOLDER: &str = "\u{f07b}";
const FOLDER_PLAIN: &str = "⌂";

fn palette(index: u8) -> Color {
    PALETTE[index as usize % PALETTE.len()]
}

fn width(text: &str) -> u16 {
    Span::raw(text).width() as u16
}

/// Écrit `text` à partir de `x` sans dépasser `end` ; renvoie la colonne suivante.
fn put(buf: &mut Buffer, x: u16, y: u16, end: u16, text: &str, style: Style) -> u16 {
    if x >= end {
        return x;
    }
    buf.set_stringn(x, y, text, (end - x) as usize, style).0
}

/// Écrit `text` aligné à droite sur `end` ; renvoie sa colonne de départ.
fn put_right(buf: &mut Buffer, start: u16, y: u16, end: u16, text: &str, style: Style) -> u16 {
    let x = end.saturating_sub(width(text)).max(start);
    put(buf, x, y, end, text, style);
    x
}

/// Garde la fin d'un chemin trop long : c'est le nom du fichier qui compte.
fn tail(text: &str, max: u16) -> String {
    let count = text.chars().count();
    if width(text) <= max || max < 2 {
        return text.to_string();
    }
    let keep = (max as usize - 1).min(count);
    let kept: String = text.chars().skip(count - keep).collect();
    format!("…{kept}")
}

fn thousands(n: usize) -> String {
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

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Date civile (UTC) d'un horodatage Unix.
fn civil(t: i64) -> (i64, i64, i64) {
    let z = t.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Âge d'un commit : « 5 h » dans le graphe, « il y a 5 h » dans le volet.
fn relative(now: i64, t: i64, long: bool) -> String {
    if t == 0 {
        return String::new();
    }
    let age = now - t;
    let short = match age {
        0..60 => return "just now".to_string(),
        60..3600 => format!("{} min", age / 60),
        3600..86_400 => format!("{} h", age / 3600),
        86_400..172_800 => return "yesterday".to_string(),
        172_800..2_592_000 => format!("{} d", age / 86_400),
        _ => {
            let (y, m, d) = civil(t);
            return format!("{y:04}-{m:02}-{d:02}");
        }
    };
    if long { format!("{short} ago") } else { short }
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let buf = frame.buffer_mut();
    app.hit = HitMap::default();
    if area.width < 30 || area.height < 6 {
        put(buf, area.x, area.y, area.right(), "Window too small", Style::new());
        return;
    }

    let rule_top = area.y + 1;
    let body_y = area.y + 2;
    // Pas de pied de page : le corps descend jusqu'au bas de l'écran, sauf la ligne
    // du champ de recherche tant qu'une saisie ou une recherche est en cours.
    let searching = app.hierarchy.is_none()
        && (app.filter.is_some() || matches!(app.prompt, Some(Prompt::Input { .. })) || app.code_find().is_some());
    let body_h = area.bottom() - body_y - u16::from(searching);
    let body_bottom = body_y + body_h;
    let dim = Style::new().fg(DIM);
    let two_way = app
        .pane
        .as_ref()
        .and_then(|p| p.diff.as_ref())
        .is_some_and(|d| d.sides.is_some());
    // Colonne du séparateur vertical quand le volet est ouvert. Le diff en deux
    // colonnes prend les deux tiers de la largeur.
    let split_at = if two_way { (area.width / 3).max(24).min(area.width / 2) } else { area.width / 2 };
    let divider = (app.pane.is_some() && app.hierarchy.is_none()).then(|| area.x + split_at);

    for x in area.left()..area.right() {
        put(buf, x, rule_top, area.right(), "─", dim);
    }
    if let Some(x) = divider {
        for y in area.y..body_bottom {
            put(buf, x, y, area.right(), "│", dim);
        }
        put(buf, x, rule_top, area.right(), "┼", dim);
    }

    if app.hierarchy.is_some() {
        draw_hierarchy(buf, app, Rect::new(area.x, area.y, area.width, 1), Rect::new(area.x, body_y, area.width, body_h));
        draw_flash(buf, app, area);
        draw_branch_panels(buf, app, Rect::new(area.x, body_y, area.width, body_h));
        if app.help {
            draw_help(buf, area);
        }
        return;
    }

    let left_w = divider.map_or(area.width, |x| x - area.x);
    draw_header(buf, app, Rect::new(area.x, area.y, left_w, 1));
    draw_graph(buf, app, Rect::new(area.x, body_y, left_w, body_h));
    if let Some(x) = divider {
        let pane_w = area.right() - x - 1;
        draw_pane_header(buf, app, Rect::new(x + 1, area.y, pane_w, 1));
        let body = Rect::new(x + 1, body_y, pane_w, body_h);
        app.hit.pane = body;
        if app.depth() >= 3 {
            draw_diff(buf, app, body);
        } else {
            draw_files(buf, app, body);
        }
    }
    if searching {
        draw_search_bar(buf, app, Rect::new(area.x, body_bottom, area.width, 1));
    }
    if app.depth() == 4 {
        draw_full(buf, app, area);
    }
    draw_flash(buf, app, area);
    if let Some(Prompt::Kind { sign }) = app.prompt {
        draw_menu(buf, Rect::new(area.x, body_y, area.width, body_h), " Search ", SEARCH_KINDS, sign);
    }
    draw_branch_panels(buf, app, Rect::new(area.x, body_y, area.width, body_h));
    if app.help {
        draw_help(buf, area);
    }
}

/// Titre de l'en-tête : le dossier puis le dépôt distant, chacun derrière son icône.
/// Sans dépôt distant, le dossier tient lieu de titre. Renvoie la colonne suivante.
/// Un clic sur l'un ou l'autre copie le chemin du dossier ou l'URL du remote.
fn draw_title(buf: &mut Buffer, app: &mut App, x: u16, y: u16, end: u16) -> u16 {
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let (repo_icon, folder_icon) = if app.plain { (REPO_PLAIN, FOLDER_PLAIN) } else { (REPO, FOLDER) };
    let folder_x = x;
    let x = put(buf, x, y, end, folder_icon, Style::new().fg(DIM));
    let x = put(buf, x + 1, y, end, &app.repo.name, bold);
    app.hit.folder = Some(Rect::new(folder_x, y, x - folder_x, 1));
    let Some(remote) = &app.repo.remote_name else { return x };
    let remote_x = x + 2;
    let x = put(buf, remote_x, y, end, repo_icon, Style::new().fg(ACCENT));
    let x = put(buf, x + 1, y, end, remote, bold);
    app.hit.remote = Some(Rect::new(remote_x, y, x.saturating_sub(remote_x), 1));
    x
}

fn draw_header(buf: &mut Buffer, app: &mut App, area: Rect) {
    let end = area.right() - 1;
    let dim = Style::new().fg(DIM);
    let n = thousands(app.commit_count());
    let count = if app.log_error.is_some() {
        "git error".to_string()
    } else if app.log_done && app.commit_count() == 1 {
        "1 commit".to_string()
    } else if app.log_done {
        format!("{n} commits")
    } else if app.want_all {
        format!("loading… {n}")
    } else {
        format!("{n}+ commits")
    };
    let count_x = put_right(buf, area.x + 10, area.y, end, &count, dim);

    let text_end = count_x.saturating_sub(2);
    let x = draw_title(buf, app, area.x + 1, area.y, text_end);
    let head = match &app.repo.head {
        Head::Branch(name) => format!(" · HEAD on {name}"),
        Head::Detached(oid) => format!(" · detached HEAD at {}", oid.short()),
        Head::Unborn => String::new(),
    };
    let x = put(buf, x, area.y, text_end, &head, dim);
    // Le graphe ne part pas de toutes les branches : l'en-tête le rappelle.
    if let Some(note) = app.branch_note() {
        put(buf, x, area.y, text_end, &format!(" · {note}"), Style::new().fg(Color::Yellow));
    }
}

fn draw_graph(buf: &mut Buffer, app: &mut App, area: Rect) {
    app.graph_height = area.height as usize;
    app.hit.graph = area;
    let dim = Style::new().fg(DIM);
    let end = area.right() - 1;
    let len = app.store.len();
    if len == 0 {
        let (text, style) = match &app.log_error {
            Some(err) => (err.clone(), Style::new().fg(Color::Red)),
            None if app.log_done && app.filter.is_some() => ("No matching commits.".to_string(), dim),
            None if app.log_done && app.branch_note().is_some() => ("No commits on the branches shown.".to_string(), dim),
            None if app.log_done => ("No commits.".to_string(), dim),
            None if app.filter.is_some() => ("Searching…".to_string(), dim),
            None => ("Loading…".to_string(), dim),
        };
        put(buf, area.x + 1, area.y, end, &text, style);
        return;
    }
    app.top = app.top.min(len - 1);
    app.hit.graph_top = app.top;
    let last = (app.top + area.height as usize).min(len);

    let nodes = app.node_style;
    let graph_w = (app.top..last)
        .map(|row| {
            let cells = app.store.cells(row);
            // Le rail déborde d'une cellule à droite du commit.
            let extra = nodes != NodeStyle::Dots && cells.last().is_some_and(|c| c.bits & COMMIT != 0);
            cells.len() + usize::from(extra)
        })
        .max()
        .unwrap_or(0)
        .min(area.width as usize / 3) as u16;
    // Dans un volet étroit, le hash et l'auteur cèdent la place au message.
    let show_hash = area.width >= 60;
    let show_meta = area.width >= 90;
    let hash_x = area.x + 1 + graph_w + 1;
    let text_x = if show_hash { hash_x + 8 } else { hash_x };
    let now = now_secs();

    for row in app.top..last {
        let y = area.y + (row - app.top) as u16;
        let selected = row == app.selected;
        let oid = app.store.oid(row);
        // HEAD se repère de trois façons : le fond de sa ligne, sa puce, son étiquette.
        let head = app.labels.get(&oid).is_some_and(|labels| labels.iter().any(|label| label.is_head));
        let (row_bg, lane_bg) = match (selected, head) {
            (true, _) => (Some(SEL_BG), LANE_SEL_BG),
            (false, true) => (Some(HEAD_BG), LANE_HEAD_BG),
            (false, false) => (None, LANE_BG),
        };
        if let Some(bg) = row_bg {
            buf.set_style(Rect::new(area.x, y, area.width, 1), Style::new().bg(bg));
        }

        // Ce qui n'est pas encore commité reste en retrait : puce et nom en gris.
        let grey = if selected { Color::Gray } else { DIM };
        let tint = oid.is_worktree().then_some(grey);
        let cells = app.store.cells(row);
        draw_graph_row(buf, area.x + 1, y, end, cells, graph_w as usize, (nodes, head), (lane_bg, tint));

        let color = palette(app.store.color(row));
        let meta = app.meta(row);
        let bright = if selected { Style::new().fg(Color::White) } else { Style::new() };
        if oid.is_worktree() {
            // Ni hash, ni auteur : un nom en italique.
            put(buf, text_x, y, end, &meta.subject, Style::new().fg(grey).add_modifier(Modifier::ITALIC));
            continue;
        }
        if show_hash {
            let hash_style = if selected { Style::new().fg(Color::Gray) } else { dim };
            put(buf, hash_x, y, end, &oid.short(), hash_style);
        }

        let mut text_end = end;
        if show_meta {
            let author: String = meta.author.chars().take(20).collect();
            let info = format!("{author} · {}", relative(now, meta.time, false));
            let w = width(&info);
            if text_x + w + 24 < end {
                let style = if selected { Style::new().fg(Color::Gray) } else { dim };
                put(buf, end - w, y, end, &info, style);
                text_end = end - w - 2;
            }
        }

        let mut x = text_x;
        if let Some(labels) = app.labels.get(&oid) {
            x = draw_labels(buf, x, y, text_end, labels, color, app.plain, |label| app.shows(label));
        }
        put(buf, x, y, text_end, &meta.subject, bright);
    }
}

fn copy_icon(plain: bool) -> &'static str {
    if plain { COPY_PLAIN } else { COPY }
}

/// Étiquettes d'un commit avec leur texte, dans l'ordre d'affichage. Une branche
/// distante posée sur le même commit que la branche locale du même nom la suit,
/// réduite à « &origin » : son nom n'apprendrait rien de plus. Le booléen la
/// signale : elle s'accole alors au cartouche de sa branche locale. `shows`
/// écarte les branches que le graphe ne montre pas.
fn label_texts(labels: &[Label], shows: impl Fn(&Label) -> bool) -> Vec<(String, &Label, bool)> {
    let labels: Vec<&Label> = labels.iter().filter(|label| shows(label)).collect();
    // Le dépôt distant, si `remote` est la branche `local` chez lui.
    let twin = |local: &Label, remote: &Label| -> Option<String> {
        let (origin, branch) = remote.name.split_once('/')?;
        (remote.kind == RefKind::Remote && local.kind == RefKind::Local && branch == local.name)
            .then(|| format!("&{origin}"))
    };
    let mut out: Vec<(String, &Label, bool)> = Vec::new();
    for &label in &labels {
        match label.kind {
            RefKind::Local => {
                out.push((label.name.clone(), label, false));
                out.extend(labels.iter().filter_map(|&remote| Some((twin(label, remote)?, remote, true))));
            }
            RefKind::Remote => {
                if !labels.iter().any(|&local| twin(local, label).is_some()) {
                    out.push((label.name.clone(), label, false));
                }
            }
            RefKind::Tag => out.push((label.name.clone(), label, false)),
        }
    }
    out
}

/// Étiquettes en flèche pointant vers le commit, à gauche. Une branche distante
/// réduite à « &origin » prolonge le cartouche de sa branche locale : un simple
/// rectangle, sans flèche ni espace entre les deux. La branche courante
/// s'annonce comme dans `git log` : « HEAD → nom ».
#[allow(clippy::too_many_arguments)]
fn draw_labels(
    buf: &mut Buffer,
    mut x: u16,
    y: u16,
    end: u16,
    labels: &[Label],
    color: Color,
    plain: bool,
    shows: impl Fn(&Label) -> bool,
) -> u16 {
    let arrow = if plain { ARROW_PLAIN } else { ARROW };
    let labels = label_texts(labels, shows);
    for (i, (name, label, joined)) in labels.iter().enumerate() {
        let name: String = if name.chars().count() > 32 {
            name.chars().take(31).chain(['…']).collect()
        } else {
            name.clone()
        };
        // HEAD détaché porte déjà ce nom : git n'admet pas de branche « HEAD ».
        let name = if label.is_head && name != "HEAD" { format!("HEAD → {name}") } else { name };
        let remaining = labels.len() - i;
        // Place gardée pour le compteur « +N » des étiquettes non affichées.
        let reserve = if remaining > 1 { 4 } else { 0 };
        // Flèche (sauf accolée), marges du nom, espace qui suit.
        let chrome = if *joined { 3 } else { 4 };
        if x + width(&name) + chrome + reserve > end {
            // Le cartouche précédent attendait sa jumelle : l'espace lui revient.
            x += u16::from(*joined);
            x = put(buf, x, y, end, &format!("+{remaining}"), Style::new().fg(DIM)) + 1;
            break;
        }
        let (bg, fg) = match label.kind {
            RefKind::Local => (color, Color::Black),
            RefKind::Remote => (REMOTE_BG, color),
            RefKind::Tag => (TAG_COLOR, Color::Black),
        };
        let mut style = Style::new().fg(fg).bg(bg);
        if label.is_head {
            style = style.add_modifier(Modifier::BOLD);
        }
        if !joined {
            x = put(buf, x, y, end, arrow, Style::new().fg(bg));
        }
        x = put(buf, x, y, end, &format!(" {name} "), style);
        // Pas d'espace avant une étiquette accolée.
        if !labels.get(i + 1).is_some_and(|next| next.2) {
            x += 1;
        }
    }
    x
}

fn draw_pane_header(buf: &mut Buffer, app: &mut App, area: Rect) {
    let depth = app.depth();
    let end = area.right() - 1;

    // Index de profondeur : seuls les niveaux atteints, le courant surligné.
    let chips_w = u16::from(depth) * 4 - 1;
    let chips_x = end.saturating_sub(chips_w).max(area.x);
    for level in 1..=depth {
        let x = chips_x + u16::from(level - 1) * 4;
        let style = if level == depth {
            Style::new().fg(Color::Black).bg(Color::LightBlue).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(Color::Gray).bg(CHIP_BG)
        };
        put(buf, x, area.y, end, &format!(" {level} "), style);
        app.hit.chips.push((Rect::new(x, area.y, 3, 1), level));
    }

    let Some(pane) = &app.pane else { return };
    let title_end = chips_x.saturating_sub(2);
    let bold = Style::new().add_modifier(Modifier::BOLD);
    match (&pane.diff, &pane.files) {
        (Some(diff), FilesState::Ready(files)) => {
            let path = files.get(diff.file).map_or("", |f| f.path.as_str());
            let room = title_end.saturating_sub(area.x + 1);
            put(buf, area.x + 1, area.y, title_end, &tail(path, room), bold);
        }
        _ if pane.oid.is_worktree() => {
            put(buf, area.x + 1, area.y, title_end, "Uncommitted changes", bold);
        }
        _ => {
            let x = put(buf, area.x + 1, area.y, title_end, &format!("Commit {}", pane.oid.short()), bold);
            if x + 2 < title_end {
                put(buf, x + 1, area.y, title_end, copy_icon(app.plain), Style::new());
                app.hit.pane_copy = Some(Rect::new(x, area.y, 3, 1));
            }
        }
    }
}

/// Niveau 2 : le commit et ses fichiers modifiés.
fn draw_files(buf: &mut Buffer, app: &mut App, area: Rect) {
    let plain_minus = "−";
    let dim = Style::new().fg(DIM);
    let green = Style::new().fg(Color::Green);
    let red = Style::new().fg(Color::Red);
    let x0 = area.x + 1;
    let end = area.right() - 1;
    let list_y = area.y + 3;
    let list_h = area.height.saturating_sub(3);
    app.pane_height = list_h as usize;
    let Some(pane) = &mut app.pane else { return };

    // Encadré : le libellé du commit et son auteur sur un fond légèrement tramé.
    buf.set_style(Rect::new(area.x, area.y, area.width, area.height.min(2)), Style::new().bg(CARD_BG));
    put(buf, x0, area.y, end, &pane.meta.subject, Style::new().add_modifier(Modifier::BOLD));
    let info = if pane.oid.is_worktree() {
        "working tree and index, compared to HEAD".to_string()
    } else {
        format!("{} · {}", pane.meta.author, relative(now_secs(), pane.meta.time, true))
    };
    put(buf, x0, area.y + 1, end, &info, Style::new().fg(Color::Gray));

    let files: &[FileEntry] = match &pane.files {
        FilesState::Loading => {
            put(buf, x0, area.y + 2, end, "Loading files…", dim);
            return;
        }
        FilesState::Failed(err) => {
            put(buf, x0, area.y + 2, end, err, red);
            return;
        }
        FilesState::Ready(files) => files,
    };
    let added: u32 = files.iter().filter_map(|f| f.added).sum();
    let deleted: u32 = files.iter().filter_map(|f| f.deleted).sum();
    let plural = if files.len() > 1 { "s" } else { "" };
    // Une recherche a pu écarter des fichiers : le décompte le dit.
    let summary = match pane.hidden_files {
        0 => format!("{} file{plural} changed", files.len()),
        hidden => format!("{} of {} files match the search", files.len(), files.len() + hidden),
    };
    put(buf, x0, area.y + 2, end, &summary, dim);
    put_right(buf, x0, area.y + 2, end, &format!("{plain_minus}{deleted}"), red);
    put_right(buf, x0, area.y + 2, end.saturating_sub(8), &format!("+{added}"), green);

    if list_h == 0 {
        return;
    }
    // La sélection reste dans la fenêtre de la liste.
    pane.file_sel = pane.file_sel.min(files.len().saturating_sub(1));
    if pane.file_sel < pane.file_top {
        pane.file_top = pane.file_sel;
    } else if pane.file_sel >= pane.file_top + list_h as usize {
        pane.file_top = pane.file_sel + 1 - list_h as usize;
    }
    app.hit.files = Rect::new(area.x, list_y, area.width, list_h);
    app.hit.file_top = pane.file_top;

    for (i, file) in files.iter().enumerate().skip(pane.file_top).take(list_h as usize) {
        let y = list_y + (i - pane.file_top) as u16;
        if i == pane.file_sel {
            buf.set_style(Rect::new(area.x, y, area.width, 1), Style::new().bg(SEL_BG));
            put(buf, area.x, y, end, "▌", Style::new().fg(ACCENT));
        }
        let status_color = match file.status {
            'A' => Color::Green,
            'D' => Color::Red,
            'R' | 'C' => Color::Cyan,
            _ => Color::Yellow,
        };
        let status = Style::new().fg(status_color).add_modifier(Modifier::BOLD);
        let mut status_text = [0u8; 4];
        put(buf, x0, y, end, file.status.encode_utf8(&mut status_text), status);

        let stats_x = match (file.added, file.deleted) {
            (Some(a), Some(d)) => {
                put_right(buf, x0, y, end, &format!("{plain_minus}{d}"), red);
                put_right(buf, x0, y, end.saturating_sub(8), &format!("+{a}"), green)
            }
            _ => put_right(buf, x0, y, end, "binary", dim),
        };
        let path_end = stats_x.saturating_sub(2);
        let room = path_end.saturating_sub(x0 + 2);
        // Les dossiers s'effacent derrière le nom du fichier.
        let shown = tail(&file.path, room);
        let name_at = shown.rfind('/').map_or(0, |slash| slash + 1);
        let selected = i == pane.file_sel;
        let folders = if selected { Style::new().fg(Color::Gray) } else { dim };
        let name = if selected { Style::new().fg(Color::White) } else { Style::new() };
        let x = put(buf, x0 + 2, y, path_end, &shown[..name_at], folders);
        put(buf, x, y, path_end, &shown[name_at..], name);
    }
}

/// Niveau 3 : diff unifié du fichier, avec numéros de ligne.
fn draw_diff(buf: &mut Buffer, app: &mut App, area: Rect) {
    app.pane_height = area.height as usize;
    let syntax = app.syntax;
    let diff = app.pane.as_mut().and_then(|p| p.diff.as_mut());
    app.hit.code = diff.and_then(|diff| draw_diff_view(buf, diff, area, syntax));
}

/// Niveau 4 : le fichier entier dans une fenêtre flottante, modifications en place.
fn draw_full(buf: &mut Buffer, app: &mut App, area: Rect) {
    let panel = Rect::new(area.x + 2, area.y + 1, area.width.saturating_sub(4), area.height.saturating_sub(2));
    if panel.width < 12 || panel.height < 3 {
        return;
    }
    let inner = Rect::new(panel.x + 1, panel.y + 1, panel.width - 2, panel.height - 2);
    app.pane_height = inner.height as usize;
    let syntax = app.syntax;
    let Some(pane) = &mut app.pane else { return };
    let Some(full) = &mut pane.full else { return };
    let path = match &pane.files {
        FilesState::Ready(files) => files.get(full.file).map_or("", |f| f.path.as_str()),
        _ => "",
    };
    // Position parmi les modifications : celles déjà atteintes ou dépassées.
    let total = full.change_tops().count();
    let reached = full.change_tops().take_while(|&top| top <= full.top).count();
    let count = match (reached, total) {
        (_, 0) if !full.done => "loading…".to_string(),
        (_, 0) => "no change".to_string(),
        (0, 1) => "1 change".to_string(),
        (0, n) => format!("{n} changes"),
        (i, n) => format!("change {i}/{n}"),
    };
    let room = panel.width.saturating_sub(width(&count) + 9);
    let title = format!(" {} · {count} ", tail(path, room));
    // Pendant une recherche dans le code, ces touches lui reviennent.
    let keys = if full.find.is_some() { " n N matches · Esc to clear the search " } else { " n N changes · Esc to close " };
    draw_panel(buf, panel, &title, keys);
    // L'intérieur reprend le fond du terminal : les couleurs du diff y restent celles du volet.
    for y in inner.top()..inner.bottom() {
        for x in inner.left()..inner.right() {
            buf[(x, y)].reset();
        }
    }
    // La fenêtre recouvre le diff : c'est dans son code que la souris sélectionne.
    app.hit.code = draw_diff_view(buf, full, inner, syntax);
}

/// Lignes d'un diff à partir de sa position de défilement : en deux colonnes quand
/// les deux versions du fichier existent, en une seule sinon. Renvoie la place
/// du code à l'écran, `None` tant qu'il n'y a rien à lire.
fn draw_diff_view(buf: &mut Buffer, diff: &mut DiffView, area: Rect, syntax: bool) -> Option<CodeHit> {
    let dim = Style::new().fg(DIM);
    let tones = if syntax { SYNTAX_TONES } else { TONES };
    let x0 = area.x + 1;
    let end = area.right() - 1;
    if diff.lines.is_empty() {
        let text = if diff.done { "No differences to show." } else { "Loading diff…" };
        put(buf, x0, area.y, end, text, dim);
        return None;
    }
    if let Some(sides) = &diff.sides {
        diff.top = diff.top.min(sides.rows.len().saturating_sub(1));
        return Some(draw_sides(buf, area, diff, &sides.rows[diff.top..], tones));
    }
    diff.top = diff.top.min(diff.lines.len() - 1);
    let visible: &[DiffLine] = &diff.lines[diff.top..(diff.top + area.height as usize).min(diff.lines.len())];
    let max_no = visible.iter().map(|l| l.old.max(l.new)).max().unwrap_or(0);
    let num_w = max_no.to_string().len().max(3);
    let number = |n: u32| if n == 0 { " ".repeat(num_w) } else { format!("{n:>num_w$}") };
    // Un fichier ajouté n'a que des lignes vertes : le « + » n'y apprendrait rien.
    let signs = visible.iter().any(|line| matches!(line.kind, LineKind::Del));
    // Numéros, signe éventuel, puis le code.
    let code_x = x0 + 2 * num_w as u16 + 2 + if signs { 2 } else { 0 };

    for (i, line) in visible.iter().enumerate() {
        let y = area.y + i as u16;
        match line.kind {
            LineKind::Hunk => {
                put(buf, x0, y, end, &line.text, Style::new().fg(Color::Cyan));
                continue;
            }
            LineKind::Meta => {
                put(buf, x0, y, end, &line.text, dim.add_modifier(Modifier::ITALIC));
                continue;
            }
            LineKind::Add => buf.set_style(Rect::new(area.x, y, area.width, 1), Style::new().bg(tones.add)),
            LineKind::Del => buf.set_style(Rect::new(area.x, y, area.width, 1), Style::new().bg(tones.del)),
            LineKind::Ctx => {}
        }
        let (sign, sign_color) = match line.kind {
            LineKind::Add => ("+", Color::LightGreen),
            LineKind::Del => ("−", Color::LightRed),
            _ => (" ", DIM),
        };
        let numbers = format!("{} {}", number(line.old), number(line.new));
        // Les numéros de ligne sont estompés : ils s'effacent derrière le contenu.
        let mut x = put(buf, x0, y, end, &numbers, dim);
        if signs {
            x = put(buf, x + 1, y, end, sign, Style::new().fg(sign_color).add_modifier(Modifier::BOLD));
        }
        put_code(buf, x + 1, y, end, line, diff.x, (&[], Color::Reset));
        mark_finds(buf, (code_x, y, end), diff.x, diff.find.as_ref(), (diff.top + i, Side::Left));
        if let Some(columns) = diff.select.and_then(|select| select.columns(Side::Left, diff.top + i)) {
            mark_selection(buf, (code_x, y, end), diff.x, line, columns);
        }
    }
    Some(CodeHit { area, top: diff.top, skip: diff.x, left: code_x, right: None, lower_edge: lower_edge(buf, area) })
}

/// Ligne de l'écran où une sélection commence à faire défiler le diff vers le
/// bas : celle qui suit le diff, ou sa dernière s'il touche le bas de l'écran.
fn lower_edge(buf: &Buffer, area: Rect) -> u16 {
    if area.bottom() >= buf.area.bottom() { area.bottom().saturating_sub(1) } else { area.bottom() }
}

/// Applique un style à un passage d'une ligne de code dessinée en `x` : `from`
/// et `to` sont des rangs de caractères dans le texte, `skip` le défilement.
fn mark_span(buf: &mut Buffer, (x, y, end): (u16, u16, u16), skip: usize, (from, to): (usize, usize), style: Style) {
    let room = end.saturating_sub(x) as usize;
    let (start, stop) = (from.saturating_sub(skip).min(room), to.saturating_sub(skip).min(room));
    if start < stop {
        buf.set_style(Rect::new(x + start as u16, y, (stop - start) as u16, 1), style);
    }
}

/// Fond de la sélection à la souris sur une ligne de code.
fn mark_selection(buf: &mut Buffer, at: (u16, u16, u16), skip: usize, line: &DiffLine, (from, to): (usize, usize)) {
    let len = line.text.chars().count();
    // Une ligne prise jusqu'au bout déborde d'une cellule : son saut de ligne.
    let to = if to == usize::MAX { len + 1 } else { to.min(len) };
    mark_span(buf, at, skip, (from, to), Style::new().bg(SELECT_BG));
}

/// Occurrences de la recherche sur une ligne de la vue, d'un côté : en noir sur
/// fond vif, l'occurrence courante d'une autre teinte.
fn mark_finds(buf: &mut Buffer, at: (u16, u16, u16), skip: usize, find: Option<&Find>, (row, side): (usize, Side)) {
    let Some(find) = find else { return };
    for (start, len, current) in find.on(row, side) {
        let bg = if current { FIND_CURRENT_BG } else { FIND_BG };
        mark_span(buf, at, skip, (start, start + len), Style::new().fg(Color::Black).bg(bg));
    }
}

/// Écrit le texte d'une ligne de diff à partir du caractère `skip`. Le texte est
/// découpé là où change la teinte syntaxique ou le fond : `marks` désigne, en
/// rangs de caractères, les passages modifiés, qui reçoivent le fond `mark_bg`.
fn put_code(
    buf: &mut Buffer,
    mut x: u16,
    y: u16,
    end: u16,
    line: &DiffLine,
    skip: usize,
    (marks, mark_bg): (&[Range<usize>], Color),
) {
    let text = line.text.as_str();
    // Octet de chaque caractère, puis la fin du texte.
    let offsets: Vec<usize> = text.char_indices().map(|(at, _)| at).chain([text.len()]).collect();
    let count = offsets.len() - 1;
    let mut cuts = vec![skip.min(count), count];
    let mut tint_end = 0;
    for &(len, _) in &line.tints {
        tint_end += len as usize;
        cuts.push(tint_end.min(count));
    }
    cuts.extend(marks.iter().flat_map(|mark| [mark.start.min(count), mark.end.min(count)]));
    cuts.sort_unstable();
    cuts.dedup();

    let mut tints = line.tints.iter();
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

/// Dessine les cellules du graphe d'une ligne selon le style des commits ;
/// `head` y change la puce. Chaque colonne active reçoit un fond `lane_bg`, large
/// d'une cellule. `tint` remplace la teinte des branches sur toute la ligne.
#[allow(clippy::too_many_arguments)]
fn draw_graph_row(
    buf: &mut Buffer,
    x0: u16,
    y: u16,
    end: u16,
    cells: &[Cell],
    width: usize,
    (nodes, head): (NodeStyle, bool),
    (lane_bg, tint): (Color, Option<Color>),
) {
    let at = |i: usize| cells.get(i).copied().unwrap_or_default();
    for i in 0..width {
        let cell = at(i);
        let before = if i > 0 { at(i - 1) } else { Cell::default() };
        let is_commit = cell.bits & COMMIT != 0;
        let after_commit = before.bits & COMMIT != 0;
        let lane = cell.bits & (UP | DOWN | COMMIT) != 0;
        let mut glyph = [0u8; 4];
        let (text, color, bold): (&str, u8, bool) = match nodes {
            NodeStyle::Rail if is_commit => {
                // La ligne traverse la cellule et tend un bras vers la puce.
                let mut bits = cell.bits & (UP | DOWN) | RIGHT;
                if before.bits & RIGHT != 0 {
                    bits |= LEFT;
                }
                let rail = Cell { bits, color: cell.color };
                (rail.glyph().encode_utf8(&mut glyph), cell.color, false)
            }
            NodeStyle::Rail if after_commit => (if head { HEAD_NODE } else { "●" }, before.color, true),
            _ if cell.bits == 0 => continue,
            _ if is_commit && head => (HEAD_NODE, cell.color, true),
            _ => (cell.glyph().encode_utf8(&mut glyph), cell.color, is_commit),
        };
        let mut style = Style::new().fg(tint.unwrap_or(palette(color)));
        if bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        if lane {
            style = style.bg(lane_bg);
        }
        put(buf, x0 + i as u16, y, end, text, style);
    }
}

/// Diff en deux colonnes : l'ancienne version à gauche, la nouvelle à droite.
fn draw_sides(buf: &mut Buffer, area: Rect, diff: &DiffView, rows: &[(u32, u32)], tones: Tones) -> CodeHit {
    let (lines, skip) = (diff.lines.as_slice(), diff.x);
    let dim = Style::new().fg(DIM);
    let x0 = area.x + 1;
    let end = area.right() - 1;
    let mid = x0 + (end - x0) / 2;
    let rows = &rows[..rows.len().min(area.height as usize)];
    let line = |index: u32| (index != NO_LINE).then(|| &lines[index as usize]);
    let max_no = rows
        .iter()
        .map(|&(l, r)| line(l).map_or(0, |d| d.old).max(line(r).map_or(0, |d| d.new)))
        .max()
        .unwrap_or(0);
    let num_w = max_no.to_string().len().max(3);

    // `changed` : fond d'une ligne modifiée, puis ses passages modifiés et leur fond.
    type Changed<'a> = Option<(Color, &'a [Range<usize>], Color)>;
    // `at` : ligne de la vue et côté, pour y poser la recherche et la sélection.
    let side = |buf: &mut Buffer, y: u16, bg_area: Rect, x: u16, end: u16, content: Option<(&DiffLine, u32)>, changed: Changed, at: (usize, Side)| {
        let Some((line, number)) = content else {
            buf.set_style(bg_area, Style::new().bg(VOID_BG));
            return;
        };
        let (bg, marks, mark_bg) = changed.unwrap_or((Color::Reset, &[], Color::Reset));
        if changed.is_some() {
            buf.set_style(bg_area, Style::new().bg(bg));
        }
        let x = put(buf, x, y, end, &format!("{number:>num_w$}"), dim);
        put_code(buf, x + 1, y, end, line, skip, (marks, mark_bg));
        mark_finds(buf, (x + 1, y, end), skip, diff.find.as_ref(), at);
        if let Some(columns) = diff.select.and_then(|select| select.columns(at.1, at.0)) {
            mark_selection(buf, (x + 1, y, end), skip, line, columns);
        }
    };

    for (i, &(l, r)) in rows.iter().enumerate() {
        let y = area.y + i as u16;
        let (left, right) = (line(l), line(r));
        if l == r
            && let Some(same) = left
        {
            match same.kind {
                LineKind::Hunk => {
                    put(buf, x0, y, end, &same.text, Style::new().fg(Color::Cyan));
                    continue;
                }
                LineKind::Meta => {
                    put(buf, x0, y, end, &same.text, dim.add_modifier(Modifier::ITALIC));
                    continue;
                }
                _ => {}
            }
        }
        let changed = l != r;
        let left_area = Rect::new(area.x, y, mid - area.x, 1);
        let right_area = Rect::new(mid + 1, y, area.right() - mid - 1, 1);
        // Une ligne retirée face à celle qui la remplace : ce qui change entre les deux ressort.
        let (old_marks, new_marks) = match (left, right) {
            (Some(old), Some(new)) if changed => inline_changes(&old.text, &new.text),
            _ => Default::default(),
        };
        let removed = changed.then_some((tones.del, old_marks.as_slice(), tones.del_mark));
        let added = changed.then_some((tones.add, new_marks.as_slice(), tones.add_mark));
        let row = diff.top + i;
        side(buf, y, left_area, x0, mid, left.map(|d| (d, d.old)), removed, (row, Side::Left));
        side(buf, y, right_area, mid + 2, end, right.map(|d| (d, d.new)), added, (row, Side::Right));
        put(buf, mid, y, end, "│", dim);
    }
    // De chaque côté : le numéro de ligne, une espace, puis le code.
    let code = num_w as u16 + 1;
    let right = Some((mid, mid + 2 + code));
    CodeHit { area, top: diff.top, skip, left: x0 + code, right, lower_edge: lower_edge(buf, area) }
}

/// Hiérarchie des branches : une ligne par branche, sans les commits.
fn draw_hierarchy(buf: &mut Buffer, app: &mut App, header: Rect, body: Rect) {
    let dim = Style::new().fg(DIM);
    let end = body.right() - 1;
    let plain = app.plain;
    app.pane_height = body.height as usize;

    let x = draw_title(buf, app, header.x + 1, header.y, end);
    put(buf, x, header.y, end, " · branch hierarchy", dim);

    let Some(view) = &mut app.hierarchy else { return };
    let (rows, hidden) = match &view.state {
        HierState::Loading => {
            put(buf, body.x + 1, body.y, end, "Computing the hierarchy…", dim);
            return;
        }
        HierState::Failed(err) => {
            put(buf, body.x + 1, body.y, end, err, Style::new().fg(Color::Red));
            return;
        }
        HierState::Ready { rows, hidden } => (rows, *hidden),
    };
    let mut count = format!("{} branches", rows.len());
    if hidden > 0 {
        count.push_str(&format!(" · {hidden} older ones hidden"));
    }
    put_right(buf, header.x + 10, header.y, end, &count, dim);
    if rows.is_empty() {
        put(buf, body.x + 1, body.y, end, "No local branches.", dim);
        return;
    }

    let height = body.height as usize;
    view.sel = view.sel.min(rows.len() - 1);
    if view.sel < view.top {
        view.top = view.sel;
    } else if view.sel >= view.top + height {
        view.top = view.sel + 1 - height;
    }
    app.hit.branches = body;
    app.hit.branch_top = view.top;

    for (i, row) in rows.iter().enumerate().skip(view.top).take(height) {
        let y = body.y + (i - view.top) as u16;
        if i == view.sel {
            buf.set_style(Rect::new(body.x, y, body.width, 1), Style::new().bg(SEL_BG));
        }
        // La couleur dit l'état par rapport au parent, comme dans git machete.
        let color = match row.sync {
            Sync::Root => Color::LightBlue,
            Sync::UpToDate => Color::Green,
            Sync::Stale => Color::LightRed,
            Sync::Merged => Color::Gray,
        };
        let mut x = body.x + 2;
        for guide in &row.guides {
            x = put(buf, x, y, end, if *guide { "│  " } else { "   " }, dim);
        }
        if row.depth > 0 {
            x = put(buf, x, y, end, if row.last { "╰─ " } else { "├─ " }, Style::new().fg(color));
        }

        let (bg, fg) = if row.sync == Sync::Merged { (REMOTE_BG, Color::Gray) } else { (color, Color::Black) };
        let mut label = Style::new().fg(fg).bg(bg);
        if row.branch.is_head {
            label = label.add_modifier(Modifier::BOLD);
        }
        x = put(buf, x, y, end, if plain { ARROW_PLAIN } else { ARROW }, Style::new().fg(bg));
        x = put(buf, x, y, end, &format!(" {} ", row.branch.name), label);
        if row.branch.is_head {
            x = put(buf, x + 1, y, end, "← HEAD", Style::new().add_modifier(Modifier::BOLD));
        }

        let state = match (row.sync, row.ahead, row.behind) {
            (Sync::Root, ..) => String::new(),
            (Sync::Merged, ..) => "merged".to_string(),
            (Sync::UpToDate, 0, _) => "same commit".to_string(),
            (Sync::UpToDate, ahead, _) => format!("↑{ahead}"),
            (Sync::Stale, ahead, behind) => format!("↑{ahead} ↓{behind} · needs rebase"),
        };
        if !state.is_empty() {
            x = put(buf, x + 1, y, end, &state, Style::new().fg(color)) + 1;
        }
        let remote = match row.branch.remote {
            Remote::InSync => "",
            Remote::Untracked => "(no upstream)",
            Remote::Ahead => "(to push)",
            Remote::Behind => "(behind remote)",
            Remote::Diverged => "(diverged from remote)",
            Remote::Gone => "(remote gone)",
        };
        put(buf, x + 1, y, end, remote, dim);
    }
}

/// Raccourcis du panneau flottant : (touches, effet). Sans touche, c'est un titre de section.
const SHORTCUTS: &[(&str, &str)] = &[
    ("", "Navigation"),
    ("↑ ↓  j k", "Move the selection, scroll a diff"),
    ("PgUp PgDn", "Move by one page"),
    ("g G  Home End", "Jump to the first / last item"),
    ("Enter", "Open the commit, the file diff, then the whole file"),
    ("n N", "Next / previous commit, file or change, by level"),
    ("/", "Search the code of a diff; then n N for the matches"),
    ("← →", "Scroll a diff sideways"),
    ("1 2 3 4", "Go to a level: graph, files, diff, whole file"),
    ("q", "Go back one level, quit from the graph"),
    ("Esc", "Go back one level, leave a search"),
    ("", ""),
    ("", "Actions"),
    ("y", "Copy the commit hash, or the file path when on a file"),
    ("b h · b f", "Branch hierarchy · pick the branches of the graph"),
    ("b o · b O", "Hide the remote branches (origin) · show only them"),
    ("s", "Search: c hash, m message, f file, h +h -h hunk"),
    ("S", "Commit style: dots or rail"),
    ("R", "Refresh: reload the commits, branches and tags"),
    ("?", "Show or hide this panel"),
    ("Ctrl+C", "Quit"),
    ("", ""),
    ("", "Mouse"),
    ("Click", "Open a commit or a file"),
    ("Click copy icon", "Copy the hash of the open commit"),
    ("Click header", "On the folder: copy its path; on the repo: its URL"),
    ("Wheel", "Scroll the graph or the pane"),
    ("Drag on a diff", "Select code: it is copied when the button is released"),
];

/// Cadre d'un panneau flottant : fond uni, titre en haut, rappel de fermeture en bas.
fn draw_panel(buf: &mut Buffer, panel: Rect, title: &str, close: &str) {
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

/// Types de recherche du sous-menu : (touches, ce qui est examiné).
const SEARCH_KINDS: &[(&str, &str)] = &[
    ("c", "commit hash"),
    ("m", "commit message"),
    ("f", "file name"),
    ("h", "hunk: any changed line"),
    ("+h", "hunk: added lines"),
    ("-h", "hunk: removed lines"),
];

/// Commandes du sous-menu des branches : (touche, effet).
const BRANCH_KEYS: &[(&str, &str)] = &[
    ("h", "hierarchy of the branches"),
    ("f", "pick the branches of the graph"),
    ("o", "show or hide the remote branches"),
    ("O", "remote branches only, or all"),
];

/// Sous-menu d'une touche à deux temps, posé en bas à gauche : celui de la
/// recherche, au-dessus du champ de saisie, ou celui des branches. Après un `+`
/// ou un `-`, seule la ligne qui peut encore suivre reste en évidence.
fn draw_menu(buf: &mut Buffer, area: Rect, title: &str, entries: &[(&str, &str)], sign: Option<char>) {
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

/// Panneaux des branches, par-dessus le graphe ou la hiérarchie : le sous-menu
/// de `b`, puis la liste des branches à cocher.
fn draw_branch_panels(buf: &mut Buffer, app: &mut App, area: Rect) {
    if matches!(app.prompt, Some(Prompt::Branches)) {
        draw_menu(buf, area, " Branches ", BRANCH_KEYS, None);
    }
    draw_picker(buf, app, area);
}

/// Liste des branches à cocher pour le graphe, au centre de l'écran. La recherche
/// n'y filtre rien : elle surligne les branches trouvées, la ligne du bas la rappelle.
fn draw_picker(buf: &mut Buffer, app: &mut App, area: Rect) {
    let scope = app.scope;
    let Some(picker) = &mut app.picker else { return };
    let name_w = picker.items.iter().map(|(_, name)| width(name)).max().unwrap_or(0);
    let w = (name_w + 12).clamp(58, 96).min(area.width.saturating_sub(2));
    // Le cadre, la liste, puis une ligne pour les touches ou la recherche.
    let h = (picker.items.len() as u16 + 3).min(area.height.saturating_sub(2));
    if w < 20 || h < 4 {
        return;
    }
    let panel = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    let checked = picker.checked.iter().filter(|&&checked| checked).count();
    let title = format!(" Branches in the graph · {checked}/{} ", picker.items.len());
    draw_panel(buf, panel, &title, " Enter to apply · Esc to cancel ");

    // Le cadre laisse sa couleur aux cellules : le texte reprend celle du terminal.
    let base = Style::new().bg(PANEL_BG).fg(Color::Reset);
    let (x0, right) = (panel.x + 2, panel.right() - 1);
    let rows = (h - 3) as usize;
    picker.height = rows;
    // Le curseur reste dans la partie visible de la liste.
    picker.top = picker.top.min(picker.sel).max((picker.sel + 1).saturating_sub(rows));
    for (i, index) in (picker.top..picker.items.len()).take(rows).enumerate() {
        let y = panel.y + 1 + i as u16;
        let (kind, name) = &picker.items[index];
        let current = index == picker.sel;
        let row = if current { base.bg(SEL_BG) } else { base };
        buf.set_style(Rect::new(panel.x + 1, y, w - 2, 1), row);
        let (mark, mark_style) = match picker.checked[index] {
            true => ("[x]", row.fg(ACCENT).add_modifier(Modifier::BOLD)),
            false => ("[ ]", row.fg(DIM)),
        };
        let x = put(buf, x0, y, right, mark, mark_style);
        // En retrait : une branche que la nature choisie (`b` `o`, `b` `O`) écarte du graphe.
        let hidden = matches!((scope, kind), (Scope::Local, RefKind::Remote) | (Scope::Remote, RefKind::Local));
        let name_style = if hidden || !picker.checked[index] { row.fg(DIM) } else { row };
        put(buf, x + 1, y, right, name, name_style);
        if picker.matches(index) {
            let exact = picker.query.chars().any(char::is_uppercase);
            let folded = if exact { name.clone() } else { name.to_lowercase() };
            if let Some(byte) = folded.find(&picker.query) {
                let (from, len) = (folded[..byte].chars().count(), picker.query.chars().count());
                let bg = if current { FIND_CURRENT_BG } else { FIND_BG };
                mark_span(buf, (x + 1, y, right), 0, (from, from + len), Style::new().fg(Color::Black).bg(bg));
            }
        }
    }

    let y = panel.bottom() - 2;
    let found = (0..picker.items.len()).filter(|&index| picker.matches(index)).count();
    if picker.typing || !picker.query.is_empty() {
        let x = put(buf, x0, y, right, "find:", base.fg(ACCENT).add_modifier(Modifier::BOLD));
        let x = put(buf, x + 1, y, right, &picker.query, base);
        if picker.typing {
            put(buf, x, y, right, "▏", base.fg(ACCENT));
        }
        let status = match found {
            0 => "no match".to_string(),
            1 => "1 match".to_string(),
            found => format!("{found} matches · n N"),
        };
        put_right(buf, x + 2, y, right.saturating_sub(1), &status, base.fg(DIM));
    } else {
        put(buf, x0, y, right, "Space check · a all or none · / search", base.fg(DIM));
    }
}

/// Champ de recherche, tout en bas : la saisie en cours, sinon la recherche appliquée
/// et le nombre de commits trouvés.
fn draw_search_bar(buf: &mut Buffer, app: &App, area: Rect) {
    // La recherche dans le code du diff ouvert passe devant celle des commits.
    let (label, query, editing, status) = if let Some((query, editing, current, count)) = app.code_find() {
        let status = match (current, count) {
            (_, 0) if query.is_empty() => String::new(),
            (_, 0) => "no match".to_string(),
            (Some(current), count) => format!("match {}/{}", current + 1, thousands(count)),
            (None, 1) => "1 match".to_string(),
            (None, count) => format!("{} matches", thousands(count)),
        };
        ("find:".to_string(), query, editing, status)
    } else {
        let (kind, query, editing) = match (&app.prompt, &app.filter) {
            (Some(Prompt::Input { kind, query }), _) => (*kind, query.as_str(), true),
            (_, Some(filter)) => (filter.kind, filter.query.as_str(), false),
            _ => return,
        };
        // À droite : le nombre de commits trouvés, quand la liste répond bien au texte affiché.
        let applied = app.filter.as_ref().is_some_and(|f| f.kind == kind && f.query == query.trim());
        let status = if applied {
            match app.store.len() {
                _ if app.log_error.is_some() => "git error".to_string(),
                _ if app.stale => "searching…".to_string(),
                n if !app.log_done => format!("searching… {}", thousands(n)),
                0 => "no match".to_string(),
                1 => "1 match".to_string(),
                n => format!("{} matches", thousands(n)),
            }
        } else if editing && !kind.is_live() {
            // Trop lourde pour suivre la frappe : cette recherche attend la validation.
            "Enter to search".to_string()
        } else {
            String::new()
        };
        (format!("{}:", kind.label()), query, editing, status)
    };
    let base = Style::new().bg(PANEL_BG);
    for x in area.left()..area.right() {
        buf[(x, area.y)].reset();
    }
    buf.set_style(area, base);
    let end = area.right() - 1;
    let text_end = put_right(buf, area.x + 1, area.y, end, &status, base.fg(DIM)).saturating_sub(2);
    let x = put(buf, area.x + 1, area.y, text_end, &label, base.fg(ACCENT).add_modifier(Modifier::BOLD)) + 1;
    // Une saisie plus longue que le champ garde sa fin visible, curseur compris.
    let room = text_end.saturating_sub(x + 1) as usize;
    let length = query.chars().count();
    let shown: String = query.chars().skip(length.saturating_sub(room)).collect();
    let x = put(buf, x, area.y, text_end, &shown, base.fg(Color::White));
    if editing {
        put(buf, x, area.y, text_end, " ", Style::new().bg(Color::Gray));
    }
}

/// Panneau flottant des raccourcis, centré par-dessus la vue courante.
fn draw_help(buf: &mut Buffer, area: Rect) {
    let key_w = SHORTCUTS.iter().map(|(keys, _)| width(keys)).max().unwrap_or(0);
    let text_w = SHORTCUTS
        .iter()
        .map(|(keys, text)| if keys.is_empty() { width(text) } else { key_w + 2 + width(text) })
        .max()
        .unwrap_or(0);
    let w = (text_w + 6).min(area.width);
    let h = (SHORTCUTS.len() as u16 + 2).min(area.height);
    let panel = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    draw_panel(buf, panel, " Keyboard shortcuts ", " ? or Esc to close ");
    let base = Style::new().bg(PANEL_BG);
    let (left, right, top) = (panel.x, panel.right() - 1, panel.y);

    let x0 = left + 3;
    for (i, (keys, text)) in SHORTCUTS.iter().enumerate().take(h.saturating_sub(2) as usize) {
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
fn draw_flash(buf: &mut Buffer, app: &App, area: Rect) {
    let Some(text) = app.flash() else { return };
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Couleur de la palette à 256 teintes la plus proche, comme la choisit un
    /// terminal sans 24 bits : le cube 6×6×6 ou la rampe de gris.
    fn reduced(color: Color) -> [i32; 3] {
        let Color::Rgb(r, g, b) = color else { panic!("{color:?} n'est pas une couleur 24 bits") };
        let rgb = [r, g, b].map(i32::from);
        let cube = rgb.map(|v| [0, 95, 135, 175, 215, 255].into_iter().min_by_key(|level| (level - v).abs()).unwrap());
        let grey = 8 + 10 * ((rgb.iter().sum::<i32>() / 3 - 3) / 10).clamp(0, 23);
        let gap = |to: [i32; 3]| (0..3).map(|i| (to[i] - rgb[i]).pow(2)).sum::<i32>();
        if gap(cube) <= gap([grey; 3]) { cube } else { [grey; 3] }
    }

    #[test]
    fn syntax_tones_survive_256_colours() {
        // Les ajouts restent verts, les retraits rouges, jamais gris.
        for add in [SYNTAX_TONES.add, SYNTAX_TONES.add_mark] {
            let [r, g, b] = reduced(add);
            assert!(g >= 95 && r == 0 && b == 0, "{add:?} → {:?}", [r, g, b]);
        }
        for del in [SYNTAX_TONES.del, SYNTAX_TONES.del_mark] {
            let [r, g, b] = reduced(del);
            assert!(r >= 95 && g == 0 && b == 0, "{del:?} → {:?}", [r, g, b]);
        }
        // L'ancien vert des ajouts s'y perdait.
        assert_eq!(reduced(Color::Rgb(0, 40, 0)), [18; 3]);
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(1_700_000_000), (2023, 11, 14));
        assert_eq!(civil(951_782_400), (2000, 2, 29));
    }

    #[test]
    fn relative_ages() {
        let now = 1_700_000_000;
        assert_eq!(relative(now, now - 30, false), "just now");
        assert_eq!(relative(now, now - 5 * 3600, false), "5 h");
        assert_eq!(relative(now, now - 5 * 3600, true), "5 h ago");
        assert_eq!(relative(now, now - 30 * 3600, true), "yesterday");
        assert_eq!(relative(now, now - 3 * 86_400, false), "3 d");
        assert_eq!(relative(now, 0, false), "");
        assert_eq!(relative(now, 951_782_400, false), "2000-02-29");
    }

    #[test]
    fn long_paths_keep_their_end() {
        assert_eq!(tail("src/ui/diff_view.rs", 12), "…iff_view.rs");
        assert_eq!(tail("src/app.rs", 40), "src/app.rs");
    }

    #[test]
    fn thousands_are_grouped() {
        assert_eq!(thousands(9), "9");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
