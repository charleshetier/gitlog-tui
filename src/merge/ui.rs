//! Rendu de `gitmerge` : trois colonnes alignées rangée par rangée. La nôtre à
//! gauche, celle qui arrive à droite, le résultat au milieu avec le curseur.
//! Seules les rangées visibles sont lues, colorées et dessinées.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::kit::edit::{self, Mode};
use crate::kit::paint::{
    ACCENT, DIM, FIND_BG, FIND_CURRENT_BG, MIN_WRAP, PANEL_BG, SEL_BG, SYNTAX_TONES, TONES, Tones, VOID_BG, draw_find_bar,
    draw_flash, draw_help, draw_repo_title, find_status, mark_span, put, put_right, put_tinted, tail, width,
};
use crate::kit::syntax::Tint;
use crate::merge::app::{App, Confirm, FileState, HitMap, SCROLL_MARGIN, Sheet};
use crate::merge::model::{Kind, Row, Segment, Side, State};

/// Fond d'un côté écarté, et du résultat d'un bloc où rien n'a été pris.
const IGNORED_BG: Color = Color::Indexed(238);
/// Fond d'un conflit qui se fusionne mot à mot : ni rouge, puisqu'il n'y a rien à
/// décider, ni vert, puisque rien n'est encore fait.
const SIMPLE_BG: Color = Color::Indexed(58);
const SIMPLE_SYNTAX_BG: Color = Color::Rgb(66, 50, 0);
/// Fond d'un passage du résultat retouché à la main. Sous la coloration
/// syntaxique il est plus sombre, comme les autres fonds, mais reste bleu une
/// fois ramené à 256 couleurs.
const MANUAL_BG: Color = Color::Indexed(23);
const MANUAL_SYNTAX_BG: Color = Color::Rgb(0, 55, 75);
/// Flèches qui prennent un côté.
const TAKE_LEFT: &str = "»";
const TAKE_RIGHT: &str = "«";

/// Colonnes de l'écran : de chaque côté une version, sa flèche et un filet ; au
/// milieu, le résultat.
#[derive(Clone, Copy)]
struct Columns {
    left: (u16, u16),
    left_gutter: u16,
    left_bar: u16,
    mid: (u16, u16),
    right_bar: u16,
    right_gutter: u16,
    right: (u16, u16),
}

impl Columns {
    fn new(area: Rect) -> Columns {
        // Trois tiers, une fois ôtés les deux filets et les deux flèches.
        let side = (area.width - 4) / 3;
        let left = (area.x, area.x + side);
        let mid = (left.1 + 2, area.right() - side - 2);
        Columns { left, left_gutter: left.1, left_bar: left.1 + 1, mid, right_bar: mid.1, right_gutter: mid.1 + 1, right: (mid.1 + 2, area.right()) }
    }
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let buf = frame.buffer_mut();
    app.hit = HitMap::default();
    if area.width < 40 || area.height < 8 {
        put(buf, area.x, area.y, area.right(), "Window too small", Style::new());
        return;
    }
    let cols = Columns::new(area);
    let body = Rect::new(area.x, area.y + 2, area.width, area.height - 3);
    draw_header(buf, app, Rect::new(area.x, area.y, area.width, 1));
    draw_rule(buf, app, cols, Rect::new(area.x, area.y + 1, area.width, 1));

    let view = View { wrap: app.wrap, mode: app.mode, syntax: app.syntax };
    let message = match app.files.get_mut(app.current).map(|file| &mut file.state) {
        Some(FileState::Open(sheet)) => {
            let (lines, wrap, code_x) = draw_sheet(buf, sheet, body, cols, view);
            app.page = body.height as usize;
            let (left_gutter, right_gutter, code) = (cols.left_gutter, cols.right_gutter, (code_x, cols.mid.1));
            app.hit = HitMap { body, lines, left_gutter, right_gutter, code, wrap, ..app.hit.clone() };
            None
        }
        Some(FileState::Refused(why)) => Some(vec![
            format!("This conflict cannot be resolved here: {why}."),
            "Settle it with git (git add, git rm), then press R to read the conflicts again.".to_string(),
        ]),
        Some(FileState::Unread) => None,
        None => Some(vec!["No conflicted file left.".to_string(), "q quits.".to_string()]),
    };
    if let Some(message) = message {
        for (i, text) in message.iter().enumerate() {
            let style = if i == 0 { Style::new().add_modifier(Modifier::BOLD) } else { Style::new().fg(DIM) };
            put(buf, body.x + 2, body.y + 1 + i as u16, body.right() - 1, text, style);
        }
    }
    draw_status(buf, app, Rect::new(area.x, area.bottom() - 1, area.width, 1));
    // Une question ou une saisie occupent la ligne d'état : aucun message ne les recouvre.
    if app.confirm.is_none() && app.prompt.is_none() {
        draw_flash(buf, area, app.flash());
    }
    if app.help {
        draw_help(buf, area, SHORTCUTS);
    }
}

/// En-tête : le dépôt, l'opération en cours, et où l'on en est dans les fichiers.
fn draw_header(buf: &mut Buffer, app: &mut App, area: Rect) {
    let end = area.right() - 1;
    let dim = Style::new().fg(DIM);
    let staged = app.files.iter().filter(|file| file.staged()).count();
    let mut count = match app.files.len() {
        0 => String::new(),
        len => format!("file {}/{len}", app.current + 1),
    };
    if staged > 0 {
        count = format!("{count} · {staged} staged");
    }
    let count_x = put_right(buf, area.x + 10, area.y, end, &count, dim);
    let text_end = count_x.saturating_sub(2);
    let (x, folder, remote) = draw_repo_title(buf, &app.repo, app.plain, (area.x + 1, area.y, text_end));
    (app.hit.folder, app.hit.remote) = (Some(folder), remote);
    let what = match app.incoming.operation {
        "" => format!(" · conflicts on {}", app.ours),
        operation => format!(" · {operation}: {} → {}", app.incoming.name, app.ours),
    };
    put(buf, x, area.y, text_end, &what, dim);
}

/// Filet sous l'en-tête ; il porte le titre de chaque colonne.
fn draw_rule(buf: &mut Buffer, app: &App, cols: Columns, area: Rect) {
    let dim = Style::new().fg(DIM);
    let bold = Style::new().add_modifier(Modifier::BOLD);
    for x in area.left()..area.right() {
        put(buf, x, area.y, area.right(), "─", dim);
    }
    let Some(file) = app.file() else { return };
    let sheet = file.sheet();
    if sheet.is_some() {
        put(buf, cols.left_bar, area.y, area.right(), "┬", dim);
        put(buf, cols.right_bar, area.y, area.right(), "┬", dim);
    }
    // Un titre : son rôle en gras, puis ce que c'est.
    let title = |buf: &mut Buffer, (x, end): (u16, u16), role: &str, what: &str| {
        let end = end.saturating_sub(1);
        let x = put(buf, x + 1, area.y, end, &format!(" {role}"), bold);
        let room = end.saturating_sub(x + 4);
        put(buf, x, area.y, end, &format!(" · {} ", tail(what, room)), dim)
    };
    if sheet.is_none() {
        title(buf, (area.x, area.right()), "result", &file.entry.path);
        return;
    }
    title(buf, cols.left, "ours", &app.ours);
    title(buf, cols.right, "theirs", &app.incoming.name);
    // Au milieu, le fichier et son état : conflits qui restent, résultat non écrit, réglé.
    let state = sheet.map(|sheet| match (sheet.unresolved(), sheet.staged, sheet.dirty) {
        (_, true, _) => ("staged ".to_string(), Color::LightGreen),
        (0, _, true) => ("not saved ".to_string(), Color::Yellow),
        (0, _, false) if sheet.saved => ("saved ".to_string(), Color::LightGreen),
        (0, ..) => ("no conflict left ".to_string(), Color::LightGreen),
        (1, ..) => ("1 conflict ".to_string(), Color::LightRed),
        (n, ..) => (format!("{n} conflicts "), Color::LightRed),
    });
    let (text, color) = state.unwrap_or_default();
    let end = cols.mid.1.saturating_sub(1);
    let state_x = put_right(buf, cols.mid.0 + 12, area.y, end, &format!(" {text}"), Style::new().fg(color));
    title(buf, (cols.mid.0, state_x), "result", &file.entry.path);
}

#[derive(Clone, Copy)]
struct View {
    wrap: bool,
    mode: Mode,
    syntax: bool,
}

/// Fonds des blocs, selon que la coloration syntaxique est active.
#[derive(Clone, Copy)]
struct Palette {
    tones: Tones,
    manual: Color,
    simple: Color,
}

impl Palette {
    fn new(syntax: bool) -> Palette {
        match syntax {
            true => Palette { tones: SYNTAX_TONES, manual: MANUAL_SYNTAX_BG, simple: SIMPLE_SYNTAX_BG },
            false => Palette { tones: TONES, manual: MANUAL_BG, simple: SIMPLE_BG },
        }
    }
}

/// Colonne d'une rangée.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Column {
    Side(Side),
    Result,
}

/// Fond d'un bloc dans une colonne : rouge tant que le conflit attend, ocre s'il
/// se fusionne mot à mot, vert pour ce qui est pris, gris pour ce qui est écarté.
fn tone(seg: &Segment, column: Column, palette: Palette, simple: bool) -> Option<Color> {
    let (tones, pending) = (palette.tones, if simple { palette.simple } else { palette.tones.del });
    match column {
        Column::Side(side) => seg.changed(side).then(|| match seg.state(side) {
            State::Pending => pending,
            State::Applied => tones.add,
            State::Ignored => IGNORED_BG,
        }),
        Column::Result => match seg.kind {
            Kind::Equal => None,
            Kind::Manual => Some(palette.manual),
            _ if seg.pending() => Some(pending),
            _ if [Side::Left, Side::Right].iter().any(|&side| seg.changed(side) && seg.state(side) == State::Applied) => Some(tones.add),
            _ => Some(IGNORED_BG),
        },
    }
}

/// Nombre de lignes d'écran qu'occupe un texte de `len` caractères replié dans `room`.
fn parts(len: usize, room: usize) -> usize {
    len.div_ceil(room.max(1)).max(1)
}

/// Ce qu'une colonne montre sur une rangée : le numéro de la ligne, son texte
/// affiché et ses teintes.
type Content<'a> = Option<(usize, &'a str, &'a [Tint])>;

/// Dessine une rangée d'une colonne, de `x0` à `end`, le code commençant en `code_x`.
fn draw_cell(buf: &mut Buffer, (x0, code_x, end): (u16, u16, u16), y: u16, content: Content, bg: Option<Color>, (skip, part): (usize, usize)) {
    let area = Rect::new(x0, y, end - x0, 1);
    let Some((number, text, tints)) = content else {
        // Rien en face dans cette colonne.
        buf.set_style(area, Style::new().bg(VOID_BG));
        return;
    };
    if let Some(bg) = bg {
        buf.set_style(area, Style::new().bg(bg));
    }
    if part == 0 {
        let num_w = (code_x - x0 - 1) as usize;
        put(buf, x0, y, code_x, &format!("{number:>num_w$}"), Style::new().fg(DIM));
    } else if skip >= text.chars().count() {
        // Une autre colonne est plus longue : il ne reste ici que le fond.
        return;
    }
    put_tinted(buf, (code_x, y, end), text, tints, skip, (&[], Color::Reset));
}

/// Dessine le document ; renvoie, pour chaque ligne du corps, la rangée montrée
/// et le rang de la ligne d'écran dans cette rangée, puis si le code est replié
/// et la colonne où commence celui du résultat.
fn draw_sheet(buf: &mut Buffer, sheet: &mut Sheet, body: Rect, cols: Columns, view: View) -> (Vec<(usize, usize)>, bool, u16) {
    let most = sheet.doc.ours.len().max(sheet.doc.theirs.len()).max(sheet.doc.lines().count());
    let num_w = most.to_string().len().max(3) as u16;
    // Dans chaque colonne : le numéro de ligne, une espace, puis le code.
    let code = num_w + 1;
    let (left_x, mid_x, right_x) = (cols.left.0 + code, cols.mid.0 + code, cols.right.0 + code);
    let room = |x: u16, end: u16| end.saturating_sub(x) as usize;
    let (left_room, mid_room, right_room) = (room(left_x, cols.left.1), room(mid_x, cols.mid.1), room(right_x, cols.right.1));
    // Trop étroit, le code ne se replie pas.
    let wrap = view.wrap && left_room.min(mid_room).min(right_room) >= MIN_WRAP;
    let height = body.height as usize;

    sheet.settle_cursor(view.mode);
    let cursor_cell = edit::cell(sheet.line().unwrap_or(""), sheet.col);
    // De côté, la vue suit le curseur ; replié, le code ne défile plus.
    if wrap {
        sheet.x = 0;
    } else if cursor_cell < sheet.x {
        sheet.x = cursor_cell;
    } else if cursor_cell >= sheet.x + mid_room {
        sheet.x = cursor_cell + 1 - mid_room;
    }

    // Hauteur d'une rangée : celle de sa colonne la plus longue. La ligne du
    // curseur compte la cellule qui suit son dernier caractère.
    let doc = &sheet.doc;
    let cursor_row = sheet.row;
    let row_height = |row: &Row| {
        if !wrap {
            return 1;
        }
        let seg = &doc.segs[row.seg];
        let len = |text: Option<&String>| text.map_or(0, |text| edit::shown(text).chars().count());
        let left = len(seg.left.clone().nth(row.line).and_then(|i| doc.ours.get(i)));
        let right = len(seg.right.clone().nth(row.line).and_then(|i| doc.theirs.get(i)));
        let mid = len(seg.lines.get(row.line)) + usize::from(row.row == cursor_row);
        parts(left, left_room).max(parts(mid, mid_room)).max(parts(right, right_room))
    };

    // De haut en bas, la vue suit le curseur en gardant un peu de marge.
    let rows = doc.rows();
    let mut top = sheet.top.min(rows.saturating_sub(1));
    if cursor_row < top + SCROLL_MARGIN {
        top = cursor_row.saturating_sub(SCROLL_MARGIN);
    }
    let target = (cursor_row + SCROLL_MARGIN).min(rows.saturating_sub(1));
    let heights: Vec<usize> = doc.walk(top).take(target + 1 - top.min(target)).map(|row| row_height(&row)).collect();
    let mut used: usize = heights.iter().sum();
    for row_h in &heights {
        if used <= height || top >= cursor_row {
            break;
        }
        used -= row_h;
        top += 1;
    }
    sheet.top = top;

    // Les teintes des lignes à l'écran, et de celles qui les précèdent.
    let shown: Vec<Row> = sheet.doc.walk(top).take(height).collect();
    let upto = |pick: fn(&Segment) -> std::ops::Range<usize>| {
        shown.iter().filter_map(|row| pick(&sheet.doc.segs[row.seg]).nth(row.line)).max().map_or(0, |line| line + 1)
    };
    let (left_upto, right_upto) = (upto(|seg| seg.left.clone()), upto(|seg| seg.right.clone()));
    let mid_upto = shown.iter().filter_map(|row| row.number).max().map_or(0, |line| line + 1);
    let from = sheet.tints[0].len();
    sheet.tints[0].extend(left_upto, sheet.doc.ours.iter().skip(from).map(String::as_str));
    let from = sheet.tints[1].len();
    sheet.tints[1].extend(mid_upto, sheet.doc.lines().skip(from));
    let from = sheet.tints[2].len();
    sheet.tints[2].extend(right_upto, sheet.doc.theirs.iter().skip(from).map(String::as_str));

    let doc = &sheet.doc;
    // Les conflits visibles qui se fusionnent mot à mot : leur fond les distingue.
    // Un seul essai par bloc, et seulement pour ceux qu'on montre.
    let mut simple: Vec<usize> = Vec::new();
    let mut tried = None;
    for row in &shown {
        if tried != Some(row.seg) {
            tried = Some(row.seg);
            if doc.segs[row.seg].pending() && doc.simple(row.seg).is_some() {
                simple.push(row.seg);
            }
        }
    }
    let palette = Palette::new(view.syntax);
    let dim = Style::new().fg(DIM);
    let current_seg = sheet.pos().seg;
    let (mut y, bottom) = (body.y, body.bottom());
    let mut lines: Vec<(usize, usize)> = Vec::new();
    for row in &shown {
        if y >= bottom {
            break;
        }
        let seg = &doc.segs[row.seg];
        let left = seg.left.clone().nth(row.line);
        let right = seg.right.clone().nth(row.line);
        let left_text = left.map(|line| edit::shown(&doc.ours[line]));
        let right_text = right.map(|line| edit::shown(&doc.theirs[line]));
        let mid_text = row.number.map(|_| edit::shown(&seg.lines[row.line]));
        let on_cursor = row.row == sheet.row;
        // Le bloc où se trouve le curseur ressort : c'est lui que les touches règlent.
        let bar = if row.seg == current_seg && seg.kind != Kind::Equal { Style::new().fg(ACCENT) } else { dim };
        let easy = simple.contains(&row.seg);
        for part in 0..row_height(row) {
            if y >= bottom {
                break;
            }
            let skip = |room: usize| if wrap { part * room } else { sheet.x };
            let content = |line: Option<usize>, text: &'_ Option<String>, tints: usize| -> Option<(usize, String, Vec<Tint>)> {
                Some((line? + 1, text.clone()?, sheet.tints[tints].get(line?).to_vec()))
            };
            let side = |buf: &mut Buffer, at: (u16, u16, u16), content: Option<(usize, String, Vec<Tint>)>, column: Column, skip: usize| {
                let content = content.as_ref().map(|(number, text, tints)| (*number, text.as_str(), tints.as_slice()));
                draw_cell(buf, at, y, content, tone(seg, column, palette, easy), (skip, part));
            };
            side(buf, (cols.left.0, left_x, cols.left.1), content(left, &left_text, 0), Column::Side(Side::Left), skip(left_room));
            side(buf, (cols.right.0, right_x, cols.right.1), content(right, &right_text, 2), Column::Side(Side::Right), skip(right_room));
            let mid = (mid_x, y, cols.mid.1);
            let mid_bg = tone(seg, Column::Result, palette, easy).or(on_cursor.then_some(SEL_BG));
            let mid_content = content(row.number, &mid_text, 1);
            match &mid_content {
                Some((number, text, tints)) => {
                    draw_cell(buf, (cols.mid.0, mid_x, cols.mid.1), y, Some((*number, text, tints)), mid_bg, (skip(mid_room), part));
                    if on_cursor && part == 0 {
                        buf.set_style(Rect::new(cols.mid.0, y, num_w, 1), Style::new().fg(Color::White));
                    }
                    if let Some(find) = &sheet.find {
                        for (start, len, current) in find.on(*number - 1) {
                            let bg = if current { FIND_CURRENT_BG } else { FIND_BG };
                            mark_span(buf, mid, skip(mid_room), (start, start + len), Style::new().fg(Color::Black).bg(bg));
                        }
                    }
                }
                None => draw_cell(buf, (cols.mid.0, mid_x, cols.mid.1), y, None, None, (0, part)),
            }
            // Le curseur : un pavé, d'une autre teinte en insertion.
            let cursor_part = if wrap { cursor_cell / mid_room.max(1) } else { 0 };
            if on_cursor && part == cursor_part && cursor_cell >= skip(mid_room) {
                let before: String = mid_text.as_deref().unwrap_or("").chars().skip(skip(mid_room)).take(cursor_cell - skip(mid_room)).collect();
                let x = mid_x + width(&before);
                if x < cols.mid.1 {
                    let bg = if view.mode == Mode::Insert { Color::LightGreen } else { Color::Gray };
                    buf.set_style(Rect::new(x, y, 1, 1), Style::new().fg(Color::Black).bg(bg));
                }
            }
            // Les flèches, sur la première ligne du bloc, du côté qui a changé.
            if part == 0 && row.line == 0 {
                for (side, x, arrow) in [(Side::Left, cols.left_gutter, TAKE_LEFT), (Side::Right, cols.right_gutter, TAKE_RIGHT)] {
                    if seg.changed(side) {
                        let style = match seg.state(side) {
                            State::Pending if easy => Style::new().fg(Color::Yellow),
                            State::Pending => Style::new().fg(Color::LightYellow).add_modifier(Modifier::BOLD),
                            State::Applied => Style::new().fg(Color::LightGreen),
                            State::Ignored => dim,
                        };
                        put(buf, x, y, x + 1, arrow, style);
                    }
                }
            }
            put(buf, cols.left_bar, y, cols.left_bar + 1, "│", bar);
            put(buf, cols.right_bar, y, cols.right_bar + 1, "│", bar);
            lines.push((row.row, part));
            y += 1;
        }
    }
    // Sous la dernière rangée, les filets continuent jusqu'en bas.
    for y in y..bottom {
        put(buf, cols.left_bar, y, cols.left_bar + 1, "│", dim);
        put(buf, cols.right_bar, y, cols.right_bar + 1, "│", dim);
    }
    (lines, wrap, mid_x)
}

/// Ligne d'état : le mode, la place du curseur et ce qu'on peut faire du bloc où
/// il se trouve. Une question ou la recherche en cours prennent sa place.
fn draw_status(buf: &mut Buffer, app: &App, area: Rect) {
    let base = Style::new().bg(PANEL_BG);
    for x in area.left()..area.right() {
        buf[(x, area.y)].reset();
    }
    buf.set_style(area, base);
    let end = area.right() - 1;
    if let Some(confirm) = app.confirm {
        let unsaved = app.files.iter().filter(|file| file.sheet().is_some_and(|sheet| sheet.dirty)).count();
        let lost = if unsaved == 1 { "1 result is not saved".to_string() } else { format!("{unsaved} results are not saved") };
        let ask = match confirm {
            Confirm::Quit => format!("{lost}. Quit and lose them? y: quit, any other key: stay"),
            Confirm::Reload => format!("{lost}. Read again from git and lose them? y: reload, any other key: stay"),
        };
        put(buf, area.x + 1, area.y, end, &ask, base.fg(Color::Yellow).add_modifier(Modifier::BOLD));
        return;
    }
    if let Some((query, editing, current, count)) = app.shown_find()
        && (editing || app.mode == Mode::Normal)
    {
        draw_find_bar(buf, area, "find:", query, editing, &find_status(query, current, count));
        return;
    }
    let badge = if app.mode == Mode::Insert { Color::LightGreen } else { ACCENT };
    let mut x = put(buf, area.x, area.y, end, &format!(" {} ", app.mode.label()), Style::new().fg(Color::Black).bg(badge).add_modifier(Modifier::BOLD));
    let help_x = put_right(buf, x, area.y, end, "? help", base.fg(DIM));
    let end = help_x.saturating_sub(2);
    let Some(sheet) = app.sheet() else { return };
    let pos = sheet.pos();
    let place = match sheet.line() {
        Some(_) => format!("{}:{}", sheet.doc.line_number(pos) + 1, sheet.col + 1),
        None => "-".to_string(),
    };
    x = put(buf, x + 1, area.y, end, &place, base.fg(DIM));
    if let Some(key) = app.keys.pending() {
        x = put(buf, x + 2, area.y, end, &key.to_string(), base.fg(Color::Yellow));
    }
    if app.mode == Mode::Insert {
        put(buf, x + 3, area.y, end, "Esc: back to normal mode", base.fg(DIM));
        return;
    }
    let Some(seg) = sheet.doc.segs.get(pos.seg) else { return };
    let (what, keys) = match seg.kind {
        Kind::Equal => return,
        // Un conflit que `m` règle de lui-même : les deux côtés ont changé des mots différents.
        Kind::Conflict if seg.pending() && sheet.doc.simple(pos.seg).is_some() => {
            ("simple conflict", "m merge   > ours   < theirs   Space keep as is")
        }
        Kind::Conflict if seg.pending() => ("conflict", "> ours   < theirs   Space keep as is"),
        Kind::Conflict => ("conflict, settled", "> ours   < theirs"),
        Kind::Left => ("changed by ours", "> ours   < theirs"),
        Kind::Right => ("changed by theirs", "> ours   < theirs"),
        Kind::Both => ("same change on both sides", "> ours   < theirs"),
        Kind::Manual => ("edited by hand", "> puts back the original"),
    };
    let color = if seg.pending() { Color::LightRed } else { Color::Gray };
    x = put(buf, x + 3, area.y, end, what, base.fg(color).add_modifier(Modifier::BOLD));
    put(buf, x + 3, area.y, end, keys, base.fg(DIM));
}

/// Raccourcis du panneau d'aide : (touches, effet). Sans touche : un titre.
const SHORTCUTS: &[(&str, &str)] = &[
    ("", "Navigation"),
    ("↑ ↓  j k", "Move the cursor; PgUp PgDn by a page; g G first / last line"),
    ("← →  h l", "Move in the line; 0 $ ^ its ends; w b e by word"),
    ("n N", "Next / previous conflict; the matches during a search"),
    ("} {", "Next / previous change"),
    ("Tab", "Next conflicted file; Shift+Tab: previous"),
    ("/", "Search the result; Esc leaves the search"),
    ("", ""),
    ("", "Merge"),
    ("> <", "Take ours (left) / theirs (right); the other then adds up"),
    ("Space", "Keep the result as it is: the conflict is settled"),
    ("m M", "Merge a simple conflict word to word / all of the file"),
    ("Ctrl+S", "Write the result to the file"),
    ("S", "Write it and stage the file (git add): conflict resolved"),
    ("R", "Read the conflicts again from git"),
    ("", ""),
    ("", "Editing the result"),
    ("i a I A", "Insert at / after the cursor, at the line start / end"),
    ("o O  Enter", "Insert on a new line below / above; Enter: at the cursor"),
    ("Esc", "Back to normal mode"),
    ("dd  x", "Delete the line / the character"),
    ("u  Ctrl+R", "Undo / redo"),
    ("", ""),
    ("", "Other"),
    ("y  z", "Copy the line; wrap or cut the long lines"),
    ("? · q", "Show or hide this panel · quit, asking if a result waits"),
    ("Click", "On » or «: take that side; in the result: put the cursor"),
    ("Wheel", "Scroll"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_help_panel_fits_a_small_terminal() {
        // Trente lignes, cadre compris.
        assert!(SHORTCUTS.len() + 2 <= 30);
        // Et quatre-vingts colonnes.
        let key_w = SHORTCUTS.iter().map(|(keys, _)| width(keys)).max().unwrap();
        assert!(SHORTCUTS.iter().all(|(_, text)| key_w + 2 + width(text) + 6 <= 80));
    }

    #[test]
    fn three_columns_fill_the_width() {
        let cols = Columns::new(Rect::new(0, 0, 120, 30));
        assert_eq!((cols.left, cols.mid, cols.right), ((0, 38), (40, 80), (82, 120)));
        assert_eq!((cols.left_gutter, cols.left_bar, cols.right_bar, cols.right_gutter), (38, 39, 80, 81));
    }
}
