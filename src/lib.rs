//! Une suite d'outils git pour le terminal, bâtie sur un tronc commun.
//!
//! - `kit` : ce que tous les outils partagent. Le terminal et sa boucle
//!   d'événements, le socle git, les briques de dessin, la coloration syntaxique
//!   et son thème, l'édition modale, la recherche, les messages et le presse-papier.
//! - `log` : `gitlog`, le graphe des commits, et `gitchange`, ce qui n'est pas
//!   encore commité.
//! - `merge` : `gitmerge`, les fichiers en conflit sur trois colonnes.
//! - `cli` : les arguments et l'aide de chaque commande.
//!
//! Chaque outil est un binaire de quelques lignes, dans `src/bin`.

mod cli;
mod kit;
mod log;
mod merge;

pub use cli::launch;
pub use kit::Tool;
