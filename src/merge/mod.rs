//! `gitmerge` : régler les conflits d'une fusion, fichier par fichier, sur
//! trois colonnes. La nôtre à gauche, celle qui arrive à droite, et au milieu
//! le résultat, qu'on édite.

pub mod app;
pub mod git;
pub mod keys;
pub mod model;
pub mod ui;

#[cfg(test)]
mod tests;
