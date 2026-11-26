//! Tronc commun d'une suite d'outils git pour le terminal. Chaque outil est un
//! binaire de quelques lignes (`src/bin`) qui lance l'application dans son mode :
//! `gitlog` montre le graphe des commits, `gitchange` ce qui n'est pas encore
//! commité. La lecture de git, les diffs, leur coloration, l'interface et la
//! boucle d'événements sont les mêmes pour tous.

mod app;
mod diff;
mod git;
mod graph;
mod hierarchy;
mod run;
mod syntax;
#[cfg(test)]
mod tests;
mod theme;
mod ui;

pub use app::Tool;
pub use run::launch;
