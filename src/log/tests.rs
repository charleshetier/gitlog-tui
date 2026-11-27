//! Test de bout en bout : un vrai dépôt temporaire, le vrai `git`, un terminal simulé.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::kit::Tool;
use crate::log::app::{Action, App, FilesState, HierState, NodeStyle};
use crate::log::git::Repo;
use crate::log::ui;

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
    // Un tag porte son icône, sans flèche : il ne pointe rien.
    assert!(level1.contains("◀ feature ") && level1.contains("◇ v0.1.0 ") && !level1.contains("◀ v0.1.0"));
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

    // Au clavier : n et N (Maj+n) pour les modifications, Tab et Maj+Tab pour le
    // voisin du niveau au-dessus ; p n'est plus une touche.
    {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let press = |code: KeyCode, modifiers| crate::log::keys::on_key(&app, KeyEvent::new(code, modifiers));
        let key = |c: char, modifiers| press(KeyCode::Char(c), modifiers);
        assert!(matches!(key('n', KeyModifiers::NONE), Some(Action::Step(1))));
        assert!(matches!(key('N', KeyModifiers::SHIFT), Some(Action::Step(-1))));
        assert!(matches!(press(KeyCode::Tab, KeyModifiers::NONE), Some(Action::Sibling(1))));
        assert!(matches!(press(KeyCode::BackTab, KeyModifiers::SHIFT), Some(Action::Sibling(-1))));
        assert!(key('p', KeyModifiers::NONE).is_none());
        // h et l doublent les flèches ; HEAD est sur H, le repli des lignes sur z.
        assert!(matches!(key('h', KeyModifiers::NONE), Some(Action::Horizontal(-4))));
        assert!(matches!(key('l', KeyModifiers::NONE), Some(Action::Horizontal(4))));
        assert!(matches!(key('H', KeyModifiers::SHIFT), Some(Action::GotoHead)));
        assert!(matches!(key('z', KeyModifiers::NONE), Some(Action::ToggleWrap)));
        assert!(key('w', KeyModifiers::NONE).is_none());
    }

    // Tab / Maj+Tab dans la liste des fichiers : commit suivant, précédent.
    app.apply(Action::Sibling(1));
    assert_eq!((app.depth(), app.selected), (2, 1));
    assert_eq!(app.pane.as_ref().map(|p| p.oid), Some(app.store.oid(1)));
    app.apply(Action::Sibling(-1));
    app.apply(Action::Sibling(-1));
    assert_eq!((app.depth(), app.selected), (2, 0));
    assert_eq!(app.flash(), Some("First commit"));
    // `n` n'y fait rien : il n'y a pas de code à parcourir.
    app.apply(Action::Step(1));
    assert_eq!((app.depth(), app.selected), (2, 0));

    // Tab / Maj+Tab dans un diff : fichier suivant, précédent, sans sortir du commit.
    wait(&mut app, "fichiers", files_ready);
    app.apply(Action::Open);
    assert_eq!(diff_file(&app), Some(0));
    app.apply(Action::Sibling(1));
    assert_eq!(diff_file(&app), Some(1));
    assert_eq!(app.copy_target().as_deref(), Some("b.txt"));
    app.apply(Action::Sibling(1));
    assert_eq!(diff_file(&app), Some(1));
    assert_eq!(app.flash(), Some("Last file of the commit"));
    app.apply(Action::Sibling(-1));
    assert_eq!(diff_file(&app), Some(0));

    // Depuis le fichier entier, Tab passe au fichier voisin sans quitter la fenêtre.
    wait(&mut app, "diff", |app| app.pane.as_ref().and_then(|p| p.diff.as_ref()).is_some_and(|d| d.done));
    app.apply(Action::Open);
    assert_eq!(app.depth(), 4);
    app.apply(Action::Sibling(1));
    wait(&mut app, "fichier entier du voisin", |app| app.pane.as_ref().and_then(|p| p.full.as_ref()).is_some_and(|d| d.file == 1 && d.done));
    assert_eq!(app.depth(), 4);
    app.apply(Action::Back { quit: false });
    assert_eq!((app.depth(), diff_file(&app)), (3, Some(1)));
    app.apply(Action::Sibling(-1));
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
    assert!(help.contains("Keyboard shortcuts") && help.contains("Copy the commit hash, the file path"));
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
    // Dans le diff déjà, n et N vont d'une modification à l'autre.
    let diff = |app: &App| {
        let view = app.pane.as_ref().unwrap().diff.as_ref().unwrap();
        (view.top, view.change_tops().collect::<Vec<_>>())
    };
    let hunk_tops = diff(&app).1;
    assert_eq!(hunk_tops.len(), 3);
    app.apply(Action::Step(1));
    assert_eq!(diff(&app).0, hunk_tops[1]);
    app.apply(Action::Step(1));
    app.apply(Action::Step(1));
    assert_eq!((diff(&app).0, app.flash()), (hunk_tops[2], Some("Last change")));
    app.apply(Action::Step(-1));
    assert_eq!((diff(&app).0, app.depth()), (hunk_tops[1], 3));

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
        let count = diff.lines.iter().filter(|l| l.kind == crate::log::diff::LineKind::Hunk).count();
        let metas = diff.lines.iter().filter(|l| l.kind == crate::log::diff::LineKind::Meta).map(|l| l.text.clone());
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
    let code = app.hit.code.clone().expect("le code du diff est repéré");
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
fn slash_searches_the_code_of_a_diff() {
    use ratatui::buffer::Buffer;
    use ratatui::style::Color;

    let dir = std::env::temp_dir().join(format!("gitlog-test-find-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    let source = |word: &str| format!("// greeting\npub fn hello() -> &'static str {{\n    \"{word}\"\n}}\n");
    std::fs::write(dir.join("lib.rs"), source("hello")).unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "Base"]);
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
    let text = |buffer: &Buffer, y: u16| -> String { (0..120).map(|x| buffer[(x, y)].symbol()).collect() };
    // Fonds des cellules où commence chaque « needle » d'une ligne de l'écran.
    let backgrounds = |buffer: &Buffer, y: u16, needle: &str| -> Vec<Color> {
        let row: Vec<char> = text(buffer, y).chars().collect();
        let needle: Vec<char> = needle.chars().collect();
        let starts = (0..row.len()).filter(|&x| row[x..].starts_with(&needle));
        starts.map(|x| buffer[(x as u16, y)].bg).collect()
    };
    let open_diff = |app: &mut App| {
        app.apply(Action::Open);
        wait(app, "diff", |app| app.pane.as_ref().and_then(|p| p.diff.as_ref()).is_some_and(|d| d.done));
    };
    let type_in = |app: &mut App, query: &str| query.chars().for_each(|key| app.apply(Action::PromptKey(key)));
    let (current, other) = (Color::Indexed(208), Color::Indexed(179));

    let mut app = App::new(Repo::open(&dir).unwrap(), true).unwrap();
    wait(&mut app, "historique", |app| app.log_done);
    draw(&mut app, 24);
    // Hors d'un diff, `/` ne fait rien.
    app.apply(Action::Find);
    assert!(app.prompt.is_none());
    app.apply(Action::Open);
    wait(&mut app, "fichiers", |app| app.pane.as_ref().is_some_and(|p| matches!(p.files, FilesState::Ready(_))));
    open_diff(&mut app);
    draw(&mut app, 24);

    // La recherche suit la frappe. « hello » : la ligne commune, comptée une fois
    // mais marquée des deux côtés, puis la ligne retirée, à gauche.
    app.apply(Action::Find);
    type_in(&mut app, "hello");
    assert_eq!(app.code_find(), Some(("hello", true, Some(0), 2)));
    let buffer = draw(&mut app, 24);
    assert!(text(&buffer, 23).contains("find: hello") && text(&buffer, 23).contains("match 1/2"), "{}", text(&buffer, 23));
    let line = |buffer: &Buffer, needle: &str| (0..24).find(|&y| text(buffer, y).contains(needle)).unwrap();
    assert_eq!(backgrounds(&buffer, line(&buffer, "fn hello"), "hello"), [current, current]);
    assert_eq!(backgrounds(&buffer, line(&buffer, "\"hello\""), "hello"), [other]);

    // Entrée garde la recherche ; `n` et `N` parcourent les occurrences, en boucle,
    // au lieu d'aller d'une modification à l'autre.
    app.apply(Action::PromptSubmit);
    assert_eq!(app.code_find(), Some(("hello", false, Some(0), 2)));
    app.apply(Action::Step(1));
    assert_eq!(app.code_find().unwrap().2, Some(1));
    let buffer = draw(&mut app, 24);
    assert!(text(&buffer, 23).contains("match 2/2"));
    assert_eq!(backgrounds(&buffer, line(&buffer, "\"hello\""), "hello"), [current]);
    app.apply(Action::Step(1));
    assert_eq!(app.code_find().unwrap().2, Some(0));
    app.apply(Action::Step(-1));
    assert_eq!(app.code_find().unwrap().2, Some(1));
    assert_eq!(app.pane.as_ref().unwrap().diff.as_ref().unwrap().file, 0);

    // Retour quitte d'abord la recherche, puis le diff.
    app.apply(Action::Back { quit: false });
    assert_eq!((app.depth(), app.code_find()), (3, None));
    assert!(!text(&draw(&mut app, 24), 23).contains("find:"));

    // Une majuscule rend la recherche sensible à la casse. Sans résultat, Entrée
    // ne garde rien ; Échap rend la vue et la recherche d'avant.
    app.apply(Action::Find);
    type_in(&mut app, "Hello");
    assert_eq!(app.code_find(), Some(("Hello", true, None, 0)));
    assert!(text(&draw(&mut app, 24), 23).contains("no match"));
    app.apply(Action::PromptSubmit);
    assert_eq!((app.code_find(), app.flash()), (None, Some("No match: Hello")));
    app.apply(Action::Find);
    type_in(&mut app, "bonjour");
    app.apply(Action::PromptSubmit);
    app.apply(Action::Find);
    type_in(&mut app, "fn");
    assert_eq!(app.code_find().unwrap().0, "fn");
    app.apply(Action::PromptCancel);
    assert_eq!(app.code_find(), Some(("bonjour", false, Some(0), 1)));

    // Le fichier entier a sa propre recherche.
    app.apply(Action::Open);
    wait(&mut app, "fichier entier", |app| app.pane.as_ref().and_then(|p| p.full.as_ref()).is_some_and(|d| d.done));
    draw(&mut app, 24);
    assert_eq!(app.code_find(), None);
    app.apply(Action::Find);
    type_in(&mut app, "GREETING");
    assert_eq!(app.code_find().unwrap().3, 0);
    app.apply(Action::PromptBackspace);
    app.apply(Action::PromptCancel);
    app.apply(Action::Find);
    type_in(&mut app, "greeting");
    app.apply(Action::PromptSubmit);
    assert_eq!(app.code_find(), Some(("greeting", false, Some(0), 1)));
    app.apply(Action::Back { quit: false });
    assert_eq!(app.depth(), 4);
    app.apply(Action::Back { quit: false });
    assert_eq!((app.depth(), app.code_find().map(|find| find.0)), (3, Some("bonjour")));

    // Fichier ajouté, plus long que la fenêtre : la vue suit l'occurrence courante.
    app.apply(Action::Back { quit: false });
    app.apply(Action::Back { quit: false });
    app.apply(Action::Move(1));
    open_diff(&mut app);
    draw(&mut app, 10);
    app.apply(Action::Find);
    type_in(&mut app, "ligne 2");
    app.apply(Action::PromptSubmit);
    // « ligne 2 », puis « ligne 20 » à « ligne 29 ».
    assert_eq!(app.code_find(), Some(("ligne 2", false, Some(0), 11)));
    let top = |app: &App| app.pane.as_ref().unwrap().diff.as_ref().unwrap().top;
    assert_eq!(top(&app), 0);
    for _ in 0..6 {
        app.apply(Action::Step(1));
    }
    let buffer = draw(&mut app, 10);
    assert!(top(&app) > 0 && app.code_find().unwrap().2 == Some(6));
    let y = (0..9).find(|&y| text(&buffer, y).contains("ligne 25")).expect("l'occurrence courante est à l'écran");
    assert_eq!(backgrounds(&buffer, y, "ligne 25"), [current]);
    app.apply(Action::Step(-1));
    app.apply(Action::Step(-1));
    assert_eq!(app.code_find().unwrap().2, Some(4));

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

    // `b` `o` masque les branches distantes, puis les rend.
    app.apply(Action::ToggleRemotes);
    assert_eq!(app.flash(), Some("Remote branches hidden"));
    wait(&mut app, "sans les distantes", |app| app.log_done);
    let view = screen(&mut app, 110, 12);
    assert!(!view.contains("origin") && view.contains("· no remotes"), "{view}");
    assert!(view.contains("◀ HEAD → main  Merge de feature") && view.contains("◇ v0.1.0 "), "{view}");
    app.apply(Action::ToggleRemotes);
    assert_eq!(app.flash(), Some("Local and remote branches"));
    wait(&mut app, "toutes", |app| app.log_done);
    let view = screen(&mut app, 110, 12);
    assert!(view.contains("&origin") && !view.contains("no remotes"), "{view}");

    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn gitchange_shows_the_uncommitted_changes() {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let dir = make_repo("changes");
    // Un fichier suivi modifié, un autre indexé, un fichier non suivi.
    std::fs::write(dir.join("a.txt"), "un\ndeux modifié\nsix\n").unwrap();
    std::fs::write(dir.join("b.txt"), "branche, puis indexé\n").unwrap();
    git(&dir, &["add", "b.txt"]);
    std::fs::write(dir.join("nouveau.txt"), "tout neuf\n").unwrap();

    let mut app = App::open(Repo::open(&dir).unwrap(), true, Tool::Changes).unwrap();
    let files_ready = |app: &App| app.pane.as_ref().is_some_and(|p| matches!(p.files, FilesState::Ready(_)));
    let diff_done = |app: &App| app.pane.as_ref().and_then(|p| p.diff.as_ref()).is_some_and(|d| d.done);
    let key = |app: &App, c: char| crate::log::keys::on_key(app, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    wait(&mut app, "fichiers", files_ready);

    // Pas de graphe : la liste des fichiers d'emblée, sur toute la largeur, au niveau 1.
    let view = screen(&mut app, 110, 14);
    let lines: Vec<&str> = view.lines().collect();
    assert!(lines[0].contains("HEAD on main") && lines[0].trim_end().ends_with(" 1"), "{view}");
    assert!(!lines[0].contains("commit") && !view.contains("Merge de feature") && !view.contains('●'), "{view}");
    assert!(lines[2].starts_with(" Uncommitted changes") && lines[4].contains("3 files changed"), "{view}");
    assert!(lines[5].contains("a.txt") && lines[6].contains("b.txt") && lines[7].contains("nouveau.txt"), "{view}");
    assert!(lines.iter().all(|line| !line.contains('│')), "{view}");
    assert_eq!(app.store.len(), 0);

    // Ce qui tient au graphe n'a pas d'effet : recherche, branches, HEAD, style.
    for c in ['s', 'b', 'h', 'S', 'n', 'N'] {
        let action = key(&app, c).unwrap();
        app.apply(action);
    }
    assert!(app.prompt.is_none() && app.hierarchy.is_none() && app.picker.is_none() && app.depth() == 2);
    // Échap ne sort pas de la liste : c'est `q` qui quitte.
    app.apply(Action::Back { quit: false });
    assert!(!app.should_quit && app.pane.is_some());

    // Niveau 2 : le diff du fichier, en deux colonnes ; niveau 3 : le fichier entier.
    app.apply(Action::Open);
    wait(&mut app, "diff", diff_done);
    let view = screen(&mut app, 110, 14);
    assert!(view.lines().next().unwrap().trim_end().ends_with(" 1   2") && view.contains("deux modifié"), "{view}");
    assert!(view.contains("a.txt") && view.contains('│'), "{view}");
    app.apply(Action::Open);
    wait(&mut app, "fichier entier", |app| app.pane.as_ref().and_then(|p| p.full.as_ref()).is_some_and(|d| d.done));
    assert_eq!(app.depth(), 4);
    assert!(screen(&mut app, 110, 14).lines().next().unwrap().trim_end().ends_with(" 1   2   3"));
    // Les touches de niveau suivent ceux de l'outil : 1 la liste, 2 le diff, 3 le fichier.
    app.apply(Action::GoLevel(2));
    assert_eq!(app.depth(), 3);
    app.apply(Action::GoLevel(3));
    assert_eq!(app.depth(), 4);
    app.apply(Action::GoLevel(1));
    assert!(app.depth() == 2 && app.pane.is_some());
    // `Tab` passe au fichier suivant depuis un diff : le fichier indexé, puis le fichier non suivi.
    app.apply(Action::GoLevel(2));
    wait(&mut app, "diff", diff_done);
    app.apply(Action::Sibling(1));
    wait(&mut app, "diff", diff_done);
    assert!(screen(&mut app, 110, 14).contains("branche, puis indexé"));
    app.apply(Action::Sibling(1));
    wait(&mut app, "diff", diff_done);
    assert!(screen(&mut app, 110, 14).contains("tout neuf"));

    // `R` relit les modifications : un fichier remis en l'état quitte la liste.
    git(&dir, &["checkout", "-q", "--", "a.txt"]);
    app.apply(Action::Refresh);
    assert_eq!(app.flash(), Some("Refreshed"));
    wait(&mut app, "fichiers", files_ready);
    let view = screen(&mut app, 110, 14);
    assert!(view.contains("2 files changed") && !view.contains("a.txt") && app.store.len() == 0, "{view}");
    // Plus rien à commiter : la liste le dit.
    git(&dir, &["checkout", "-q", "--", "b.txt"]);
    git(&dir, &["reset", "-q", "--hard"]);
    std::fs::remove_file(dir.join("nouveau.txt")).unwrap();
    app.apply(Action::Refresh);
    wait(&mut app, "fichiers", files_ready);
    assert!(screen(&mut app, 110, 14).contains("No changes: the working tree matches HEAD."));
    app.apply(Action::Open);
    assert_eq!(app.depth(), 2);

    // L'aide ne garde que ce qui vaut ici ; `q` quitte depuis la liste.
    app.apply(Action::ToggleHelp);
    let help = screen(&mut app, 110, 30);
    assert!(help.contains("Go to a level: files, diff, whole file") && !help.contains("Branch hierarchy"), "{help}");
    app.apply(Action::ToggleHelp);
    app.apply(Action::Back { quit: true });
    assert!(app.should_quit);

    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn gitchange_works_before_the_first_commit() {
    let dir = std::env::temp_dir().join(format!("gitlog-test-unborn-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("indexé.txt"), "premier\n").unwrap();
    git(&dir, &["add", "."]);
    std::fs::write(dir.join("libre.txt"), "second\n").unwrap();

    let mut app = App::open(Repo::open(&dir).unwrap(), true, Tool::Changes).unwrap();
    wait(&mut app, "fichiers", |app| app.pane.as_ref().is_some_and(|p| !matches!(p.files, FilesState::Loading)));
    let view = screen(&mut app, 110, 12);
    assert!(view.contains("2 files changed") && view.contains("indexé.txt") && view.contains("libre.txt"), "{view}");
    app.apply(Action::Open);
    wait(&mut app, "diff", |app| app.pane.as_ref().and_then(|p| p.diff.as_ref()).is_some_and(|d| d.done));
    assert!(screen(&mut app, 110, 12).contains("premier"));

    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn w_wraps_long_lines() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Position;
    use ratatui::style::Color;

    let dir = std::env::temp_dir().join(format!("gitlog-test-wrap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    let words = |tail: &str| (1..=40).map(|n| format!("mot{n}")).chain([tail.to_string()]).collect::<Vec<_>>().join(" ");
    std::fs::write(dir.join("long.txt"), format!("court\n{}\nfin de fichier\n", words("QUEUE"))).unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "Base"]);
    std::fs::write(dir.join("long.txt"), format!("court\n{}\nfin de fichier\n", words("QUEUE_DROITE"))).unwrap();
    std::fs::write(dir.join("ajout.txt"), format!("{}\nseconde ligne\n", words("QUEUE_AJOUT"))).unwrap();
    git(&dir, &["add", "."]);
    let subject = format!("Message {} FINMESSAGE", (1..=30).map(|n| format!("partie{n}")).collect::<Vec<_>>().join(" "));
    git(&dir, &["commit", "-q", "-m", &subject]);
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "Dernier"]);

    let draw = |app: &mut App, height: u16| -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(120, height)).unwrap();
        terminal.draw(|frame| ui::draw(frame, app)).unwrap();
        terminal.backend().buffer().clone()
    };
    let text = |buffer: &Buffer, y: u16| -> String { (0..120).map(|x| buffer[(x, y)].symbol()).collect() };
    let line = |buffer: &Buffer, needle: &str| (0..buffer.area.height).find(|&y| text(buffer, y).contains(needle));
    let column = |buffer: &Buffer, y: u16, needle: &str| text(buffer, y).find(needle).map(|byte| text(buffer, y)[..byte].chars().count() as u16);

    let mut app = App::new(Repo::open(&dir).unwrap(), true).unwrap();
    wait(&mut app, "historique", |app| app.log_done);

    // Le graphe : un message trop long est coupé, puis replié sous lui-même.
    let cut = draw(&mut app, 12);
    assert!(line(&cut, "FINMESSAGE").is_none() && line(&cut, "Base") == Some(4));
    app.apply(Action::ToggleWrap);
    assert_eq!(app.flash(), Some("Long lines wrapped"));
    let wrapped = draw(&mut app, 12);
    let (first, last, base) = (line(&wrapped, "Message partie1").unwrap(), line(&wrapped, "FINMESSAGE").unwrap(), line(&wrapped, "Base").unwrap());
    assert!(first == 3 && last > first && base == last + 1, "{first} {last} {base}");
    // Sous la première rangée : le trait de la branche, pas de hash ni d'auteur, le texte aligné.
    let start = column(&wrapped, first, "Message").unwrap();
    for y in first + 1..=last {
        let row = text(&wrapped, y);
        assert!(row.starts_with(" │") && !row.contains("·"), "{row}");
        assert_eq!(row.chars().take_while(|c| !c.is_alphanumeric()).count() as u16, start, "{row}");
    }
    // Le repli se fait entre deux mots.
    assert!((first + 1..last).all(|y| text(&wrapped, y).trim_end().ends_with(char::is_numeric)));
    // Chaque rangée de l'écran sait de quelle ligne du graphe elle vient.
    let rows = &app.hit.graph_rows;
    assert_eq!(rows[..2], [0, 1]);
    assert!(rows[1..=(last - first) as usize + 1].iter().all(|&row| row == 1) && rows[(base - 2) as usize] == 2, "{rows:?}");
    // La sélection reste à l'écran, même sous une ligne qui en occupe plusieurs.
    app.apply(Action::Bottom);
    let low = draw(&mut app, 2 + (last - first) + 1);
    assert!(line(&low, "Base").is_some() && app.top > 0, "{}", text(&low, 2));
    app.apply(Action::Top);

    // Le diff en deux colonnes : la longue ligne se replie de chaque côté.
    app.apply(Action::ToggleWrap);
    assert_eq!(app.flash(), Some("Long lines cut"));
    app.apply(Action::Move(1));
    app.apply(Action::Open);
    wait(&mut app, "fichiers", |app| app.pane.as_ref().is_some_and(|p| matches!(p.files, FilesState::Ready(_))));
    app.apply(Action::Move(1));
    app.apply(Action::Open);
    wait(&mut app, "diff", |app| app.pane.as_ref().and_then(|p| p.diff.as_ref()).is_some_and(|d| d.done));
    let cut = draw(&mut app, 30);
    assert!(line(&cut, "QUEUE").is_none() && line(&cut, "fin de fichier").is_some());
    app.apply(Action::ToggleWrap);
    let wrapped = draw(&mut app, 30);
    let (start, old_end, new_end) = (line(&wrapped, "mot1 ").unwrap(), line(&wrapped, "QUEUE ").unwrap(), line(&wrapped, "QUEUE_DROITE").unwrap());
    let after = line(&wrapped, "fin de fichier").unwrap();
    assert!(old_end > start && new_end >= old_end && after == new_end + 1, "{start} {old_end} {new_end} {after}");
    // Le numéro de ligne n'est écrit qu'à la première rangée.
    assert!(text(&wrapped, start).contains("  2 mot1") && !text(&wrapped, start + 1).contains("  2 "));
    let code = app.hit.code.clone().unwrap();
    assert!(code.wrap && code.lines[(start - code.area.y) as usize].1 == 0 && code.lines[(new_end - code.area.y) as usize].1 > 0);
    // Une sélection suit le texte d'une rangée à la suivante.
    let (_, right) = code.right.unwrap();
    app.apply(Action::SelectStart(Position::new(right, start)));
    app.apply(Action::SelectDrag(Position::new(right + 3, start + 1)));
    let selected = app.selected_text().unwrap();
    let width = (119 - right) as usize;
    assert!(words("QUEUE_DROITE").starts_with(&selected) && selected.chars().count() == width + 4, "{selected}");
    app.apply(Action::SelectEnd);
    // La recherche surligne une occurrence sur la rangée où elle se trouve.
    app.apply(Action::Find);
    "DROITE".chars().for_each(|key| app.apply(Action::PromptKey(key)));
    app.apply(Action::PromptSubmit);
    let found = draw(&mut app, 30);
    let y = line(&found, "QUEUE_DROITE").unwrap();
    let x = column(&found, y, "DROITE").unwrap();
    assert_eq!(found[(x, y)].bg, Color::Indexed(208));
    app.apply(Action::Back { quit: false });

    // Le fichier ajouté, sur une seule colonne.
    app.apply(Action::Back { quit: false });
    app.apply(Action::Move(-1));
    app.apply(Action::Open);
    wait(&mut app, "diff", |app| app.pane.as_ref().and_then(|p| p.diff.as_ref()).is_some_and(|d| d.done));
    let wrapped = draw(&mut app, 30);
    let (start, end) = (line(&wrapped, "mot1 ").unwrap(), line(&wrapped, "QUEUE_AJOUT").unwrap());
    assert!(end > start && line(&wrapped, "seconde ligne") == Some(end + 1));
    app.apply(Action::ToggleWrap);
    assert!(line(&draw(&mut app, 30), "QUEUE_AJOUT").is_none());

    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn h_jumps_to_head_without_opening_it() {
    let dir = make_repo("goto-head");
    // HEAD loin du haut : une branche plus récente, avec de quoi remplir l'écran.
    git(&dir, &["checkout", "-q", "-b", "longue"]);
    for n in 1..=30 {
        git(&dir, &["commit", "-q", "--allow-empty", "-m", &format!("Suite {n}")]);
    }
    git(&dir, &["checkout", "-q", "main"]);
    let mut app = App::new(Repo::open(&dir).unwrap(), true).unwrap();
    wait(&mut app, "historique", |app| app.log_done);
    let view = screen(&mut app, 110, 12);
    assert!(view.contains("Suite 30") && !view.contains("HEAD → main"), "{view}");

    // Le curseur va sur le commit de HEAD, la vue le suit, et rien ne s'ouvre.
    app.apply(Action::GotoHead);
    let view = screen(&mut app, 110, 12);
    assert!(view.contains("HEAD → main  Merge de feature"), "{view}");
    assert_eq!((app.selected, app.depth()), (30, 1));
    assert!(app.top > 0 && app.pane.is_none());
    // Déjà à l'écran : le curseur y revient sans que la vue bouge.
    let top = app.top;
    app.apply(Action::Move(2));
    app.apply(Action::GotoHead);
    assert_eq!((app.selected, app.top), (30, top));

    // Un commit ouvert n'est pas quitté pour autant.
    app.apply(Action::Top);
    app.apply(Action::Open);
    app.apply(Action::GotoHead);
    assert_eq!((app.depth(), app.selected), (2, 0));
    app.apply(Action::Back { quit: false });
    // HEAD écarté du graphe par le choix des branches : un message le dit.
    git(&dir, &["update-ref", "refs/remotes/origin/vieux", "v0.1.0"]);
    app.apply(Action::Refresh);
    app.apply(Action::OnlyRemotes);
    wait(&mut app, "distantes seules", |app| app.log_done && !app.stale);
    app.apply(Action::GotoHead);
    assert_eq!(app.flash(), Some("HEAD is not in the graph"));

    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn branches_of_the_graph_are_chosen() {
    let dir = make_repo("branches");
    // origin/main suit main, puis main avance seule ; origin/extra n'existe que chez origin.
    git(&dir, &["update-ref", "refs/remotes/origin/main", "main"]);
    git(&dir, &["checkout", "-q", "-b", "extra"]);
    std::fs::write(dir.join("d.txt"), "distant\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "Distant seulement"]);
    git(&dir, &["update-ref", "refs/remotes/origin/extra", "extra"]);
    git(&dir, &["checkout", "-q", "main"]);
    git(&dir, &["branch", "-q", "-D", "extra"]);
    std::fs::write(dir.join("e.txt"), "local\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "Local seulement"]);
    // Du travail non commité, posé sur HEAD.
    std::fs::write(dir.join("e.txt"), "local, en cours\n").unwrap();

    let mut app = App::new(Repo::open(&dir).unwrap(), true).unwrap();
    wait(&mut app, "historique", |app| app.log_done);
    let reload = |app: &mut App| wait(app, "graphe", |app| app.log_done && !app.stale);
    let keys = |app: &mut App, keys: &str| keys.chars().for_each(|key| app.apply(Action::PromptKey(key)));
    let view = screen(&mut app, 110, 16);
    assert!(view.contains("Distant seulement") && view.contains("Local seulement") && view.contains("- changes -"), "{view}");

    // `b` ouvre le sous-menu ; une touche qui n'en est pas le referme sans rien faire.
    app.apply(Action::BranchMenu);
    let view = screen(&mut app, 110, 16);
    assert!(view.contains(" Branches ") && view.contains("pick the branches of the graph"), "{view}");
    keys(&mut app, "x");
    assert!(app.prompt.is_none() && app.picker.is_none() && app.hierarchy.is_none());
    // `b` `h` : la hiérarchie, ouverte puis refermée.
    app.apply(Action::BranchMenu);
    keys(&mut app, "h");
    assert!(app.hierarchy.is_some() && app.prompt.is_none());
    app.apply(Action::BranchMenu);
    assert!(screen(&mut app, 110, 16).contains(" Branches "));
    keys(&mut app, "h");
    assert!(app.hierarchy.is_none());

    // `b` `o` : sans les branches distantes, le commit qui n'est que chez origin disparaît.
    app.apply(Action::BranchMenu);
    keys(&mut app, "o");
    reload(&mut app);
    let view = screen(&mut app, 110, 16);
    assert!(!view.contains("Distant seulement") && !view.contains("origin"), "{view}");
    assert!(view.contains("Local seulement") && view.contains("- changes -") && view.contains("· no remotes"), "{view}");
    // `b` `O` : elles seules. Plus de commit local, ni de travail en cours ; les
    // branches locales n'ont plus d'étiquette, les distantes ont leur nom entier.
    app.apply(Action::BranchMenu);
    keys(&mut app, "O");
    assert_eq!(app.flash(), Some("Remote branches only"));
    reload(&mut app);
    let view = screen(&mut app, 110, 16);
    assert!(view.contains("◀ origin/extra  Distant seulement") && view.contains("◀ origin/main  Merge de feature"), "{view}");
    assert!(!view.contains("Local seulement") && !view.contains("- changes -") && !view.contains("HEAD →"), "{view}");
    assert!(!view.contains("◀ feature") && view.contains("◇ v0.1.0 ") && view.contains("· remotes only"), "{view}");

    // b t cache les tags : leur cartouche s'en va, et le graphe ne part plus d'eux.
    app.apply(Action::ToggleTags);
    wait(&mut app, "graphe", |app| app.log_done);
    let view = screen(&mut app, 110, 14);
    assert!(!view.contains("v0.1.0") && view.contains("remotes only, no tags"), "{view}");
    app.apply(Action::ToggleTags);
    wait(&mut app, "graphe", |app| app.log_done);
    assert_eq!(app.flash(), Some("Tags shown"));
    assert!(screen(&mut app, 110, 14).contains("◇ v0.1.0 "));
    // `b` `o` les cache alors : il reste les locales. `b` `O` deux fois : seules, puis toutes.
    app.apply(Action::ToggleRemotes);
    assert_eq!(app.scope, crate::log::app::Scope::Local);
    app.apply(Action::OnlyRemotes);
    app.apply(Action::OnlyRemotes);
    assert_eq!(app.flash(), Some("Local and remote branches"));
    reload(&mut app);
    assert!(screen(&mut app, 110, 16).contains("Distant seulement"));

    // `b` `f` : la liste des branches, toutes cochées, locales d'abord.
    app.apply(Action::BranchMenu);
    keys(&mut app, "f");
    let picker = app.picker.as_ref().expect("la liste est ouverte");
    let names: Vec<&str> = picker.items.iter().map(|(_, name)| name.as_str()).collect();
    assert_eq!(names, ["feature", "main", "origin/extra", "origin/main"]);
    assert!(picker.checked.iter().all(|&checked| checked));
    let view = screen(&mut app, 110, 16);
    assert!(view.contains("Branches in the graph · 4/4") && view.contains("[x] origin/extra"), "{view}");
    // `v` décoche tout ; rien de coché ne s'applique pas. `a` n'est plus un raccourci.
    keys(&mut app, "av");
    app.apply(Action::PromptSubmit);
    assert!(app.picker.is_some() && app.flash() == Some("Pick at least one branch"));
    assert!(screen(&mut app, 110, 16).contains("Branches in the graph · 0/4"));
    // `/` cherche sans filtrer : le curseur va sur la branche trouvée, la liste reste entière.
    keys(&mut app, "/EXTRA");
    assert_eq!(app.picker.as_ref().unwrap().sel, 0);
    for _ in 0..5 {
        app.apply(Action::PromptBackspace);
    }
    keys(&mut app, "extra");
    let view = screen(&mut app, 110, 16);
    assert!(view.contains("find: extra") && view.contains("1 match") && view.contains("[ ] feature"), "{view}");
    app.apply(Action::PromptSubmit);
    assert!(app.picker.as_ref().is_some_and(|picker| picker.sel == 2 && !picker.typing));
    // Espace coche la branche sous le curseur ; `n` et `N` passent d'une trouvée à l'autre.
    keys(&mut app, " ");
    keys(&mut app, "/main");
    app.apply(Action::PromptSubmit);
    assert_eq!(app.picker.as_ref().unwrap().sel, 3);
    keys(&mut app, "n");
    assert_eq!(app.picker.as_ref().unwrap().sel, 1);
    keys(&mut app, "N");
    assert_eq!(app.picker.as_ref().unwrap().sel, 3);
    // Échap quitte la recherche, puis Entrée applique : le graphe part de origin/extra seule.
    app.apply(Action::PromptCancel);
    assert!(app.picker.as_ref().is_some_and(|picker| picker.query.is_empty()));
    app.apply(Action::PromptSubmit);
    assert!(app.picker.is_none());
    reload(&mut app);
    let view = screen(&mut app, 110, 16);
    assert!(view.contains("◀ origin/extra  Distant seulement") && view.contains("· 1 branch picked"), "{view}");
    assert!(!view.contains("Local seulement") && !view.contains("origin/main") && !view.contains("HEAD →"), "{view}");
    assert!(!view.contains("- changes -") && view.contains("Merge de feature"), "{view}");
    // Une branche choisie que la nature écarte : il ne reste rien à montrer.
    app.apply(Action::ToggleRemotes);
    reload(&mut app);
    let view = screen(&mut app, 110, 16);
    assert!(view.contains("No commits on the branches shown.") && view.contains("no remotes, 1 branch picked"), "{view}");
    app.apply(Action::ToggleRemotes);

    // La liste rouvre sur le choix en place. Échap la referme sans rien changer ;
    // `v` puis Entrée reviennent à toutes les branches.
    app.apply(Action::PickBranches);
    assert_eq!(app.picker.as_ref().unwrap().checked, [false, false, true, false]);
    keys(&mut app, "jv");
    app.apply(Action::PromptCancel);
    assert!(app.picker.is_none() && app.picked.as_ref().is_some_and(|picked| picked.len() == 1));
    app.apply(Action::PickBranches);
    keys(&mut app, "v");
    app.apply(Action::PromptSubmit);
    assert!(app.picked.is_none());
    reload(&mut app);
    let view = screen(&mut app, 110, 16);
    assert!(view.contains("Local seulement") && view.contains("Distant seulement") && !view.contains("picked"), "{view}");

    // Une branche choisie puis supprimée est oubliée au rafraîchissement ; comme
    // c'était la seule, le graphe revient à toutes les branches.
    app.apply(Action::PickBranches);
    keys(&mut app, "v ");
    app.apply(Action::PromptSubmit);
    assert_eq!(app.picked.as_ref().map(|picked| picked.len()), Some(1));
    git(&dir, &["branch", "-q", "-D", "feature"]);
    app.apply(Action::Refresh);
    reload(&mut app);
    assert!(app.picked.is_none() && app.log_error.is_none());
    assert!(screen(&mut app, 110, 16).contains("Local seulement"));

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
        diff.lines.iter().filter(|l| l.kind == crate::log::diff::LineKind::Add).map(|l| l.text.clone()).collect()
    };
    app.apply(Action::Open);
    wait(&mut app, "diff", diff_done);
    assert_eq!(texts(&app), ["cinq"]);
    // Un fichier non suivi se lit en entier, comme un ajout.
    app.apply(Action::Sibling(1));
    app.apply(Action::Sibling(1));
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
    use crate::log::git::Head;

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
    assert!(matches!(app.prompt, Some(crate::log::app::Prompt::Kind { sign: Some('+') })));
    app.apply(Action::PromptKey('h'));
    app.apply(Action::PromptKey('x'));
    let typing = screen(&mut app, 110, 20);
    let last = typing.lines().last().unwrap();
    assert!(last.starts_with(" +hunk: x") && last.trim_end().ends_with("Enter to search"), "{last}");
    assert_eq!(app.filter.as_ref().map(|f| f.kind), Some(crate::log::git::SearchKind::Hash), "les hunks attendent Entrée");
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

    // y copie le nom de la branche sous le curseur, et non le hash du commit.
    app.apply(Action::Move(1));
    assert_eq!(app.copy_target().as_deref(), Some("feature"));
    app.apply(Action::CopySelected);
    // Sans presse-papier dans l'environnement de test, le message le dit ;
    // c'est bien la copie qui a été tentée.
    assert!(app.flash().is_some_and(|said| said.contains("feature") || said == "Clipboard unavailable"));
    app.apply(Action::Bottom);
    assert_eq!(app.copy_target().as_deref(), Some("wip-suite"));

    // Entrée sur une branche ramène au graphe, sur son dernier commit.
    app.apply(Action::Open);
    assert!(app.hierarchy.is_none());
    let meta = app.meta(app.selected);
    assert_eq!(meta.subject, "Suite du brouillon");

    // r fait de la branche sous le curseur la racine de l'arbre.
    app.apply(Action::ToggleHierarchy);
    let ready = |app: &App| app.hierarchy.as_ref().is_some_and(|h| matches!(h.state, HierState::Ready { .. }));
    wait(&mut app, "hiérarchie", ready);
    app.apply(Action::Bottom);
    assert_eq!(app.copy_target().as_deref(), Some("wip-suite"));
    app.apply(Action::RootHere);
    wait(&mut app, "hiérarchie", ready);
    assert_eq!(app.flash(), Some("wip-suite is the root of the tree"));
    let view = screen(&mut app, 100, 12);
    let lines: Vec<&str> = view.lines().collect();
    assert!(lines[2].contains("◀ wip-suite ") && !lines[2].contains("├─"), "{view}");
    // main est passée dessous, avec tout ce qu'elle porte.
    assert!(view.lines().skip(3).any(|line| line.contains("◀ main ")), "{view}");
    // Sur la racine choisie, r rend l'arbre à son choix d'origine.
    app.apply(Action::Top);
    app.apply(Action::RootHere);
    wait(&mut app, "hiérarchie", ready);
    assert_eq!(app.flash(), Some("Root back to the usual branch"));
    assert!(screen(&mut app, 100, 12).lines().nth(2).is_some_and(|line| line.contains("◀ main ")));

    // h ouvre et referme la vue.
    app.apply(Action::ToggleHierarchy);
    assert!(app.hierarchy.is_none());
    app.apply(Action::ToggleHierarchy);
    assert!(app.hierarchy.is_some());
    app.apply(Action::ToggleHierarchy);
    assert!(app.hierarchy.is_none());

    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}
