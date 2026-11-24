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
    let mut app = App::new(Repo::open(&dir).unwrap(), true).unwrap();
    wait(&mut app, "historique", |app| app.log_done);
    assert_eq!(app.store.len(), 4);

    // Niveau 1 : graphe, étiquettes, hash et icône de copie.
    let level1 = screen(&mut app, 110, 12);
    println!("{level1}");
    let lines: Vec<&str> = level1.lines().collect();
    assert!(lines[0].contains("HEAD on main") && lines[0].contains("4 commits"));
    assert!(lines[2].contains("●─╮") && lines[2].contains("◀ main ") && lines[2].contains("Merge de feature"));
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
    assert!(unified.contains("b.txt") && unified.contains("+ nouveau"));

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

    // n / p dans la liste des fichiers : commit suivant, précédent.
    app.apply(Action::Step(1));
    assert_eq!((app.depth(), app.selected), (2, 1));
    assert_eq!(app.pane.as_ref().map(|p| p.oid), Some(app.store.oid(1)));
    app.apply(Action::Step(-1));
    app.apply(Action::Step(-1));
    assert_eq!((app.depth(), app.selected), (2, 0));
    assert_eq!(app.flash(), Some("First commit"));

    // n / p dans un diff : fichier suivant, précédent, sans sortir du commit.
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
    assert!(screen(&mut app, 110, 30).contains("? help"));
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
fn commit_styles_link_the_commits() {
    let dir = make_repo("styles");
    let mut app = App::new(Repo::open(&dir).unwrap(), false).unwrap();
    wait(&mut app, "historique", |app| app.log_done);
    let graph = |app: &mut App| -> Vec<String> {
        screen(app, 110, 12).lines().skip(2).take(4).map(|l| l[..l.find(' ').map_or(0, |_| 12)].to_string()).collect()
    };

    // Par défaut : une puce par commit.
    assert_eq!(app.node_style, NodeStyle::Dots);
    assert!(graph(&mut app)[0].starts_with(" ●─╮"));

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
    assert!(rail[0].starts_with(" ╭●╮"), "{}", rail[0]);
    assert!(rail[1].starts_with(" ├●│"), "{}", rail[1]);
    assert!(rail[2].starts_with(" │ ├●"), "{}", rail[2]);
    assert!(rail[3].starts_with(" ╰●╯"), "{}", rail[3]);

    app.apply(Action::CycleNodes);
    assert_eq!(app.node_style, NodeStyle::Dots);
    assert!(graph(&mut app)[0].starts_with(" ●─╮"));

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
