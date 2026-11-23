//! Rendu : seules les lignes visibles sont lues et dessinées, et chaque zone
//! cliquable est mémorisée pour traduire les clics en actions.

use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use crate::app::{App, FilesState, HitMap};
use crate::diff::{DiffLine, FileEntry, LineKind};
use crate::git::{Head, Label, RefKind};
use crate::graph::COMMIT;

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
const REMOTE_BG: Color = Color::Indexed(240);
const CHIP_BG: Color = Color::Indexed(238);
const ADD_BG: Color = Color::Indexed(22);
const DEL_BG: Color = Color::Indexed(52);
const DIM: Color = Color::DarkGray;

const ARROW: &str = "\u{e0b2}";
const ARROW_PLAIN: &str = "◀";
const COPY: &str = "\u{f0c5}";
const COPY_PLAIN: &str = "⧉";

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
            out.push(' ');
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
        0..60 => return "à l'instant".to_string(),
        60..3600 => format!("{} min", age / 60),
        3600..86_400 => format!("{} h", age / 3600),
        86_400..172_800 => return "hier".to_string(),
        172_800..2_592_000 => format!("{} j", age / 86_400),
        _ => {
            let (y, m, d) = civil(t);
            return format!("{y:04}-{m:02}-{d:02}");
        }
    };
    if long { format!("il y a {short}") } else { short }
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let buf = frame.buffer_mut();
    app.hit = HitMap::default();
    if area.width < 30 || area.height < 6 {
        put(buf, area.x, area.y, area.right(), "Fenêtre trop petite", Style::new());
        return;
    }

    let rule_top = area.y + 1;
    let body_y = area.y + 2;
    let rule_bottom = area.bottom() - 2;
    let body_h = rule_bottom - body_y;
    let dim = Style::new().fg(DIM);
    // Colonne du séparateur vertical quand le volet est ouvert.
    let divider = app.pane.is_some().then(|| area.x + area.width / 2);

    for y in [rule_top, rule_bottom] {
        for x in area.left()..area.right() {
            put(buf, x, y, area.right(), "─", dim);
        }
    }
    if let Some(x) = divider {
        for y in area.y..rule_bottom {
            put(buf, x, y, area.right(), "│", dim);
        }
        put(buf, x, rule_top, area.right(), "┼", dim);
        put(buf, x, rule_bottom, area.right(), "┴", dim);
    }

    let left_w = divider.map_or(area.width, |x| x - area.x);
    draw_header(buf, app, Rect::new(area.x, area.y, left_w, 1));
    draw_graph(buf, app, Rect::new(area.x, body_y, left_w, body_h));
    if let Some(x) = divider {
        let pane_w = area.right() - x - 1;
        draw_pane_header(buf, app, Rect::new(x + 1, area.y, pane_w, 1));
        let body = Rect::new(x + 1, body_y, pane_w, body_h);
        app.hit.pane = body;
        if app.depth() == 3 {
            draw_diff(buf, app, body);
        } else {
            draw_files(buf, app, body);
        }
    }
    draw_status(buf, app, Rect::new(area.x, area.bottom() - 1, area.width, 1));
}

fn draw_header(buf: &mut Buffer, app: &App, area: Rect) {
    let end = area.right() - 1;
    let dim = Style::new().fg(DIM);
    let n = thousands(app.store.len());
    let count = if app.log_error.is_some() {
        "erreur git".to_string()
    } else if app.log_done {
        format!("{n} commits")
    } else if app.want_all {
        format!("chargement… {n}")
    } else {
        format!("{n}+ commits")
    };
    let count_x = put_right(buf, area.x + 10, area.y, end, &count, dim);

    let text_end = count_x.saturating_sub(2);
    let mut x = put(buf, area.x + 1, area.y, text_end, "gitlog", Style::new().add_modifier(Modifier::BOLD));
    x = put(buf, x + 2, area.y, text_end, &app.repo.name, dim);
    let head = match &app.repo.head {
        Head::Branch(name) => format!(" · HEAD sur {name}"),
        Head::Detached(oid) => format!(" · HEAD détaché sur {}", oid.short()),
        Head::Unborn => String::new(),
    };
    put(buf, x, area.y, text_end, &head, dim);
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
            None if app.log_done => ("Aucun commit.".to_string(), dim),
            None => ("Chargement…".to_string(), dim),
        };
        put(buf, area.x + 1, area.y, end, &text, style);
        return;
    }
    app.top = app.top.min(len - 1);
    app.hit.graph_top = app.top;
    let last = (app.top + area.height as usize).min(len);

    let graph_w = (app.top..last)
        .map(|row| app.store.cells(row).len())
        .max()
        .unwrap_or(0)
        .min(area.width as usize / 3) as u16;
    // Dans un volet étroit, le hash et l'auteur cèdent la place au message.
    let show_hash = area.width >= 60;
    let show_meta = area.width >= 90;
    let hash_x = area.x + 1 + graph_w + 1;
    let icon_x = hash_x + 8;
    let text_x = if show_hash { icon_x + 2 } else { hash_x };
    if show_hash {
        app.hit.icon_x = Some(icon_x);
    }
    let now = now_secs();

    for row in app.top..last {
        let y = area.y + (row - app.top) as u16;
        let selected = row == app.selected;
        if selected {
            buf.set_style(Rect::new(area.x, y, area.width, 1), Style::new().bg(SEL_BG));
        }

        for (i, cell) in app.store.cells(row).iter().take(graph_w as usize).enumerate() {
            if cell.bits == 0 {
                continue;
            }
            let mut style = Style::new().fg(palette(cell.color));
            if cell.bits & COMMIT != 0 {
                style = style.add_modifier(Modifier::BOLD);
            }
            let mut glyph = [0u8; 4];
            put(buf, area.x + 1 + i as u16, y, end, cell.glyph().encode_utf8(&mut glyph), style);
        }

        let color = palette(app.store.color(row));
        let oid = app.store.oid(row);
        let meta = app.meta(row);
        let bright = if selected { Style::new().fg(Color::White) } else { Style::new() };
        if show_hash {
            let hash_style = if selected { Style::new().fg(Color::Gray) } else { dim };
            put(buf, hash_x, y, end, &oid.short(), hash_style);
            put(buf, icon_x, y, end, copy_icon(app.plain), if selected { bright } else { dim });
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
            x = draw_labels(buf, x, y, text_end, labels, color, app.plain);
        }
        put(buf, x, y, text_end, &meta.subject, bright);
    }
}

fn copy_icon(plain: bool) -> &'static str {
    if plain { COPY_PLAIN } else { COPY }
}

/// Étiquettes en flèche pointant vers le commit, à gauche.
fn draw_labels(
    buf: &mut Buffer,
    mut x: u16,
    y: u16,
    end: u16,
    labels: &[Label],
    color: Color,
    plain: bool,
) -> u16 {
    let arrow = if plain { ARROW_PLAIN } else { ARROW };
    for (i, label) in labels.iter().enumerate() {
        let name: String = if label.name.chars().count() > 32 {
            label.name.chars().take(31).chain(['…']).collect()
        } else {
            label.name.clone()
        };
        let remaining = labels.len() - i;
        // Place gardée pour le compteur « +N » des étiquettes non affichées.
        let reserve = if remaining > 1 { 4 } else { 0 };
        if x + width(&name) + 4 + reserve > end {
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
        x = put(buf, x, y, end, arrow, Style::new().fg(bg));
        x = put(buf, x, y, end, &format!(" {name} "), style) + 1;
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

    put(buf, x0, area.y, end, &pane.meta.subject, Style::new().add_modifier(Modifier::BOLD));
    let info = format!("{} · {}", pane.meta.author, relative(now_secs(), pane.meta.time, true));
    put(buf, x0, area.y + 1, end, &info, dim);

    let files: &[FileEntry] = match &pane.files {
        FilesState::Loading => {
            put(buf, x0, area.y + 2, end, "Chargement des fichiers…", dim);
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
    let summary = format!("{} fichier{plural} modifié{plural}", files.len());
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
            put(buf, area.x, y, end, "▌", Style::new().fg(Color::LightBlue));
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
            _ => put_right(buf, x0, y, end, "binaire", dim),
        };
        let path_end = stats_x.saturating_sub(2);
        let room = path_end.saturating_sub(x0 + 2);
        put(buf, x0 + 2, y, path_end, &tail(&file.path, room), Style::new());
    }
}

/// Niveau 3 : diff unifié du fichier, avec numéros de ligne.
fn draw_diff(buf: &mut Buffer, app: &mut App, area: Rect) {
    app.pane_height = area.height as usize;
    let dim = Style::new().fg(DIM);
    let x0 = area.x + 1;
    let end = area.right() - 1;
    let Some(diff) = app.pane.as_mut().and_then(|p| p.diff.as_mut()) else { return };
    if diff.lines.is_empty() {
        let text = if diff.done { "Aucune différence à afficher." } else { "Chargement du diff…" };
        put(buf, x0, area.y, end, text, dim);
        return;
    }
    diff.top = diff.top.min(diff.lines.len() - 1);
    let visible: &[DiffLine] = &diff.lines[diff.top..(diff.top + area.height as usize).min(diff.lines.len())];
    let max_no = visible.iter().map(|l| l.old.max(l.new)).max().unwrap_or(0);
    let num_w = max_no.to_string().len().max(3);
    let number = |n: u32| if n == 0 { " ".repeat(num_w) } else { format!("{n:>num_w$}") };

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
            LineKind::Add => buf.set_style(Rect::new(area.x, y, area.width, 1), Style::new().bg(ADD_BG)),
            LineKind::Del => buf.set_style(Rect::new(area.x, y, area.width, 1), Style::new().bg(DEL_BG)),
            LineKind::Ctx => {}
        }
        let (sign, sign_color) = match line.kind {
            LineKind::Add => ("+", Color::LightGreen),
            LineKind::Del => ("−", Color::LightRed),
            _ => (" ", DIM),
        };
        let numbers = format!("{} {}", number(line.old), number(line.new));
        let mut x = put(buf, x0, y, end, &numbers, Style::new().fg(Color::Gray));
        x = put(buf, x + 1, y, end, sign, Style::new().fg(sign_color).add_modifier(Modifier::BOLD));
        let text: String = line.text.chars().skip(diff.x).collect();
        put(buf, x + 1, y, end, &text, Style::new());
    }
}

fn draw_status(buf: &mut Buffer, app: &App, area: Rect) {
    let end = area.right() - 1;
    if let Some(text) = app.flash() {
        let style = Style::new().fg(Color::LightGreen).add_modifier(Modifier::BOLD);
        put(buf, area.x + 1, area.y, end, text, style);
        return;
    }
    let hints = match app.depth() {
        1 => "↑↓ naviguer · Entrée ou clic : ouvrir le commit · y copier le hash · q quitter",
        2 => "↑↓ fichier · Entrée ou clic : diff du fichier · y copier le hash · q fermer le volet · 1 2 changer de niveau",
        _ => "↑↓ défiler · ←→ décaler · q revenir aux fichiers · 1 2 3 changer de niveau",
    };
    put(buf, area.x + 1, area.y, end, hints, Style::new().fg(DIM));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(1_700_000_000), (2023, 11, 14));
        assert_eq!(civil(951_782_400), (2000, 2, 29));
    }

    #[test]
    fn relative_ages() {
        let now = 1_700_000_000;
        assert_eq!(relative(now, now - 30, false), "à l'instant");
        assert_eq!(relative(now, now - 5 * 3600, false), "5 h");
        assert_eq!(relative(now, now - 5 * 3600, true), "il y a 5 h");
        assert_eq!(relative(now, now - 30 * 3600, true), "hier");
        assert_eq!(relative(now, now - 3 * 86_400, false), "3 j");
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
        assert_eq!(thousands(1_234_567), "1 234 567");
    }
}
