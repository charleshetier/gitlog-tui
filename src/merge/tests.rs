//! Tests de bout en bout de `gitmerge` : un vrai dépôt temporaire, une vraie
//! fusion en conflit, un terminal simulé.

use std::path::{Path, PathBuf};
use std::process::Command;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use ratatui::layout::Position;

use crate::kit::edit::Mode;
use crate::kit::git::Repo;
use crate::merge::app::{Action, App, FileState};
use crate::merge::model::{Doc, Kind};
use crate::merge::keys::on_key;
use crate::merge::ui;

fn git(dir: &Path, args: &[&str]) -> bool {
    run_git(dir, args).status.success()
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    String::from_utf8_lossy(&run_git(dir, args).stdout).into_owned()
}

fn run_git(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.name=Alice", "-c", "user.email=alice@example.com"])
        .args(["-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"])
        .args(args)
        .output()
        .expect("git doit être dans le PATH")
}

fn screen(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| ui::draw(frame, app)).unwrap();
    let buffer = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..height {
        for x in 0..width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

/// Frappe des touches : chaque caractère de `keys` est une touche.
fn press(app: &mut App, keys: &str) {
    for c in keys.chars() {
        key(app, KeyCode::Char(c));
    }
}

fn key(app: &mut App, code: KeyCode) {
    if let Some(action) = on_key(app, KeyEvent::new(code, KeyModifiers::NONE)) {
        app.apply(action);
    }
}

fn ctrl(app: &mut App, c: char) {
    if let Some(action) = on_key(app, KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)) {
        app.apply(action);
    }
}

/// Un fichier de plus dans le dépôt : nom, base, notre version, la leur ; `None` le supprime.
type Versions<'a> = (&'a str, &'a str, Option<&'a str>, Option<&'a str>);

const BASE: &str = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\n";

/// Une fusion de `feature` dans `main` arrêtée sur ses conflits. `a.txt` : un
/// conflit sur la ligne 2, un changement de notre seul côté (ligne 6), un de
/// l'autre (une ligne ajoutée à la fin). `b.txt` : un conflit. `files` ajoute
/// d'autres fichiers.
fn make_conflict(name: &str, files: &[Versions]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("gitmerge-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut all = vec![
        ("a.txt", BASE, Some("one\nTWO ours\nthree\nfour\nfive\nSIX\nseven\neight\n"), Some("one\nTWO theirs\nthree\nfour\nfive\nsix\nseven\neight\nnine\n")),
        ("b.txt", "alpha\nbeta\n", Some("alpha\nbeta ours\n"), Some("alpha\nbeta theirs\n")),
    ];
    all.extend_from_slice(files);
    let write = |pick: fn(&Versions) -> Option<String>| {
        for file in &all {
            match pick(file) {
                Some(text) => std::fs::write(dir.join(file.0), text).unwrap(),
                None => drop(std::fs::remove_file(dir.join(file.0))),
            }
        }
    };
    assert!(git(&dir, &["init", "-q", "-b", "main"]));
    write(|file| Some(file.1.to_string()));
    assert!(git(&dir, &["add", "-A"]) && git(&dir, &["commit", "-q", "-m", "Base"]));
    assert!(git(&dir, &["checkout", "-q", "-b", "feature"]));
    write(|file| file.3.map(str::to_string));
    assert!(git(&dir, &["add", "-A"]) && git(&dir, &["commit", "-q", "-m", "Theirs"]));
    assert!(git(&dir, &["checkout", "-q", "main"]));
    write(|file| file.2.map(str::to_string));
    assert!(git(&dir, &["add", "-A"]) && git(&dir, &["commit", "-q", "-m", "Ours"]));
    assert!(!git(&dir, &["merge", "feature"]), "la fusion doit s'arrêter sur un conflit");
    dir
}

fn open(dir: &Path) -> App {
    App::open(Repo::open(dir).unwrap(), true, false).unwrap()
}

fn text(app: &App) -> String {
    app.sheet().expect("un fichier ouvert").doc.text()
}

/// Les segments se suivent sans trou ni chevauchement, de chaque côté.
fn check(doc: &Doc) {
    let (mut left, mut right) = (0, 0);
    for seg in &doc.segs {
        assert_eq!((seg.left.start, seg.right.start), (left, right), "{:?}", doc.segs);
        assert!(seg.rows() > 0, "segment vide : {:?}", doc.segs);
        if seg.kind == Kind::Equal {
            assert_eq!(seg.lines, doc.ours[seg.left.clone()]);
            assert_eq!(seg.lines, doc.theirs[seg.right.clone()]);
        }
        (left, right) = (seg.left.end, seg.right.end);
    }
    assert_eq!((left, right), (doc.ours.len(), doc.theirs.len()));
}

#[test]
fn opens_on_the_first_conflict() {
    let dir = make_conflict("open", &[]);
    let mut app = open(&dir);
    let view = screen(&mut app, 120, 16);
    assert!(view.contains("merge: feature → main"), "{view}");
    assert!(view.contains("file 1/2"), "{view}");
    assert!(view.contains("ours · main") && view.contains("theirs · feature") && view.contains("result · a.txt"), "{view}");
    assert!(view.contains("1 conflict"), "{view}");
    // Le conflit garde la base au milieu, entre les deux côtés et leurs flèches.
    let row = view.lines().find(|line| line.contains("TWO ours")).expect("la ligne du conflit");
    assert!(row.contains("2 TWO ours") && row.contains("»│  2 two") && row.contains("│«  2 TWO theirs"), "{row}");
    // Les changements d'un seul côté sont déjà dans le résultat.
    assert_eq!(text(&app), "one\ntwo\nthree\nfour\nfive\nSIX\nseven\neight\nnine\n");
    let sheet = app.sheet().unwrap();
    assert_eq!((sheet.row, sheet.unresolved()), (1, 1));
    // La ligne ajoutée par l'autre côté n'a rien en face à gauche.
    assert!(view.lines().any(|line| line.contains("│  9 nine") && line.contains("«  9 nine")), "{view}");
    assert!(view.contains(" NORMAL ") && view.contains("conflict") && view.contains("> ours"), "{view}");
    check(&sheet.doc);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn taking_sides_undo_and_redo() {
    let dir = make_conflict("take", &[]);
    let mut app = open(&dir);
    press(&mut app, ">");
    assert!(text(&app).starts_with("one\nTWO ours\nthree"));
    assert_eq!(app.sheet().unwrap().unresolved(), 0);
    assert_eq!(app.flash(), Some("Took ours · no conflict left: Ctrl+S saves, S stages"));
    // L'autre côté s'ajoute à la suite ; repris seul, il remplace tout.
    press(&mut app, "<");
    assert!(text(&app).starts_with("one\nTWO ours\nTWO theirs\nthree"));
    assert_eq!(app.flash(), Some("Added theirs below · no conflict left: Ctrl+S saves, S stages"));
    press(&mut app, "<");
    assert!(text(&app).starts_with("one\nTWO theirs\nthree"));
    press(&mut app, "uu");
    assert!(text(&app).starts_with("one\nTWO ours\nthree"));
    press(&mut app, "u");
    assert!(text(&app).starts_with("one\ntwo\nthree"));
    assert_eq!(app.sheet().unwrap().unresolved(), 1);
    press(&mut app, "u");
    assert_eq!(app.flash(), Some("Already at the oldest change"));
    ctrl(&mut app, 'r');
    assert!(text(&app).starts_with("one\nTWO ours\nthree"));
    // Un changement pris d'office se défait en prenant l'autre côté : la base revient.
    press(&mut app, "}");
    assert_eq!(app.sheet().unwrap().line(), Some("SIX"));
    press(&mut app, "<");
    assert!(text(&app).contains("five\nsix\nseven"));
    assert_eq!(app.flash(), Some("Took theirs"));
    press(&mut app, "}c");
    assert_eq!(app.flash(), Some("No more changes"));
    // `Tab` vaut `]` : le conflit suivant, en boucle. Il n'en reste aucun.
    press(&mut app, "C{");
    assert_eq!(app.sheet().unwrap().line(), Some("TWO ours"));
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.flash(), Some("No conflict left in this file"));
    // Hors d'un bloc, il n'y a rien à prendre.
    press(&mut app, "g>");
    assert_eq!(app.flash(), Some("No change under the cursor"));
    check(&app.sheet().unwrap().doc);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_result_is_edited_in_insert_mode() {
    let dir = make_conflict("edit", &[]);
    let mut app = open(&dir);
    // `x` garde la base ; `A` écrit à la fin de la ligne.
    press(&mut app, "x");
    assert_eq!(app.sheet().unwrap().unresolved(), 0);
    press(&mut app, "A");
    assert_eq!(app.mode, Mode::Insert);
    press(&mut app, " and a half");
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.sheet().unwrap().line(), Some("two and a half"));
    // En insertion, les lettres des commandes s'écrivent.
    press(&mut app, "o");
    press(&mut app, "x > q");
    key(&mut app, KeyCode::Enter);
    press(&mut app, "next");
    key(&mut app, KeyCode::Esc);
    assert!(text(&app).starts_with("one\ntwo and a half\nx > q\nnext\nthree"), "{}", text(&app));
    // Toute une saisie se défait d'un coup.
    press(&mut app, "u");
    assert!(text(&app).starts_with("one\ntwo and a half\nthree"));
    ctrl(&mut app, 'r');
    press(&mut app, "dd");
    assert!(text(&app).starts_with("one\ntwo and a half\nx > q\nthree"), "{}", text(&app));
    // Le retour arrière en début de ligne la joint à la précédente.
    press(&mut app, "kI");
    key(&mut app, KeyCode::Backspace);
    key(&mut app, KeyCode::Esc);
    assert!(text(&app).starts_with("one\ntwo and a halfx > q\nthree"), "{}", text(&app));
    // Une saisie sans frappe ne laisse rien à défaire.
    press(&mut app, "i");
    key(&mut app, KeyCode::Esc);
    press(&mut app, "u");
    assert!(text(&app).starts_with("one\ntwo and a half\nx > q\nthree"), "{}", text(&app));
    // Une ligne ouverte reprend le retrait de celle d'où l'on vient.
    press(&mut app, "ggI");
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Esc);
    press(&mut app, "o");
    press(&mut app, "in");
    key(&mut app, KeyCode::Esc);
    assert!(text(&app).starts_with("\tone\n\tin\n"), "{}", text(&app));
    let view = screen(&mut app, 120, 16);
    assert!(view.contains("edited by hand") && view.contains("not saved"), "{view}");
    check(&app.sheet().unwrap().doc);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn saving_writes_the_file_and_staging_resolves_it() {
    let dir = make_conflict("save", &[]);
    let mut app = open(&dir);
    // Tant qu'un conflit attend, le fichier n'est pas remis à git.
    press(&mut app, "S");
    assert_eq!(app.flash(), Some("1 conflict left: take a side, or x keeps the result as it is"));
    assert!(git_out(&dir, &["ls-files", "-u"]).contains("a.txt"));
    press(&mut app, ">");
    ctrl(&mut app, 's');
    let saved = "one\nTWO ours\nthree\nfour\nfive\nSIX\nseven\neight\nnine\n";
    assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), saved);
    assert_eq!(app.flash(), Some("Saved a.txt · S stages it"));
    // Écrire ne règle rien pour git : il faut le demander.
    assert!(git_out(&dir, &["ls-files", "-u"]).contains("a.txt"));
    assert!(screen(&mut app, 120, 16).contains(" saved "));
    press(&mut app, "S");
    assert!(!git_out(&dir, &["ls-files", "-u"]).contains("a.txt"));
    assert_eq!(app.flash(), Some("Staged a.txt · 1 file left"));
    // Le fichier suivant qui reste à régler s'ouvre.
    assert_eq!(app.current, 1);
    let view = screen(&mut app, 120, 16);
    assert!(view.contains("file 2/2 · 1 staged") && view.contains("result · b.txt"), "{view}");
    // `S` écrit le résultat s'il ne l'est pas encore.
    press(&mut app, "<S");
    assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "alpha\nbeta theirs\n");
    assert_eq!(git_out(&dir, &["ls-files", "-u"]), "");
    assert_eq!(app.flash(), Some("Staged b.txt · every conflict is resolved: git commit concludes the merge"));
    assert!(screen(&mut app, 120, 16).contains("staged"));
    // La fusion se conclut par un commit ordinaire.
    assert!(git(&dir, &["commit", "-q", "--no-edit"]));
    // Relus, les conflits ont disparu.
    press(&mut app, "R");
    assert!(app.files.is_empty());
    assert!(screen(&mut app, 120, 16).contains("No conflicted file left."));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn n_walks_the_files_and_keeps_their_results() {
    let dir = make_conflict("files", &[]);
    let mut app = open(&dir);
    press(&mut app, ">n");
    assert_eq!(app.current, 1);
    assert_eq!(text(&app), "alpha\nbeta\n");
    press(&mut app, "n");
    assert_eq!((app.current, app.flash()), (1, Some("Last conflicted file")));
    // Le résultat du premier fichier attend, sans être écrit.
    press(&mut app, "N");
    assert!(text(&app).starts_with("one\nTWO ours"));
    press(&mut app, "N");
    assert_eq!((app.current, app.flash()), (0, Some("First conflicted file")));
    assert!(std::fs::read_to_string(dir.join("a.txt")).unwrap().contains("<<<<<<<"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn search_highlights_and_moves_the_cursor() {
    let dir = make_conflict("find", &[]);
    let mut app = open(&dir);
    press(&mut app, "/e");
    let view = screen(&mut app, 120, 16);
    // La saisie suit le curseur : la première occurrence après lui, la deuxième du fichier.
    assert!(view.contains("find: e") && view.contains("match 2/8"), "{view}");
    assert_eq!(app.sheet().unwrap().line(), Some("three"));
    press(&mut app, "ight");
    key(&mut app, KeyCode::Enter);
    let sheet = app.sheet().unwrap();
    assert_eq!((sheet.line(), sheet.col), (Some("eight"), 0));
    assert_eq!(sheet.find.as_ref().map(|find| find.hits.len()), Some(1));
    // `n` parcourt alors les occurrences, pas les fichiers.
    press(&mut app, "/");
    key(&mut app, KeyCode::Esc);
    press(&mut app, "/I");
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.sheet().unwrap().line(), Some("SIX"));
    press(&mut app, "n");
    assert_eq!((app.current, app.sheet().unwrap().line()), (0, Some("SIX")));
    // Sans majuscule, la casse ne compte pas ; la recherche suit les modifications.
    press(&mut app, "/t");
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.sheet().unwrap().find.as_ref().unwrap().hits.len(), 3);
    press(&mut app, "g>");
    press(&mut app, "]>");
    assert_eq!(app.sheet().unwrap().find.as_ref().unwrap().hits.len(), 3, "{}", text(&app));
    press(&mut app, "N");
    assert_eq!(app.sheet().unwrap().line(), Some("eight"));
    // Une recherche sans résultat ne reste pas ; Esc retire celle en place.
    press(&mut app, "/zzz");
    assert!(screen(&mut app, 120, 16).contains("no match"));
    key(&mut app, KeyCode::Enter);
    assert!(app.sheet().unwrap().find.is_none());
    press(&mut app, "/o");
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Esc);
    assert!(app.sheet().unwrap().find.is_none());
    press(&mut app, "n");
    assert_eq!(app.current, 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn quitting_asks_before_losing_a_result() {
    let dir = make_conflict("quit", &[]);
    let mut app = open(&dir);
    press(&mut app, ">q");
    assert!(!app.should_quit);
    let view = screen(&mut app, 120, 16);
    assert!(view.contains("1 result is not saved. Quit and lose them? y: quit, any other key: stay"), "{view}");
    // Toute autre touche annule, sans rien faire d'autre.
    press(&mut app, "<");
    assert!(app.confirm.is_none() && text(&app).starts_with("one\nTWO ours\nthree"));
    press(&mut app, "R");
    assert!(screen(&mut app, 120, 16).contains("Read again from git and lose them?"));
    press(&mut app, "y");
    assert!(text(&app).starts_with("one\ntwo\nthree"));
    assert_eq!(app.flash(), Some("2 conflicted files"));
    // Sans rien à perdre, `q` quitte aussitôt.
    press(&mut app, "q");
    assert!(app.should_quit);
    let mut app = open(&dir);
    press(&mut app, ">qy");
    assert!(app.should_quit);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn files_it_cannot_merge_are_shown_as_such() {
    let dir = make_conflict(
        "refused",
        &[("c.txt", "x\n", None, Some("x theirs\n")), ("d.bin", "a\0b\n", Some("a\0ours\n"), Some("a\0theirs\n")), ("e.txt", "", Some("new ours\n"), Some("new theirs\n"))],
    );
    let mut app = open(&dir);
    assert_eq!(app.files.len(), 5);
    press(&mut app, "nn");
    let view = screen(&mut app, 120, 16);
    assert!(view.contains("This conflict cannot be resolved here: our side deleted this file."), "{view}");
    assert!(view.contains("result · c.txt"), "{view}");
    // Les touches d'édition n'y font rien.
    press(&mut app, "ix>");
    assert_eq!(app.mode, Mode::Normal);
    press(&mut app, "n");
    assert!(matches!(&app.files[3].state, FileState::Refused(why) if why == "this is a binary file"));
    // Un fichier vide à la base : tout ce que chaque côté y a mis est en conflit.
    press(&mut app, "n");
    assert_eq!((app.sheet().unwrap().unresolved(), text(&app).as_str()), (1, ""));
    press(&mut app, "><");
    assert_eq!(text(&app), "new ours\nnew theirs\n");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_mouse_takes_a_side_and_puts_the_cursor() {
    let dir = make_conflict("mouse", &[]);
    let mut app = open(&dir);
    let view = screen(&mut app, 120, 16);
    let y = view.lines().position(|line| line.contains("TWO theirs")).unwrap() as u16;
    let hit = app.hit.clone();
    app.apply(Action::Click(Position::new(hit.right_gutter, y)));
    assert!(text(&app).starts_with("one\nTWO theirs\nthree"));
    // Un clic dans le résultat y pose le curseur, à la colonne visée.
    screen(&mut app, 120, 16);
    let y = view.lines().position(|line| line.contains("seven")).unwrap() as u16;
    app.apply(Action::Click(Position::new(app.hit.code.0 + 2, y)));
    let sheet = app.sheet().unwrap();
    assert_eq!((sheet.line(), sheet.col), (Some("seven"), 2));
    // Une flèche n'existe que du côté qui a changé.
    let y = view.lines().position(|line| line.contains("SIX")).unwrap() as u16;
    app.apply(Action::Click(Position::new(hit.right_gutter, y)));
    assert!(text(&app).contains("SIX"));
    app.apply(Action::Click(Position::new(hit.left_gutter, y)));
    assert!(text(&app).contains("SIX"));
    app.apply(Action::Scroll(3));
    assert!(app.sheet().unwrap().row >= app.sheet().unwrap().top);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn long_lines_wrap_or_scroll() {
    let long = "a long line: ".repeat(20);
    let (ours, theirs) = (format!("{long}ours\nend\n"), format!("{long}theirs\nend\n"));
    let dir = make_conflict("wrap", &[("z.txt", "short\nend\n", Some(&ours), Some(&theirs))]);
    let mut app = open(&dir);
    press(&mut app, "nn>");
    let cut = screen(&mut app, 120, 24);
    assert_eq!(cut.matches("a long line").count(), 3 * 2, "{cut}");
    // À la fin de la ligne, la vue a défilé de côté pour montrer le curseur.
    press(&mut app, "$");
    assert!(screen(&mut app, 120, 24).contains("line: ours"));
    assert!(app.sheet().unwrap().x > 0);
    press(&mut app, "w");
    let wrapped = screen(&mut app, 120, 24);
    assert!(wrapped.matches("a long line").count() > 30, "{wrapped}");
    assert!(wrapped.contains("line: ours") && wrapped.contains("line: theirs"), "{wrapped}");
    // « end » passe sous toutes les lignes d'écran de la ligne repliée.
    let (first, last) = (wrapped.lines().position(|line| line.contains("  1 a long")).unwrap(), wrapped.lines().position(|line| line.contains("  2 end")).unwrap());
    assert!(last - first >= 7, "{wrapped}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn any_keys_at_any_size_keep_the_document_sound() {
    let dir = make_conflict("fuzz", &[("e.txt", "", Some("new ours\n"), Some("new theirs\n"))]);
    let keys = [
        KeyCode::Char('j'), KeyCode::Char('k'), KeyCode::Char('>'), KeyCode::Char('<'), KeyCode::Char('x'), KeyCode::Char('i'),
        KeyCode::Char('a'), KeyCode::Char('o'), KeyCode::Char('O'), KeyCode::Char('d'), KeyCode::Char('u'), KeyCode::Char('A'),
        KeyCode::Char(']'), KeyCode::Char('}'), KeyCode::Char('{'), KeyCode::Char('G'), KeyCode::Char('g'), KeyCode::Char('w'),
        KeyCode::Char('n'), KeyCode::Char('N'), KeyCode::Char('l'), KeyCode::Char('h'), KeyCode::Char('$'), KeyCode::Char('z'),
        KeyCode::Esc, KeyCode::Enter, KeyCode::Backspace, KeyCode::Delete, KeyCode::Tab, KeyCode::Up, KeyCode::Down, KeyCode::Left,
        KeyCode::Right, KeyCode::Home, KeyCode::End, KeyCode::PageDown, KeyCode::PageUp, KeyCode::Esc, KeyCode::Backspace,
    ];
    let mut seed = 2024u32;
    let mut next = move |below: usize| {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
        (seed >> 16) as usize % below
    };
    for round in 0..6 {
        let mut app = open(&dir);
        for step in 0..500 {
            let code = keys[next(keys.len())];
            if next(40) == 0 {
                ctrl(&mut app, 'r');
            }
            key(&mut app, code);
            if let Some(sheet) = app.sheet() {
                check(&sheet.doc);
                assert!(sheet.row < sheet.doc.rows().max(1), "tour {round}, pas {step}");
            }
            if step % 7 == 0 {
                screen(&mut app, 40 + next(120) as u16, 8 + next(40) as u16);
            }
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}
