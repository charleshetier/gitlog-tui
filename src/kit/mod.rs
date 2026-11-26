//! Tronc commun de la suite : ce que tous les outils partagent.

pub mod git;
pub mod notice;
pub mod paint;
pub mod syntax;
pub mod term;
pub mod theme;

/// Outil de la suite.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    /// `gitlog` : le graphe des commits, puis le détail de celui qu'on ouvre.
    Log,
    /// `gitchange` : ce qui n'est pas encore commité, sans le graphe. Le détail
    /// est ouvert d'emblée : liste des fichiers, diff, fichier entier.
    Changes,
}

impl Tool {
    /// Nom de la commande.
    pub fn name(self) -> &'static str {
        match self {
            Tool::Log => "gitlog",
            Tool::Changes => "gitchange",
        }
    }
}
