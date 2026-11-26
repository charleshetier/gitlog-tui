//! Recherche d'un texte dans des lignes, la même dans tous les outils : sans
//! majuscule dans ce qu'on cherche, la casse est ignorée.

/// Texte recherché, prêt à être comparé caractère pour caractère.
pub struct Needle {
    chars: Vec<char>,
    exact: bool,
}

impl Needle {
    pub fn new(query: &str) -> Needle {
        let exact = query.chars().any(char::is_uppercase);
        let mut needle = Needle { chars: Vec::new(), exact };
        needle.chars = query.chars().map(|c| needle.fold(c)).collect();
        needle
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    /// Longueur d'une occurrence, en caractères.
    pub fn len(&self) -> usize {
        self.chars.len()
    }

    // Caractère pour caractère : les rangs restent ceux du texte affiché.
    fn fold(&self, c: char) -> char {
        if self.exact { c } else { c.to_lowercase().next().unwrap_or(c) }
    }

    /// Rang du premier caractère de chaque occurrence dans `text` ; elles ne se
    /// chevauchent pas.
    pub fn starts(&self, text: &str) -> Vec<usize> {
        let mut starts = Vec::new();
        if self.is_empty() {
            return starts;
        }
        let text: Vec<char> = text.chars().map(|c| self.fold(c)).collect();
        let mut at = 0;
        while at + self.chars.len() <= text.len() {
            if text[at..at + self.chars.len()] == self.chars[..] {
                starts.push(at);
                at += self.chars.len();
            } else {
                at += 1;
            }
        }
        starts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_matters_only_with_a_capital() {
        assert_eq!(Needle::new("ab").starts("ab AB aB abab"), [0, 3, 6, 9, 11]);
        assert_eq!(Needle::new("Ab").starts("ab Ab"), [3]);
        assert_eq!(Needle::new("aa").starts("aaaaa"), [0, 2]);
        assert!(Needle::new("").starts("abc").is_empty());
        assert_eq!(Needle::new("été").len(), 3);
    }
}
