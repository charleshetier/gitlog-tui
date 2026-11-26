//! `gitlog` et `gitchange` : le graphe des commits et le détail d'un commit, ou
//! de ce qui n'est pas encore commité. Les deux outils sont la même application,
//! lancée dans un mode ou dans l'autre.

pub mod app;
pub mod diff;
pub mod git;
pub mod graph;
pub mod hierarchy;
pub mod keys;
#[cfg(test)]
mod tests;
pub mod ui;
