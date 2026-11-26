//! Une suite d'outils git pour le terminal, bâtie sur un tronc commun.
//!
//! - `kit` : ce que tous les outils partagent. Le terminal et sa boucle
//!   d'événements, le socle git, les briques de dessin, la coloration syntaxique
//!   et son thème, les messages et le presse-papier.
//! - `log` : `gitlog`, le graphe des commits, et `gitchange`, ce qui n'est pas
//!   encore commité.
//! - `cli` : les arguments et l'aide de chaque commande.
//!
//! Chaque outil est un binaire de quelques lignes, dans `src/bin`.

mod cli;
mod kit;
mod log;

pub use cli::launch;
pub use kit::Tool;
