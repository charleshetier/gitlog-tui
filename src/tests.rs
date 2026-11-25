//! Test de bout en bout : un vrai dépôt temporaire, le vrai `git`, un terminal simulé.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::app::{Action, App, FilesState, HierState, NodeStyle};
use crate::git::Repo;
use crate::ui;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.name=Alice", "-c", "user.email=alice@example.com"])
        .args(["-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"])
        .args(args)
        .output()
        .expect("git doit être dans le PATH");
    assert!(status.status.success(), "git {args:?} : {}", String::from_utf8_lossy(&status.stderr));
}

/// main : c1, c2, puis merge de `feature` (f1) ; tag v0.1.0 sur c1.
fn make_repo(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("gitlog-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "un\ndeux\ntrois\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "Squelette initial"]);
    git(&dir, &["tag", "v0.1.0"]);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.join("a.txt"), "un\nDEUX\ntrois\nquatre\n").unwrap();
    std::fs::write(dir.join("b.txt"), "nouveau\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "Travail sur la branche"]);
    git(&dir, &["checkout", "-q", "main"]);
    std::fs::write(dir.join("c.txt"), "principal\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "Suite sur main"]);
    git(&dir, &["merge", "-q", "--no-ff", "-m", "Merge de feature", "feature"]);
    dir
}

fn wait(app: &mut App, what: &str, done: impl Fn(&App) -> bool) {
    let start = Instant::now();
    while !done(app) {
        app.tick();
        assert!(start.elapsed() < Duration::from_secs(300), "délai dépassé : {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
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

#[test]
fn three_levels_end_to_end() {
    let dir = make_repo("levels");
    git(&dir, &["remote", "add", "origin", "https://example.com/alice/demo.git"]);
    let mut app = App::new(Repo::open(&dir).unwrap(), true).unwrap();
    wait(&mut app, "historique", |app| app.log_done);
    assert_eq!(app.store.len(), 4);

    // Niveau 1 : graphe, étiquettes, hash et icône de copie.
    let level1 = screen(&mut app, 110, 12);
    println!("{level1}");
    let lines: Vec<&str> = level1.lines().collect();
    assert!(lines[0].contains("HEAD on main") && lines[0].contains("4 commits"));
    // En-tête : le dossier puis le dépôt distant, chacun derrière son icône.
    assert!(lines[0].starts_with(" ⌂ gitlog-test-levels-"), "{}", lines[0]);
    assert!(lines[0].contains("  ◆ demo · HEAD on main"), "{}", lines[0]);
    // Les deux noms sont cliquables : chemin du dossier, URL du remote.
    let (folder, remote) = (app.hit.folder.unwrap(), app.hit.remote.unwrap());
    assert_eq!((folder.x, folder.y, remote.y), (1, 0, 0));
    assert_eq!(remote.width, 6, "icône, espace, « demo »");
    assert!(folder.right() < remote.x);
    assert_eq!(app.repo.remote_url.as_deref(), Some("https://example.com/alice/demo.git"));
    assert!(app.folder_path().ends_with(&format!("gitlog-test-levels-{}", std::process::id())));
    assert_eq!(app.folder_path().contains('/'), !cfg!(windows));
    assert!(lines[2].contains("◉─╮") && lines[2].contains("◀ HEAD → main ") && lines[2].contains("Merge de feature"));
    assert!(!lines[2].contains("⧉") && lines[2].contains("Alice"));
    assert!(level1.contains("◀ feature ") && level1.contains("◀ v0.1.0 "));
    assert!(lines[5].contains("●─╯") && lines[5].contains("Squelette initial"));
    assert_eq!(app.depth(), 1);

    // Niveau 2 : le merge est comparé à son premier parent.
    app.apply(Action::Open);
    wait(&mut app, "fichiers", |app| {
        app.pane.as_ref().is_some_and(|p| !matches!(p.files, FilesState::Loading))
    });
    let level2 = screen(&mut app, 110, 12);
    println!("{level2}");
    assert_eq!(app.depth(), 2);
    assert!(level2.contains("Commit ") && level2.contains(" 1 ") && level2.contains(" 2 "));
    assert!(level2.contains("2 files changed") && level2.contains("a.txt") && level2.contains("b.txt"));
    assert!(level2.contains("+2") && level2.contains("−1"));

    // Niveau 3 : diff du premier fichier, numéros de ligne compris.
    app.apply(Action::Open);
    wait(&mut app, "diff", |app| {
        app.pane.as_ref().and_then(|p| p.diff.as_ref()).is_some_and(|d| d.done)
    });
    let level3 = screen(&mut app, 110, 12);
    println!("{level3}");
    assert_eq!(app.depth(), 3);
    assert!(level3.contains(" 3 ") && level3.contains("@@ -1,3 +1,4 @@"));
    // Fichier modifié : deux colonnes, l'ancienne ligne face à la nouvelle.
    let changed = level3.lines().find(|l| l.contains("deux")).expect("ligne modifiée");
    assert!(changed.contains("2 deux") && changed.contains("2 DEUX"), "{changed}");
    let added = level3.lines().find(|l| l.contains("quatre")).expect("ligne ajoutée");
    assert!(added.contains("│   4 quatre"), "{added}");

    // Fichier ajouté : une seule version existe, le diff reste sur une colonne.
    app.apply(Action::Back { quit: true });
    app.apply(Action::Move(1));
    app.apply(Action::Open);
    wait(&mut app, "diff du fichier ajouté", |app| {
        app.pane.as_ref().and_then(|p| p.diff.as_ref()).is_some_and(|d| d.done)
    });
    let unified = screen(&mut app, 110, 12);
    println!("{unified}");
    // Le fond vert suffit à dire l'ajout : pas de « + » devant chaque ligne.
    assert!(unified.contains("b.txt") && unified.contains("  1 nouveau") && !unified.contains("+ nouveau"), "{unified}");

    // q remonte d'un niveau à la fois, puis quitte.
    app.apply(Action::Back { quit: true });
    assert_eq!(app.depth(), 2);
    app.apply(Action::Back { quit: true });
    assert_eq!(app.depth(), 1);
    assert!(!app.should_quit);
    app.apply(Action::Back { quit: true });
    assert!(app.should_quit);

    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn navigation_and_levels_by_index() {
    let dir = make_repo("nav");
    let mut app = App::new(Repo::open(&dir).unwrap(), true).unwrap();
    wait(&mut app, "historique", |app| app.log_done);
    screen(&mut app, 100, 10);

    app.apply(Action::Move(1));
    assert_eq!(app.selected, 1);
    app.apply(Action::Bottom);
    assert_eq!(app.selected, 3);
    app.apply(Action::Move(1));
    assert_eq!(app.selected, 3);
    app.apply(Action::Top);
    assert_eq!(app.selected, 0);

    // Les touches 1 2 3 n'atteignent que les niveaux accessibles.
    app.apply(Action::GoLevel(3));
    assert_eq!(app.depth(), 1);
    app.apply(Action::GoLevel(2));
    assert_eq!(app.depth(), 2);
    wait(&mut app, "fichiers", |app| {
        app.pane.as_ref().is_some_and(|p| matches!(p.files, FilesState::Ready(_)))
    });
    app.apply(Action::GoLevel(3));
    assert_eq!(app.depth(), 3);
    app.apply(Action::GoLevel(1));
    assert_eq!(app.depth(), 1);

    // Un clic sur un autre commit ouvre son volet au niveau 2.
    app.apply(Action::ClickCommit(3));
    assert_eq!((app.selected, app.depth()), (3, 2));

    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn outside_a_repository_is_a_clear_error() {
    let dir = std::env::temp_dir().join(format!("gitlog-test-norepo-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let err = Repo::open(&dir).unwrap_err().to_string();
    assert!(err.contains("is not inside a git repository"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Mesures sur un gros dépôt, à lancer à la main :
/// `GITLOG_BENCH_REPO=<chemin> cargo test --release bench -- --ignored --nocapture`
#[test]
#[ignore]
fn bench_big_repository() {
    let Some(dir) = std::env::var_os("GITLOG_BENCH_REPO") else {
        eprintln!("GITLOG_BENCH_REPO n'est pas défini : rien à mesurer");
        return;
    };
    let start = Instant::now();
    let mut app = App::new(Repo::open(Path::new(&dir)).unwrap(), true).unwrap();
    app.graph_height = 50;
    wait(&mut app, "premières lignes", |app| app.store.len() >= 50 || app.log_done);
    let first_rows = start.elapsed();

    let t = Instant::now();
    screen(&mut app, 200, 54);
    let first_frame = t.elapsed();
    let t = Instant::now();
    screen(&mut app, 200, 54);
    let next_frame = t.elapsed();

    // Contre-pression : sans défilement, le chargement s'arrête de lui-même.
    let until = Instant::now() + Duration::from_millis(800);
    while Instant::now() < until {
        app.tick();
        std::thread::sleep(Duration::from_millis(2));
    }
    let plateau = app.store.len();
    let paused = !app.log_done;
    assert!(app.log_done || plateau <= 50 + 2000 + 256, "le flux ne s'est pas arrêté : {plateau}");

    let t = Instant::now();
    app.apply(Action::Bottom);
    wait(&mut app, "historique complet", |app| app.log_done);
    let full_load = t.elapsed();
    let t = Instant::now();
    screen(&mut app, 200, 54);
    let last_frame = t.elapsed();

    let rows = app.store.len();
    println!("commits                      : {rows}");
    println!("premières lignes             : {first_rows:?}");
    println!("première image (50 lignes)   : {first_frame:?}");
    println!("image suivante (cache chaud) : {next_frame:?}");
    println!("lignes chargées au repos     : {plateau} (git en pause : {paused})");
    println!("chargement complet           : {full_load:?}");
    println!("image en fin d'historique    : {last_frame:?}");
    println!("mémoire du graphe            : {} octets par commit", app.store.memory_bytes() / rows.max(1));
}

#[test]
fn copy_step_and_help_shortcuts() {
    let dir = make_repo("keys");
    let mut app = App::new(Repo::open(&dir).unwrap(), true).unwrap();
    wait(&mut app, "historique", |app| app.log_done);
    screen(&mut app, 110, 30);
    let files_ready = |app: &App| app.pane.as_ref().is_some_and(|p| matches!(p.files, FilesState::Ready(_)));
    let diff_file = |app: &App| app.pane.as_ref().and_then(|p| p.diff.as_ref()).map(|d| d.file);

    // y : le hash sur un commit, le chemin sur un fichier.
    assert_eq!(app.copy_target(), Some(app.store.oid(0).to_hex()));
    app.apply(Action::Open);
    assert_eq!(app.copy_target(), Some(app.store.oid(0).to_hex()), "fichiers pas encore chargés");
    wait(&mut app, "fichiers", files_ready);
    assert_eq!(app.copy_target().as_deref(), Some("a.txt"));
    app.apply(Action::Move(1));
    assert_eq!(app.copy_target().as_deref(), Some("b.txt"));

    // Au clavier : n pour le suivant, N (Maj+n) pour le précédent ; p n'est plus une touche.
    {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let key = |c: char, modifiers| crate::on_key(&app, KeyEvent::new(KeyCode::Char(c), modifiers));
        assert!(matches!(key('n', KeyModifiers::NONE), Some(Action::Step(1))));
        assert!(matches!(key('N', KeyModifiers::SHIFT), Some(Action::Step(-1))));
        assert!(key('p', KeyModifiers::NONE).is_none());
    }

    // n / N dans la liste des fichiers : commit suivant, précédent.
    app.apply(Action::Step(1));
    assert_eq!((app.depth(), app.selected), (2, 1));
    assert_eq!(app.pane.as_ref().map(|p| p.oid), Some(app.store.oid(1)));
    app.apply(Action::Step(-1));
    app.apply(Action::Step(-1));
    assert_eq!((app.depth(), app.selected), (2, 0));
    assert_eq!(app.flash(), Some("First commit"));

    // n / N dans un diff : fichier suivant, précédent, sans sortir du commit.
    wait(&mut app, "fichiers", files_ready);
    app.apply(Action::Open);
    assert_eq!(diff_file(&app), Some(0));
    app.apply(Action::Step(1));
    assert_eq!(diff_file(&app), Some(1));
    assert_eq!(app.copy_target().as_deref(), Some("b.txt"));
    app.apply(Action::Step(1));
    assert_eq!(diff_file(&app), Some(1));
    assert_eq!(app.flash(), Some("Last file of the commit"));
    app.apply(Action::Step(-1));
    assert_eq!(diff_file(&app), Some(0));

    // ? : panneau flottant, qui ne laisse passer que sa fermeture.
    // Sans pied de page : la liste va jusqu'en bas, le message éphémère s'y superpose à droite.
    let bottom = screen(&mut app, 110, 30);
    let last = bottom.lines().last().unwrap();
    assert!(last.trim_end().ends_with("Last file of the commit"), "{last}");
    assert!(last.contains('│') && !bottom.contains('┴') && !bottom.contains("? help"));
    app.apply(Action::ToggleHelp);
    let help = screen(&mut app, 110, 30);
    println!("{help}");
    assert!(help.contains("Keyboard shortcuts") && help.contains("Copy the commit hash, or the file path"));
    assert!(help.contains("╰") && help.contains("Scroll the graph or the pane"));
    app.apply(Action::Move(1));
    app.apply(Action::Back { quit: true });
    assert!(!app.help && !app.should_quit);
    assert_eq!(app.depth(), 3);
    assert!(!screen(&mut app, 110, 30).contains("Keyboard shortcuts"));

    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn whole_file_window() {
    // Un fichier de 60 lignes, dont trois endroits changent : seul le fichier entier les relie.
    let dir = std::env::temp_dir().join(format!("gitlog-test-full-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    let lines = |edit: &dyn Fn(u32) -> String| (1..=60).map(edit).collect::<Vec<_>>().join("\n") + "\n";
    std::fs::write(dir.join("big.txt"), lines(&|n| format!("ligne {n}"))).unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "Base"]);
    let edited = lines(&|n| if n == 5 || n == 50 { format!("LIGNE {n}") } else { format!("ligne {n}") });
    std::fs::write(dir.join("big.txt"), edited + "ligne 61\n").unwrap();
    git(&dir, &["commit", "-q", "-am", "Trois retouches"]);

    let mut app = App::new(Repo::open(&dir).unwrap(), true).unwrap();
    wait(&mut app, "historique", |app| app.log_done);
    screen(&mut app, 120, 24);
    app.apply(Action::Open);
    wait(&mut app, "fichiers", |app| app.pane.as_ref().is_some_and(|p| matches!(p.files, FilesState::Ready(_))));
    app.apply(Action::Open);
    wait(&mut app, "diff", |app| app.pane.as_ref().and_then(|p| p.diff.as_ref()).is_some_and(|d| d.done));
    let hunks = app.pane.as_ref().unwrap().diff.as_ref().unwrap().len();
    assert!(hunks < 40, "le diff ne porte que les hunks : {hunks} lignes");

    // Entrée sur le diff : le fichier entier, placé sur la première modification.
    app.apply(Action::Open);
    assert_eq!(app.depth(), 4);
    wait(&mut app, "fichier entier", |app| app.pane.as_ref().and_then(|p| p.full.as_ref()).is_some_and(|d| d.done));
    let full = |app: &App| {
        let view = app.pane.as_ref().unwrap().full.as_ref().unwrap();
        (view.top, view.change_tops().collect::<Vec<_>>(), view.len())
    };
    let (top, tops, len) = full(&app);
    assert!(len > 61, "{len} lignes");
    assert_eq!(tops.len(), 3);
    assert_eq!(top, tops[0]);
    let first = screen(&mut app, 120, 24);
    println!("{first}");
    assert!(first.contains("big.txt · change 1/3") && first.contains("n N changes"));
    let changed = first.lines().find(|l| l.contains("LIGNE 5")).expect("ligne modifiée");
    assert!(changed.contains("5 ligne 5") && changed.contains("5 LIGNE 5"), "{changed}");
    assert!(first.contains("ligne 12"), "le contexte dépasse celui du diff");

    // n et N sautent d'une modification à l'autre, sans sortir du fichier.
    app.apply(Action::Step(1));
    assert_eq!(full(&app).0, tops[1]);
    let second = screen(&mut app, 120, 24);
    assert!(second.contains("change 2/3") && second.contains("LIGNE 50") && second.contains("ligne 47"));
    app.apply(Action::Step(1));
    app.apply(Action::Step(1));
    assert_eq!(full(&app).0, tops[2]);
    assert_eq!(app.flash(), Some("Last change"));
    app.apply(Action::Step(-1));
    app.apply(Action::Step(-1));
    app.apply(Action::Step(-1));
    assert_eq!(full(&app).0, tops[0]);
    assert_eq!(app.flash(), Some("First change"));
    // Le défilement et la copie visent la fenêtre et son fichier.
    app.apply(Action::Move(2));
    assert_eq!(full(&app).0, tops[0] + 2);
    assert_eq!(app.copy_target().as_deref(), Some("big.txt"));

    // Échap referme la fenêtre : le diff du niveau 3 est toujours là, intact.
    app.apply(Action::Back { quit: false });
    assert_eq!(app.depth(), 3);
    assert_eq!(app.pane.as_ref().unwrap().diff.as_ref().unwrap().len(), hunks);
    assert!(!screen(&mut app, 120, 24).contains("n N changes"));
    app.apply(Action::GoLevel(4));
    assert_eq!(app.depth(), 4);
    app.apply(Action::GoLevel(2));
    assert_eq!(app.depth(), 2);

    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn syntax_colours_the_diff() {
    use ratatui::buffer::Buffer;
    use ratatui::style::Color;

    let dir = std::env::temp_dir().join(format!("gitlog-test-syntax-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    let source = |word: &str| format!("// greeting\npub fn hello() -> &'static str {{\n    \"{word}\"\n}}\n");
    std::fs::write(dir.join("lib.rs"), source("hello")).unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "Base"]);
    std::fs::write(dir.join("lib.rs"), source("bonjour")).unwrap();
    git(&dir, &["commit", "-q", "-am", "Edit"]);

    let draw = |app: &mut App| -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal.draw(|frame| ui::draw(frame, app)).unwrap();
        terminal.backend().buffer().clone()
    };
    // Couleurs de texte et de fond de la première cellule de `needle` à l'écran.
    let colours = |buffer: &Buffer, needle: &str| -> (Color, Color) {
        let needle: Vec<char> = needle.chars().collect();
        for y in 0..24 {
            let row: Vec<char> = (0..120).map(|x| buffer[(x, y)].symbol().chars().next().unwrap_or(' ')).collect();
            if let Some(x) = row.windows(needle.len()).position(|w| w == needle.as_slice()) {
                let cell = &buffer[(x as u16, y)];
                return (cell.fg, cell.bg);
            }
        }
        panic!("« {} » n'est pas à l'écran", needle.iter().collect::<String>());
    };
    let open_diff = |app: &mut App| {
        app.apply(Action::Open);
        wait(app, "fichiers", |app| app.pane.as_ref().is_some_and(|p| matches!(p.files, FilesState::Ready(_))));
        app.apply(Action::Open);
        wait(app, "diff", |app| app.pane.as_ref().and_then(|p| p.diff.as_ref()).is_some_and(|d| d.done));
    };

    let mut app = App::new(Repo::open(&dir).unwrap(), true).unwrap();
    wait(&mut app, "historique", |app| app.log_done);
    draw(&mut app);
    open_diff(&mut app);
    let buffer = draw(&mut app);
    let rgb = |color: Color| matches!(color, Color::Rgb(..));
    let (keyword, _) = colours(&buffer, "fn hello");
    let (comment, _) = colours(&buffer, "// greeting");
    assert!(rgb(keyword) && rgb(comment) && keyword != comment, "{keyword:?} {comment:?}");
    // La modification garde son fond, assombri sous les teintes ; le texte y est coloré aussi.
    // La teinte est lue dans la chaîne : selon le thème, ses guillemets n'en ont pas.
    let (_, removed_bg) = colours(&buffer, "\"hello\"");
    let (_, added_bg) = colours(&buffer, "\"bonjour\"");
    let (removed, added) = (colours(&buffer, "hello\"").0, colours(&buffer, "bonjour\"").0);
    assert!(rgb(removed) && removed == added, "{removed:?} {added:?}");
    assert_eq!((removed_bg, added_bg), (Color::Rgb(63, 0, 1), Color::Rgb(0, 56, 0)));
    // Dans la ligne modifiée, seul le mot qui change porte le fond soutenu.
    assert_eq!(colours(&buffer, "hello\"").1, Color::Rgb(128, 0, 8));
    assert_eq!(colours(&buffer, "bonjour\"").1, Color::Rgb(0, 88, 0));
    assert_eq!(colours(&buffer, "\" ").1, Color::Rgb(63, 0, 1));
    // Les numéros de ligne sont estompés, sur le fond de leur ligne.
    assert_eq!(colours(&buffer, "1 // greeting"), (Color::DarkGray, Color::Reset));
    assert_eq!(colours(&buffer, "3     \"hello\""), (Color::DarkGray, Color::Rgb(63, 0, 1)));
    assert_eq!(colours(&buffer, "3     \"bonjour\""), (Color::DarkGray, Color::Rgb(0, 56, 0)));
    // Le défilement horizontal emporte les teintes avec le texte.
    app.apply(Action::Horizontal(4));
    let buffer = draw(&mut app);
    assert!(!screen(&mut app, 120, 24).contains("pub fn hello"));
    assert_eq!(colours(&buffer, "fn hello").0, keyword);
    assert_eq!(colours(&buffer, "bonjour\"").0, added);

    // Le fichier entier est coloré de la même façon.
    app.apply(Action::Horizontal(-4));
    app.apply(Action::Open);
    wait(&mut app, "fichier entier", |app| app.pane.as_ref().and_then(|p| p.full.as_ref()).is_some_and(|d| d.done));
    let buffer = draw(&mut app);
    assert_eq!(colours(&buffer, "// greeting").0, comment);
    assert_eq!(colours(&buffer, "1 // greeting").0, Color::DarkGray);
    app.apply(Action::GoLevel(1));

    // Sans coloration : le texte et les fonds d'origine.
    app.syntax = false;
    open_diff(&mut app);
    let buffer = draw(&mut app);
    assert_eq!(colours(&buffer, "fn hello").0, Color::Reset);
    assert_eq!(colours(&buffer, "\"bonjour\""), (Color::Reset, Color::Indexed(22)));
    assert_eq!(colours(&buffer, "bonjour\""), (Color::Reset, Color::Indexed(28)));

    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn search_narrows_the_commit_details() {
    // Un commit qui touche trois fichiers ; `notes.txt` change à deux endroits éloignés.
    let dir = std::env::temp_dir().join(format!("gitlog-test-narrow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    let notes = |edit: &dyn Fn(u32) -> String| (1..=40).map(edit).collect::<Vec<_>>().join("\n") + "\n";
    std::fs::write(dir.join("notes.txt"), notes(&|n| format!("ligne {n}"))).unwrap();
    std::fs::write(dir.join("old.txt"), "garde\nAiguille perdue\n").unwrap();
    std::fs::write(dir.join("other.txt"), "rien\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "Base"]);
    let edited = notes(&|n| match n {
        5 => "une aiguille ici".to_string(),
        35 => "autre chose".to_string(),
        n => format!("ligne {n}"),
    });
    std::fs::write(dir.join("notes.txt"), edited).unwrap();
    std::fs::write(dir.join("old.txt"), "garde\n").unwrap();
    std::fs::write(dir.join("other.txt"), "rien\ndu tout\n").unwrap();
    git(&dir, &["commit", "-q", "-am", "Trois fichiers"]);

    let mut app = App::new(Repo::open(&dir).unwrap(), true).unwrap();
    wait(&mut app, "historique", |app| app.log_done);
    screen(&mut app, 120, 24);
    let search = |app: &mut App, keys: &str, query: &str| {
        app.apply(Action::GoLevel(1));
        app.apply(Action::Search);
        for key in keys.chars().chain(query.chars()) {
            app.apply(Action::PromptKey(key));
        }
        app.apply(Action::PromptSubmit);
        wait(app, "recherche", |app| app.log_done);
        assert_eq!(app.meta(0).subject, "Trois fichiers");
    };
    // Ouvre le premier commit trouvé : ses fichiers, puis le nombre de ceux qui sont écartés.
    let files = |app: &mut App| -> (Vec<String>, usize) {
        app.apply(Action::Open);
        wait(app, "fichiers", |app| app.pane.as_ref().is_some_and(|p| matches!(p.files, FilesState::Ready(_))));
        let pane = app.pane.as_ref().unwrap();
        let FilesState::Ready(files) = &pane.files else { unreachable!() };
        (files.iter().map(|f| f.path.clone()).collect(), pane.hidden_files)
    };
    // Ouvre le fichier sélectionné : les en-têtes de hunk et les messages de son diff.
    let hunks = |app: &mut App| -> (usize, Vec<String>) {
        app.apply(Action::Open);
        wait(app, "diff", |app| app.pane.as_ref().and_then(|p| p.diff.as_ref()).is_some_and(|d| d.done));
        let diff = app.pane.as_ref().unwrap().diff.as_ref().unwrap();
        let count = diff.lines.iter().filter(|l| l.kind == crate::diff::LineKind::Hunk).count();
        let metas = diff.lines.iter().filter(|l| l.kind == crate::diff::LineKind::Meta).map(|l| l.text.clone());
        (count, metas.collect())
    };
    let names = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();

    // Sans recherche : tout le commit.
    assert_eq!(files(&mut app), (names(&["notes.txt", "old.txt", "other.txt"]), 0));
    assert_eq!(hunks(&mut app), (2, names(&[])));

    // Recherche dans les hunks : les fichiers concernés, et leurs seuls hunks concernés.
    search(&mut app, "h", "aiguille");
    assert_eq!(files(&mut app), (names(&["notes.txt", "old.txt"]), 1));
    let view = screen(&mut app, 120, 24);
    assert!(view.contains("2 of 3 files match the search"), "{view}");
    assert_eq!(hunks(&mut app), (1, names(&["— 1 other hunk hidden by the search —"])));
    let view = screen(&mut app, 120, 24);
    assert!(view.contains("une aiguille ici") && !view.contains("autre chose"), "{view}");
    // La fenêtre du fichier entier n'est pas triée.
    app.apply(Action::Open);
    wait(&mut app, "fichier entier", |app| app.pane.as_ref().and_then(|p| p.full.as_ref()).is_some_and(|d| d.done));
    let full = app.pane.as_ref().unwrap().full.as_ref().unwrap();
    assert!(full.len() >= 40 && full.change_tops().count() == 2);

    // Par signe : la ligne ajoutée est dans un fichier, la ligne retirée dans un autre.
    search(&mut app, "+h", "AIGUILLE");
    assert_eq!(files(&mut app), (names(&["notes.txt"]), 2));
    assert_eq!(hunks(&mut app).0, 1);
    search(&mut app, "-h", "aiguille");
    assert_eq!(files(&mut app), (names(&["old.txt"]), 2));
    assert_eq!(hunks(&mut app), (1, names(&[])));

    // Recherche par fichier : les fichiers du nom cherché, leur diff en entier.
    search(&mut app, "f", "NOTES");
    assert_eq!(files(&mut app), (names(&["notes.txt"]), 2));
    assert_eq!(hunks(&mut app), (2, names(&[])));

    // Une recherche dans les messages ne trie pas le détail.
    search(&mut app, "m", "trois");
    assert_eq!(files(&mut app).1, 0);
    assert_eq!(hunks(&mut app).0, 2);

    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mouse_selects_code_in_a_diff() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Position;
    use ratatui::style::Color;

    let dir = std::env::temp_dir().join(format!("gitlog-test-select-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    let source = |word: &str| format!("// greeting\npub fn hello() -> &'static str {{\n    \"{word}\"\n}}\n");
    std::fs::write(dir.join("lib.rs"), source("hello")).unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "Base"]);
    // Un fichier modifié, en deux colonnes, et un fichier ajouté, en une seule.
    std::fs::write(dir.join("lib.rs"), source("bonjour")).unwrap();
    let long: String = (1..=30).map(|n| format!("ligne {n}\n")).collect();
    std::fs::write(dir.join("long.txt"), long).unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "Edit"]);

    let draw = |app: &mut App, height: u16| -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(120, height)).unwrap();
        terminal.draw(|frame| ui::draw(frame, app)).unwrap();
        terminal.backend().buffer().clone()
    };
    // Première cellule de `needle` à l'écran, à partir de la colonne `from`.
    let find = |buffer: &Buffer, needle: &str, from: u16| -> Position {
        let needle: Vec<char> = needle.chars().collect();
        for y in 0..buffer.area.height {
            let row: Vec<char> = (from..120).map(|x| buffer[(x, y)].symbol().chars().next().unwrap_or(' ')).collect();
            if let Some(x) = row.windows(needle.len()).position(|w| w == needle.as_slice()) {
                return Position::new(from + x as u16, y);
            }
        }
        panic!("« {} » n'est pas à l'écran", needle.iter().collect::<String>());
    };
    let open_diff = |app: &mut App| {
        app.apply(Action::Open);
        wait(app, "diff", |app| app.pane.as_ref().and_then(|p| p.diff.as_ref()).is_some_and(|d| d.done));
    };

    let mut app = App::new(Repo::open(&dir).unwrap(), true).unwrap();
    wait(&mut app, "historique", |app| app.log_done);
    draw(&mut app, 24);
    app.apply(Action::Open);
    wait(&mut app, "fichiers", |app| app.pane.as_ref().is_some_and(|p| matches!(p.files, FilesState::Ready(_))));
    open_diff(&mut app);
    let buffer = draw(&mut app, 24);
    let code = app.hit.code.expect("le code du diff est repéré");
    let (mid, _) = code.right.expect("deux colonnes");

    // Du « b » de « bonjour » à l'accolade de la ligne suivante, à droite : le
    // texte seul, sans numéros, d'une ligne à l'autre.
    let start = find(&buffer, "bonjour", mid);
    let brace = find(&buffer, "4 }", mid);
    app.apply(Action::SelectStart(start));
    assert!(app.selecting && app.selected_text().is_none(), "un clic seul ne sélectionne rien");
    app.apply(Action::SelectDrag(Position::new(brace.x + 2, brace.y)));
    assert_eq!(app.selected_text().as_deref(), Some("bonjour\"\n}"));
    // La sélection se voit, du côté où elle a commencé seulement.
    let buffer = draw(&mut app, 24);
    let selected = Color::Indexed(18);
    assert_eq!((buffer[(start.x, start.y)].bg, buffer[(brace.x + 2, brace.y)].bg), (selected, selected));
    assert_ne!(buffer[(start.x - 1, start.y)].bg, selected);
    assert!((0..mid).all(|x| buffer[(x, start.y)].bg != selected && buffer[(x, brace.y)].bg != selected));
    // Le pointeur passe à gauche : la sélection reste à droite, ramenée à son bord.
    app.apply(Action::SelectDrag(Position::new(2, start.y)));
    assert_eq!(app.selected_text().as_deref(), Some("    \"b"));

    // À gauche, l'ancienne version. Le défilement horizontal décale les colonnes.
    let old = find(&buffer, "hello\"", 0);
    app.apply(Action::SelectStart(old));
    app.apply(Action::SelectDrag(Position::new(old.x + 4, old.y)));
    assert_eq!(app.selected_text().as_deref(), Some("hello"));
    app.apply(Action::Horizontal(4));
    let buffer = draw(&mut app, 24);
    let old = find(&buffer, "hello\"", 0);
    app.apply(Action::SelectStart(old));
    app.apply(Action::SelectDrag(Position::new(old.x + 5, old.y)));
    assert_eq!(app.selected_text().as_deref(), Some("hello\""));

    // Un clic sans déplacement efface la sélection, sans rien copier.
    app.apply(Action::SelectStart(old));
    app.apply(Action::SelectEnd);
    assert!(!app.selecting && app.selected_text().is_none());

    // Fichier ajouté, sur une colonne, dans une fenêtre basse : tirer jusqu'au
    // bas de l'écran fait défiler le diff, et la sélection s'étend d'autant.
    app.apply(Action::Back { quit: true });
    app.apply(Action::Move(1));
    open_diff(&mut app);
    let buffer = draw(&mut app, 10);
    let first = find(&buffer, "ligne 1", 60);
    app.apply(Action::SelectStart(Position::new(first.x + 6, first.y)));
    for _ in 0..3 {
        app.apply(Action::SelectDrag(Position::new(first.x + 6, 9)));
    }
    let diff = app.pane.as_ref().unwrap().diff.as_ref().unwrap();
    assert_eq!(diff.top, 3);
    // De la fin de « ligne 1 » au même rang dans « ligne 10 », venue sous le pointeur.
    let text = app.selected_text().unwrap();
    assert!(text.starts_with("1\nligne 2\n") && text.ends_with("\nligne 9\nligne 1"), "{text}");
    assert_eq!(text.lines().count(), 10);

    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn remote_branches_are_shortened_and_hidden() {
    let dir = make_repo("remotes");
    // origin/main suit main ; origin/feature est resté en arrière de feature.
    git(&dir, &["update-ref", "refs/remotes/origin/main", "main"]);
    git(&dir, &["update-ref", "refs/remotes/origin/feature", "v0.1.0"]);
    let mut app = App::new(Repo::open(&dir).unwrap(), true).unwrap();
    wait(&mut app, "historique", |app| app.log_done);

    // Sur la ligne de sa branche locale, la branche distante se réduit à « &origin »,
    // accolée à son cartouche : ni flèche, ni espace entre les deux.
    let view = screen(&mut app, 110, 12);
    println!("{view}");
    let line = |view: &str, subject: &str| view.lines().find(|l| l.contains(subject)).unwrap().to_string();
    assert!(line(&view, "Merge de feature").contains("◀ HEAD → main  &origin  Merge de feature"), "{view}");
    assert!(!view.contains("origin/main"));
    // Ailleurs, elle garde son nom entier.
    assert!(line(&view, "Squelette initial").contains("◀ origin/feature "), "{view}");
    assert!(line(&view, "Travail sur la branche").contains("◀ feature  Travail"), "{view}");

    // `o` masque les branches distantes, puis les rend.
    app.apply(Action::ToggleRemotes);
    assert_eq!(app.flash(), Some("Remote branches hidden"));
    let view = screen(&mut app, 110, 12);
    assert!(!view.contains("origin"), "{view}");
    assert!(view.contains("◀ HEAD → main  Merge de feature") && view.contains("◀ v0.1.0 "), "{view}");
    app.apply(Action::ToggleRemotes);
    assert_eq!(app.flash(), Some("Remote branches shown"));
    assert!(screen(&mut app, 110, 12).contains("&origin"));

    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn uncommitted_changes_head_the_graph() {
    let dir = make_repo("changes");
    // Un fichier suivi modifié, un fichier indexé, un fichier non suivi.
    std::fs::write(dir.join("a.txt"), "un\nDEUX\ntrois\nquatre\ncinq\n").unwrap();
    std::fs::write(dir.join("staged.txt"), "indexé\n").unwrap();
    git(&dir, &["add", "staged.txt"]);
    std::fs::write(dir.join("untracked.rs"), "fn main() {}\nfn other() {}\n").unwrap();

    let mut app = App::new(Repo::open(&dir).unwrap(), true).unwrap();
    wait(&mut app, "historique", |app| app.log_done);
    // La première ligne est celle des modifications, reliée à HEAD ; elle n'est pas un commit.
    assert!(app.store.oid(0).is_worktree());
    assert_eq!((app.store.len(), app.commit_count()), (5, 4));
    let view = screen(&mut app, 110, 14);
    println!("{view}");
    let lines: Vec<&str> = view.lines().collect();
    assert!(lines[0].trim_end().ends_with("4 commits"), "{}", lines[0]);
    assert!(lines[2].starts_with(" ● ") && lines[2].trim_end().ends_with("- changes -"), "{}", lines[2]);
    // La puce et le nom sont en gris : plus clair sous la sélection, en retrait sinon.
    let greys = |app: &mut App| {
        let mut terminal = Terminal::new(TestBackend::new(110, 14)).unwrap();
        terminal.draw(|frame| ui::draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        let name = lines[2].chars().position(|c| c == '-').unwrap() as u16;
        (buffer[(1, 2)].fg, buffer[(name, 2)].fg, buffer[(1, 3)].fg)
    };
    use ratatui::style::Color;
    assert_eq!(greys(&mut app), (Color::Gray, Color::Gray, Color::LightBlue));
    app.apply(Action::Move(1));
    assert_eq!(greys(&mut app), (Color::DarkGray, Color::DarkGray, Color::LightBlue));
    app.apply(Action::Move(-1));
    assert!(lines[3].contains("Merge de feature"));
    assert_eq!(app.copy_target(), None, "pas de hash à copier");

    // Elle s'ouvre comme un commit : ses fichiers, puis leur diff.
    app.apply(Action::Open);
    let files_ready = |app: &App| app.pane.as_ref().is_some_and(|p| matches!(p.files, FilesState::Ready(_)));
    wait(&mut app, "fichiers", files_ready);
    let listed = |app: &App| -> Vec<(char, String, Option<u32>)> {
        let FilesState::Ready(files) = &app.pane.as_ref().unwrap().files else { unreachable!() };
        files.iter().map(|f| (f.status, f.path.clone(), f.added)).collect()
    };
    assert_eq!(
        listed(&app),
        [
            ('M', "a.txt".to_string(), Some(1)),
            ('A', "staged.txt".to_string(), Some(1)),
            ('?', "untracked.rs".to_string(), Some(2)),
        ]
    );
    let view = screen(&mut app, 110, 14);
    assert!(view.contains("Uncommitted changes") && view.contains("3 files changed"), "{view}");
    let diff_done = |app: &App| app.pane.as_ref().and_then(|p| p.diff.as_ref()).is_some_and(|d| d.done);
    let texts = |app: &App| -> Vec<String> {
        let diff = app.pane.as_ref().unwrap().diff.as_ref().unwrap();
        diff.lines.iter().filter(|l| l.kind == crate::diff::LineKind::Add).map(|l| l.text.clone()).collect()
    };
    app.apply(Action::Open);
    wait(&mut app, "diff", diff_done);
    assert_eq!(texts(&app), ["cinq"]);
    // Un fichier non suivi se lit en entier, comme un ajout.
    app.apply(Action::Step(1));
    app.apply(Action::Step(1));
    wait(&mut app, "diff", diff_done);
    assert_eq!(texts(&app), ["fn main() {}", "fn other() {}"]);
    assert_eq!(app.copy_target().as_deref(), Some("untracked.rs"));

    // Le dossier de travail bouge : `R` relit la liste ouverte.
    app.apply(Action::GoLevel(2));
    std::fs::remove_file(dir.join("untracked.rs")).unwrap();
    app.apply(Action::Refresh);
    wait(&mut app, "rafraîchissement", |app| app.log_done && files_ready(app));
    assert_eq!((app.depth(), listed(&app).len()), (2, 2));
    assert!(app.store.oid(app.selected).is_worktree());

    // Tout est commité : la ligne disparaît, et son volet avec elle.
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "Tout est rangé"]);
    app.apply(Action::Refresh);
    wait(&mut app, "rafraîchissement", |app| app.log_done);
    assert_eq!((app.depth(), app.store.len(), app.commit_count()), (1, 5, 5));
    assert!(!app.store.oid(0).is_worktree());
    assert_eq!(app.flash(), Some("Refreshed"));

    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn refresh_reloads_the_repository() {
    use crate::git::Head;

    let dir = make_repo("refresh");
    let mut app = App::new(Repo::open(&dir).unwrap(), true).unwrap();
    wait(&mut app, "historique", |app| app.log_done);
    screen(&mut app, 100, 12);
    let before = app.store.len();
    app.apply(Action::Move(2));
    let kept = app.store.oid(2);

    // Le dépôt bouge hors de gitlog : une branche, un commit.
    git(&dir, &["checkout", "-q", "-b", "later"]);
    std::fs::write(dir.join("f.txt"), "plus tard\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "Commit venu d'ailleurs"]);
    assert!(!screen(&mut app, 100, 12).contains("Commit venu d'ailleurs"));

    app.apply(Action::Refresh);
    assert_eq!(app.flash(), Some("Refreshed"));
    // L'ancienne liste reste à l'écran jusqu'à l'arrivée de la nouvelle.
    assert_eq!(app.store.len(), before);
    wait(&mut app, "rafraîchissement", |app| app.log_done);
    assert_eq!(app.store.len(), before + 1);
    assert_eq!(app.repo.head, Head::Branch("later".to_string()));
    // Le commit sélectionné l'est toujours, une ligne plus bas.
    assert_eq!((app.selected, app.store.oid(app.selected)), (3, kept));
    let view = screen(&mut app, 100, 12);
    assert!(view.contains("Commit venu d'ailleurs") && view.contains("later"), "{view}");

    // Plus bas dans l'historique, la ligne sélectionnée ne bouge pas à l'écran.
    screen(&mut app, 100, 6);
    app.apply(Action::Bottom);
    let top = app.top;
    assert!(app.selected == 4 && top > 0, "la liste a défilé : {top}");
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "Encore un"]);
    app.apply(Action::Refresh);
    wait(&mut app, "rafraîchissement", |app| app.log_done);
    assert_eq!((app.selected, app.top), (5, top + 1));
    git(&dir, &["reset", "-q", "--hard", "HEAD~1"]);
    app.apply(Action::Refresh);
    wait(&mut app, "rafraîchissement", |app| app.log_done);
    app.apply(Action::Top);
    app.apply(Action::Move(3));
    screen(&mut app, 100, 12);
    assert_eq!((app.top, app.store.oid(app.selected)), (0, kept));

    // Un volet ouvert le reste, sur le même commit.
    app.apply(Action::Open);
    wait(&mut app, "fichiers", |app| app.pane.as_ref().is_some_and(|p| matches!(p.files, FilesState::Ready(_))));
    app.apply(Action::Refresh);
    wait(&mut app, "rafraîchissement", |app| app.log_done);
    assert_eq!((app.depth(), app.pane.as_ref().map(|p| p.oid)), (2, Some(kept)));
    assert_eq!(app.store.oid(app.selected), kept);
    app.apply(Action::GoLevel(1));

    // Le commit affiché disparaît de l'historique : le volet se referme.
    app.apply(Action::Top);
    app.apply(Action::Open);
    assert_eq!(app.depth(), 2);
    git(&dir, &["reset", "-q", "--hard", "HEAD~1"]);
    app.apply(Action::Refresh);
    wait(&mut app, "rafraîchissement", |app| app.log_done);
    assert_eq!((app.depth(), app.store.len()), (1, before));
    assert_eq!(app.flash(), Some("This commit is not in the graph"));

    // Dans la hiérarchie des branches, les branches sont relues elles aussi.
    app.apply(Action::ToggleHierarchy);
    let ready = |app: &App| app.hierarchy.as_ref().is_some_and(|h| !matches!(h.state, HierState::Loading));
    wait(&mut app, "hiérarchie", ready);
    assert!(!screen(&mut app, 100, 12).contains("toute-neuve"));
    git(&dir, &["branch", "toute-neuve"]);
    app.apply(Action::Refresh);
    wait(&mut app, "hiérarchie", ready);
    assert!(screen(&mut app, 100, 12).contains("toute-neuve"));

    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn search_filters_the_commits() {
    let dir = make_repo("search");
    let mut app = App::new(Repo::open(&dir).unwrap(), true).unwrap();
    wait(&mut app, "historique", |app| app.log_done);
    screen(&mut app, 110, 20);
    let second = app.store.oid(1);

    // `s`, le type, le texte, Entrée : il ne reste que les commits trouvés.
    let search = |app: &mut App, keys: &str, query: &str| -> Vec<String> {
        app.apply(Action::Search);
        for key in keys.chars().chain(query.chars()) {
            app.apply(Action::PromptKey(key));
        }
        app.apply(Action::PromptSubmit);
        assert!(app.prompt.is_none());
        wait(app, "recherche", |app| app.log_done);
        assert_eq!(app.log_error, None);
        (0..app.store.len()).map(|row| app.meta(row).subject).collect()
    };
    assert_eq!(search(&mut app, "m", "SUITE"), ["Suite sur main"]);
    assert_eq!(search(&mut app, "f", "B.tx"), ["Travail sur la branche"]);
    assert_eq!(search(&mut app, "h", "quatre"), ["Travail sur la branche"]);
    assert_eq!(search(&mut app, "+h", "quatre"), ["Travail sur la branche"]);
    assert!(search(&mut app, "-h", "quatre").is_empty());
    assert_eq!(search(&mut app, "-h", "deux"), ["Travail sur la branche"]);
    assert_eq!(search(&mut app, "+h", "trois"), ["Squelette initial"]);
    assert_eq!(search(&mut app, "+h", "deux").len(), 2, "sans casse : « deux » puis « DEUX »");
    assert!(search(&mut app, "m", "a.b*c[").is_empty());
    assert!(search(&mut app, "h", "a.b*c[").is_empty());

    // Le champ reste en bas avec le nombre de résultats ; les lignes n'ont plus de graphe.
    assert_eq!(search(&mut app, "c", &second.to_hex()[..9]).len(), 1);
    assert_eq!(app.store.oid(0), second);
    let found = screen(&mut app, 110, 20);
    println!("{found}");
    let lines: Vec<&str> = found.lines().collect();
    assert!(lines[19].starts_with(" hash: ") && lines[19].trim_end().ends_with("1 match"), "{}", lines[19]);
    assert!(lines[2].starts_with(" ● ") && lines[3].trim().is_empty());

    // Recherche en direct : sans Entrée, la liste suit la frappe après une courte pause.
    app.apply(Action::Search);
    for key in "msuite".chars() {
        app.apply(Action::PromptKey(key));
    }
    assert_eq!(app.filter.as_ref().map(|f| f.query.as_str()), Some(&second.to_hex()[..9]), "pas avant la pause");
    assert_eq!(app.store.len(), 1, "l'ancienne liste reste affichée en attendant");
    wait(&mut app, "recherche en direct", |app| {
        app.log_done && app.filter.as_ref().is_some_and(|f| f.query == "suite")
    });
    assert!(app.prompt.is_some() && !app.stale);
    assert_eq!(app.meta(0).subject, "Suite sur main");
    let live = screen(&mut app, 110, 20);
    let last = live.lines().last().unwrap();
    assert!(last.starts_with(" message: suite") && last.trim_end().ends_with("1 match"), "{last}");
    // Champ vidé : retour au graphe complet, toujours sans quitter la saisie.
    for _ in 0..5 {
        app.apply(Action::PromptBackspace);
    }
    wait(&mut app, "champ vidé", |app| app.log_done && app.filter.is_none());
    assert_eq!(app.store.len(), 4);
    // Échap abandonne : la recherche d'avant l'ouverture du menu revient, sur son commit.
    app.apply(Action::PromptCancel);
    assert!(app.prompt.is_none());
    wait(&mut app, "recherche rétablie", |app| app.log_done);
    assert_eq!(app.filter.as_ref().map(|f| f.query.as_str()), Some(&second.to_hex()[..9]));
    assert_eq!((app.store.len(), app.store.oid(app.selected)), (1, second));

    // Le sous-menu attend un type ; `+` puis `h` ouvre le champ des lignes ajoutées.
    app.apply(Action::Search);
    let menu = screen(&mut app, 110, 20);
    println!("{menu}");
    assert!(menu.contains("╭─ Search ") && menu.contains("commit message") && menu.contains("-h"));
    app.apply(Action::PromptKey('+'));
    app.apply(Action::PromptKey('m'));
    assert!(matches!(app.prompt, Some(crate::app::Prompt::Kind { sign: Some('+') })));
    app.apply(Action::PromptKey('h'));
    app.apply(Action::PromptKey('x'));
    let typing = screen(&mut app, 110, 20);
    let last = typing.lines().last().unwrap();
    assert!(last.starts_with(" +hunk: x") && last.trim_end().ends_with("Enter to search"), "{last}");
    assert_eq!(app.filter.as_ref().map(|f| f.kind), Some(crate::git::SearchKind::Hash), "les hunks attendent Entrée");
    // Échap abandonne la saisie et garde la recherche appliquée.
    app.apply(Action::PromptCancel);
    assert!(app.prompt.is_none() && app.filter.is_some());

    // q sort de la recherche sans quitter, sur le même commit dans le graphe complet.
    app.apply(Action::Back { quit: true });
    assert!(app.filter.is_none() && !app.should_quit);
    wait(&mut app, "retour au graphe", |app| app.log_done);
    assert_eq!((app.store.len(), app.selected), (4, 1));
    app.apply(Action::Back { quit: true });
    assert!(app.should_quit);

    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn commit_styles_link_the_commits() {
    let dir = make_repo("styles");
    let mut app = App::new(Repo::open(&dir).unwrap(), false).unwrap();
    wait(&mut app, "historique", |app| app.log_done);
    let graph = |app: &mut App| -> Vec<String> {
        screen(app, 110, 12).lines().skip(2).take(4).map(|l| l[..l.find(' ').map_or(0, |_| 12)].to_string()).collect()
    };

    // Par défaut : une puce par commit.
    assert_eq!(app.node_style, NodeStyle::Dots);
    assert!(graph(&mut app)[0].starts_with(" ◉─╮"));

    // Chaque colonne active porte un fond d'une cellule, pas l'intervalle entre deux colonnes.
    let mut terminal = Terminal::new(TestBackend::new(110, 12)).unwrap();
    terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    let reset = buffer[(0, 3)].bg;
    assert_eq!(buffer[(1, 3)].symbol(), "●");
    assert_eq!(buffer[(3, 3)].symbol(), "│");
    assert_ne!(buffer[(1, 3)].bg, reset);
    assert_eq!(buffer[(2, 3)].bg, reset);
    assert_eq!(buffer[(3, 3)].bg, buffer[(1, 3)].bg);
    assert_ne!(buffer[(1, 2)].bg, buffer[(2, 2)].bg, "la bande reste visible sur la ligne sélectionnée");

    // Rail : la ligne ne s'interrompt jamais, la puce est accrochée à droite.
    app.apply(Action::CycleNodes);
    assert_eq!(app.node_style, NodeStyle::Rail);
    let rail = graph(&mut app);
    println!("{rail:#?}");
    assert!(rail[0].starts_with(" ╭◉╮"), "{}", rail[0]);
    assert!(rail[1].starts_with(" ├●│"), "{}", rail[1]);
    assert!(rail[2].starts_with(" │ ├●"), "{}", rail[2]);
    assert!(rail[3].starts_with(" ╰●╯"), "{}", rail[3]);

    app.apply(Action::CycleNodes);
    assert_eq!(app.node_style, NodeStyle::Dots);
    assert!(graph(&mut app)[0].starts_with(" ◉─╮"));

    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn head_stands_out_in_the_graph() {
    let dir = make_repo("head");
    let mut app = App::new(Repo::open(&dir).unwrap(), true).unwrap();
    wait(&mut app, "historique", |app| app.log_done);
    // Fonds de la marge, de la colonne du graphe et du message, pour une ligne.
    let bgs = |app: &mut App, y: u16| {
        let mut terminal = Terminal::new(TestBackend::new(110, 12)).unwrap();
        terminal.draw(|frame| ui::draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        (buffer[(0, y)].bg, buffer[(1, y)].bg, buffer[(60, y)].bg)
    };

    // La branche courante : puce pleine cerclée, étiquette « HEAD → nom ».
    let view = screen(&mut app, 110, 12);
    let lines: Vec<&str> = view.lines().collect();
    assert!(lines[2].starts_with(" ◉─╮") && lines[2].contains("◀ HEAD → main  Merge"), "{view}");
    assert_eq!(view.matches('◉').count(), 1, "{view}");
    assert!(lines[4].contains("◀ feature  Travail"), "{view}");

    // Hors sélection, sa ligne garde un fond propre, distinct de celui de la sélection.
    let selected = bgs(&mut app, 2);
    app.apply(Action::Move(1));
    let (head, other) = (bgs(&mut app, 2), bgs(&mut app, 4));
    assert_eq!(head.0, head.2, "le fond couvre toute la ligne");
    assert_ne!(head.0, other.0);
    assert_ne!(head.0, selected.0);
    assert_eq!(bgs(&mut app, 3).0, selected.0);
    assert_ne!(head.1, head.0, "la bande du graphe reste visible");

    // HEAD détaché : l'étiquette « HEAD » seule, la puce et le fond suivent.
    git(&dir, &["checkout", "-q", "--detach", "feature"]);
    app.apply(Action::Refresh);
    wait(&mut app, "relecture", |app| app.log_done);
    let view = screen(&mut app, 110, 12);
    let line = view.lines().find(|l| l.contains("Travail sur la branche")).unwrap();
    assert!(line.contains("◉") && line.contains("◀ HEAD  ◀ feature  Travail"), "{view}");
    assert!(!view.contains("HEAD →") && view.matches('◉').count() == 1, "{view}");

    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn branch_hierarchy_view() {
    let dir = make_repo("hierarchy");
    // main ← wip ← wip-suite ; feature est déjà fusionnée dans main.
    git(&dir, &["checkout", "-q", "-b", "wip"]);
    std::fs::write(dir.join("d.txt"), "brouillon\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "Brouillon"]);
    git(&dir, &["checkout", "-q", "-b", "wip-suite"]);
    std::fs::write(dir.join("e.txt"), "suite\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "Suite du brouillon"]);
    git(&dir, &["checkout", "-q", "main"]);

    let mut app = App::new(Repo::open(&dir).unwrap(), true).unwrap();
    wait(&mut app, "historique", |app| app.log_done);
    screen(&mut app, 100, 12);

    app.apply(Action::ToggleHierarchy);
    wait(&mut app, "hiérarchie", |app| {
        app.hierarchy.as_ref().is_some_and(|h| !matches!(h.state, HierState::Loading))
    });
    let view = screen(&mut app, 100, 12);
    println!("{view}");
    let lines: Vec<&str> = view.lines().collect();
    assert!(lines[0].contains("branch hierarchy") && lines[0].contains("4 branches"));
    assert!(lines[2].contains("◀ main ") && lines[2].contains("← HEAD"));
    assert!(lines[3].contains("├─ ◀ feature ") && lines[3].contains("merged"));
    assert!(lines[4].contains("╰─ ◀ wip ") && lines[4].contains("↑1"));
    assert!(lines[5].contains("   ╰─ ◀ wip-suite ") && lines[5].contains("↑1"));
    // Aucun commit dans cette vue.
    assert!(!view.contains("Brouillon") && !view.contains("Merge de feature"));

    // Entrée sur une branche ramène au graphe, sur son dernier commit.
    app.apply(Action::Bottom);
    app.apply(Action::Open);
    assert!(app.hierarchy.is_none());
    let meta = app.meta(app.selected);
    assert_eq!(meta.subject, "Suite du brouillon");

    // h ouvre et referme la vue.
    app.apply(Action::ToggleHierarchy);
    assert!(app.hierarchy.is_some());
    app.apply(Action::ToggleHierarchy);
    assert!(app.hierarchy.is_none());

    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}
