//! Hiérarchie des branches locales, à la manière de `git machete` : chaque
//! branche est rattachée à la branche dont elle est partie, sans les commits.
//!
//! L'arbre est déduit du graphe : une branche a pour parent la branche déjà
//! placée par rapport à laquelle elle a le moins de commits propres. Deux
//! branches sur le même commit sont frères : rien ne dit laquelle vient de
//! l'autre.

use crate::log::graph::Oid;

/// Branches prises pour racine, par ordre de préférence.
const ROOT_NAMES: [&str; 4] = ["main", "master", "trunk", "develop"];

/// État d'une branche par rapport à son parent dans l'arbre.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sync {
    Root,
    /// Le parent est un ancêtre : rien à rebaser.
    UpToDate,
    /// Le parent a avancé depuis le point de départ : à rebaser.
    Stale,
    /// Tous les commits de la branche sont déjà dans le parent.
    Merged,
}

/// État d'une branche par rapport à sa branche distante suivie.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Remote {
    Untracked,
    InSync,
    Ahead,
    Behind,
    Diverged,
    Gone,
}

#[derive(Clone, Debug)]
pub struct Branch {
    pub name: String,
    pub oid: Oid,
    pub is_head: bool,
    pub remote: Remote,
}

/// Une ligne de la vue : la branche, sa place dans l'arbre et son état.
#[derive(Clone, Debug)]
pub struct BranchRow {
    pub branch: Branch,
    pub depth: u16,
    pub sync: Sync,
    /// Commits propres à la branche, et commits du parent qui lui manquent.
    pub ahead: u32,
    pub behind: u32,
    /// Pour chaque ancêtre intermédiaire : vrai s'il a encore des frères en dessous.
    pub guides: Vec<bool>,
    /// Dernier enfant de son parent.
    pub last: bool,
}

/// Construit l'arbre. `ab[b][p]` donne (commits de `b` absents de `p`,
/// commits de `p` absents de `b`). `wanted` impose la racine, quand on l'a
/// choisie dans la vue.
pub fn build(branches: &[Branch], ab: &[Vec<(u32, u32)>], wanted: Option<&str>) -> Vec<BranchRow> {
    let n = branches.len();
    if n == 0 {
        return Vec::new();
    }
    // Racine : celle qu'on a choisie, sinon un nom conventionnel, sinon la
    // branche la plus contenue dans les autres.
    let root = wanted
        .and_then(|name| branches.iter().position(|b| b.name == name))
        .or_else(|| ROOT_NAMES.iter().find_map(|name| branches.iter().position(|b| b.name == *name)))
        .unwrap_or_else(|| {
            (0..n)
                .min_by_key(|&b| {
                    let own: u64 = (0..n).map(|p| u64::from(ab[b][p].0)).sum();
                    (own, &branches[b].name)
                })
                .unwrap()
        });

    // Les branches les plus proches de la racine sont placées d'abord ; un
    // parent est toujours choisi parmi les branches déjà placées.
    let mut order: Vec<usize> = (0..n).filter(|&b| b != root).collect();
    order.sort_by(|&a, &b| (ab[a][root].0, &branches[a].name).cmp(&(ab[b][root].0, &branches[b].name)));

    let mut parent = vec![root; n];
    let mut depth = vec![0u16; n];
    let mut sync = vec![Sync::Root; n];
    let mut placed = vec![root];
    for &b in &order {
        let p = if ab[b][root].0 == 0 {
            // Entièrement contenue dans la racine.
            root
        } else {
            placed
                .iter()
                .copied()
                // Même contenu qu'une branche déjà placée : elle en dépend directement.
                .filter(|&p| ab[b][p].0 == 0)
                .min_by_key(|&p| (ab[b][p].1, depth[p]))
                .unwrap_or_else(|| {
                    placed
                        .iter()
                        .copied()
                        .min_by_key(|&p| (ab[b][p].0, depth[p], &branches[p].name))
                        .unwrap()
                })
        };
        // Même commit : l'une ne vient pas de l'autre, elles sont frères. La
        // racine n'en a pas : ce qui la rejoint reste son enfant.
        let p = if ab[b][p] == (0, 0) && p != root { parent[p] } else { p };
        sync[b] = match ab[b][p] {
            (0, behind) if behind > 0 => Sync::Merged,
            (_, 0) => Sync::UpToDate,
            _ => Sync::Stale,
        };
        parent[b] = p;
        depth[b] = depth[p] + 1;
        placed.push(b);
    }

    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &b in &order {
        children[parent[b]].push(b);
    }
    // Les branches qui n'en portent pas d'autres d'abord, et la plus grosse
    // descendance en dernier : les longs traits de l'arbre restent en bas.
    let mut carried = vec![0usize; n];
    for &b in order.iter().rev() {
        carried[parent[b]] += carried[b] + 1;
    }
    for list in &mut children {
        list.sort_by(|&a, &b| (carried[a], &branches[a].name).cmp(&(carried[b], &branches[b].name)));
    }

    // Parcours en profondeur, sans récursion.
    let mut rows = Vec::with_capacity(n);
    let mut stack: Vec<(usize, Vec<bool>, bool)> = vec![(root, Vec::new(), true)];
    while let Some((node, guides, last)) = stack.pop() {
        let (ahead, behind) = if node == root { (0, 0) } else { ab[node][parent[node]] };
        let mut child_guides = guides.clone();
        if node != root {
            child_guides.push(!last);
        }
        let count = children[node].len();
        for (i, &child) in children[node].iter().enumerate().rev() {
            stack.push((child, child_guides.clone(), i + 1 == count));
        }
        rows.push(BranchRow {
            branch: branches[node].clone(),
            depth: depth[node],
            sync: sync[node],
            ahead,
            behind,
            guides,
            last,
        });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Décrit chaque branche par l'ensemble de ses commits et en déduit la matrice.
    fn tree(branches: &[(&str, &[u32])]) -> Vec<(String, u16, Sync, u32, u32)> {
        let infos: Vec<Branch> = branches
            .iter()
            .enumerate()
            .map(|(i, (name, _))| Branch {
                name: name.to_string(),
                oid: Oid::from_bytes(&[i as u8; 20]),
                is_head: false,
                remote: Remote::Untracked,
            })
            .collect();
        let missing = |a: &[u32], b: &[u32]| a.iter().filter(|c| !b.contains(c)).count() as u32;
        let ab: Vec<Vec<(u32, u32)>> = branches
            .iter()
            .map(|(_, b)| branches.iter().map(|(_, p)| (missing(b, p), missing(p, b))).collect())
            .collect();
        build(&infos, &ab, None)
            .into_iter()
            .map(|r| (r.branch.name, r.depth, r.sync, r.ahead, r.behind))
            .collect()
    }

    fn row(name: &str, depth: u16, sync: Sync, ahead: u32, behind: u32) -> (String, u16, Sync, u32, u32) {
        (name.to_string(), depth, sync, ahead, behind)
    }

    #[test]
    fn stacked_branches_follow_each_other() {
        let rows = tree(&[("part2", &[1, 2, 3, 4, 5]), ("main", &[1, 2]), ("part1", &[1, 2, 3, 4])]);
        assert_eq!(
            rows,
            [
                row("main", 0, Sync::Root, 0, 0),
                row("part1", 1, Sync::UpToDate, 2, 0),
                row("part2", 2, Sync::UpToDate, 1, 0),
            ]
        );
    }

    #[test]
    fn siblings_attach_to_the_root_even_when_it_moved() {
        // main a avancé de 3 commits : les deux branches sont à rebaser, sous main.
        let rows = tree(&[("main", &[1, 2, 6, 7, 8]), ("a", &[1, 2, 3]), ("b", &[1, 2, 4, 5])]);
        assert_eq!(
            rows,
            [
                row("main", 0, Sync::Root, 0, 0),
                row("a", 1, Sync::Stale, 1, 3),
                row("b", 1, Sync::Stale, 2, 3),
            ]
        );
    }

    #[test]
    fn merged_and_identical_branches() {
        // x et y sont sur le même commit : frères, et non l'une sous l'autre.
        let rows = tree(&[("main", &[1, 2]), ("old", &[1]), ("x", &[1, 2, 3]), ("y", &[1, 2, 3])]);
        assert_eq!(
            rows,
            [
                row("main", 0, Sync::Root, 0, 0),
                row("old", 1, Sync::Merged, 0, 1),
                row("x", 1, Sync::UpToDate, 1, 0),
                row("y", 1, Sync::UpToDate, 1, 0),
            ]
        );
    }

    #[test]
    fn a_branch_on_the_root_commit_stays_under_it() {
        // La racine ne peut pas avoir de frère : ce qui est sur son commit reste dessous.
        let rows = tree(&[("main", &[1]), ("same", &[1]), ("topic", &[1, 2])]);
        assert_eq!(
            rows,
            [
                row("main", 0, Sync::Root, 0, 0),
                row("same", 1, Sync::UpToDate, 0, 0),
                row("topic", 1, Sync::UpToDate, 1, 0),
            ]
        );
    }

    #[test]
    fn the_root_can_be_chosen() {
        let infos: Vec<Branch> = ["main", "release", "topic"]
            .iter()
            .map(|name| Branch { name: name.to_string(), oid: Oid::from_bytes(&[0; 20]), is_head: false, remote: Remote::Untracked })
            .collect();
        // release ⊂ main ⊂ topic : par son nom, main est la racine d'office.
        let sets: [&[u32]; 3] = [&[1, 2], &[1], &[1, 2, 3]];
        let missing = |a: &[u32], b: &[u32]| a.iter().filter(|c| !b.contains(c)).count() as u32;
        let ab: Vec<Vec<(u32, u32)>> =
            sets.iter().map(|b| sets.iter().map(|p| (missing(b, p), missing(p, b))).collect()).collect();
        let shape = |rows: Vec<BranchRow>| -> Vec<(String, u16)> { rows.into_iter().map(|r| (r.branch.name, r.depth)).collect() };
        assert_eq!(shape(build(&infos, &ab, None)), [("main".into(), 0), ("release".into(), 1), ("topic".into(), 1)]);
        // Choisie, release passe au-dessus et main devient son enfant.
        assert_eq!(shape(build(&infos, &ab, Some("release"))), [("release".into(), 0), ("main".into(), 1), ("topic".into(), 2)]);
        // Un nom qui n'est plus là ne change rien.
        assert_eq!(shape(build(&infos, &ab, Some("gone"))), shape(build(&infos, &ab, None)));
    }

    #[test]
    fn root_without_conventional_name_is_the_most_contained_branch() {
        let rows = tree(&[("topic", &[1, 2]), ("base", &[1])]);
        assert_eq!(rows[0], row("base", 0, Sync::Root, 0, 0));
        assert_eq!(rows[1], row("topic", 1, Sync::UpToDate, 1, 0));
    }

    #[test]
    fn guides_mark_ancestors_with_remaining_siblings() {
        let infos: Vec<Branch> = ["main", "a", "a1", "b"]
            .iter()
            .map(|name| Branch {
                name: name.to_string(),
                oid: Oid::from_bytes(&[0; 20]),
                is_head: false,
                remote: Remote::Untracked,
            })
            .collect();
        // main ← a ← a1, main ← b ; b, sans descendance, passe avant a.
        let sets: [&[u32]; 4] = [&[1], &[1, 2], &[1, 2, 3], &[1, 4]];
        let missing = |a: &[u32], b: &[u32]| a.iter().filter(|c| !b.contains(c)).count() as u32;
        let ab: Vec<Vec<(u32, u32)>> =
            sets.iter().map(|b| sets.iter().map(|p| (missing(b, p), missing(p, b))).collect()).collect();
        let rows = build(&infos, &ab, None);
        let shape: Vec<(&str, Vec<bool>, bool)> =
            rows.iter().map(|r| (r.branch.name.as_str(), r.guides.clone(), r.last)).collect();
        assert_eq!(
            shape,
            [
                ("main", vec![], true),
                ("b", vec![], false),
                ("a", vec![], true),
                ("a1", vec![false], true),
            ]
        );
    }
}
