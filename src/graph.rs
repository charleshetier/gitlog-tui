//! Calcul incrémental des colonnes du graphe et stockage compact des lignes.
//!
//! Les commits arrivent dans l'ordre de `git log --date-order` : un enfant
//! toujours avant ses parents. Chaque colonne active attend un commit précis ;
//! le calcul ne garde que ces colonnes et traite chaque commit une seule fois.

pub const UP: u8 = 1;
pub const DOWN: u8 = 2;
pub const LEFT: u8 = 4;
pub const RIGHT: u8 = 8;
pub const COMMIT: u8 = 16;

/// Une cellule de terminal du graphe : les directions qui la traversent et
/// l'indice de couleur de sa branche.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Cell {
    pub bits: u8,
    pub color: u8,
}

impl Cell {
    pub fn glyph(self) -> char {
        if self.bits & COMMIT != 0 {
            return '●';
        }
        match self.bits & (UP | DOWN | LEFT | RIGHT) {
            0 => ' ',
            b if b == UP | DOWN || b == UP || b == DOWN => '│',
            b if b == LEFT | RIGHT || b == LEFT || b == RIGHT => '─',
            b if b == DOWN | LEFT => '╮',
            b if b == DOWN | RIGHT => '╭',
            b if b == UP | LEFT => '╯',
            b if b == UP | RIGHT => '╰',
            b if b == UP | DOWN | LEFT => '┤',
            b if b == UP | DOWN | RIGHT => '├',
            b if b == LEFT | RIGHT | DOWN => '┬',
            b if b == LEFT | RIGHT | UP => '┴',
            _ => '┼',
        }
    }
}

/// Identifiant d'objet git (SHA-1 sur 20 octets ou SHA-256 sur 32).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Oid {
    bytes: [u8; 32],
    len: u8,
}

impl Oid {
    pub fn from_hex(hex: &str) -> Option<Oid> {
        let h = hex.as_bytes();
        if (h.len() != 40 && h.len() != 64) || !h.iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        let val = |c: u8| (c as char).to_digit(16).unwrap() as u8;
        let mut bytes = [0u8; 32];
        for (i, pair) in h.chunks(2).enumerate() {
            bytes[i] = val(pair[0]) << 4 | val(pair[1]);
        }
        Some(Oid { bytes, len: (h.len() / 2) as u8 })
    }

    pub fn from_bytes(b: &[u8]) -> Oid {
        let mut bytes = [0u8; 32];
        bytes[..b.len()].copy_from_slice(b);
        Oid { bytes, len: b.len() as u8 }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }

    pub fn to_hex(self) -> String {
        self.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn short(self) -> String {
        let mut s = self.to_hex();
        s.truncate(7);
        s
    }
}

#[derive(Clone, Copy)]
struct Lane {
    oid: Oid,
    color: u8,
}

/// État du calcul : uniquement les colonnes actives.
#[derive(Default)]
pub struct Layout {
    lanes: Vec<Option<Lane>>,
    next_color: u8,
    row: Vec<Cell>,
    closing: Vec<usize>,
}

impl Layout {
    fn new_color(&mut self) -> u8 {
        let c = self.next_color;
        self.next_color = self.next_color.wrapping_add(1);
        c
    }

    fn cell(&mut self, idx: usize) -> &mut Cell {
        if idx >= self.row.len() {
            self.row.resize(idx + 1, Cell::default());
        }
        &mut self.row[idx]
    }

    /// Trace la liaison horizontale entre la colonne du commit et la colonne `k`.
    fn hline(&mut self, col: usize, k: usize, color: u8) {
        let (a, b) = if k > col { (2 * col, 2 * k) } else { (2 * k, 2 * col) };
        for idx in a + 1..b {
            let c = self.cell(idx);
            c.bits |= LEFT | RIGHT;
            if c.bits & (UP | DOWN) == 0 {
                c.color = color;
            }
        }
    }

    /// Place un commit et renvoie sa colonne et les cellules de sa ligne.
    pub fn push(&mut self, oid: &Oid, parents: &[Oid]) -> (u16, &[Cell]) {
        self.row.clear();
        self.closing.clear();

        // Colonnes qui attendaient ce commit : la première le reçoit, les autres se referment.
        let mut found = None;
        for (j, lane) in self.lanes.iter().enumerate() {
            if lane.is_some_and(|l| l.oid == *oid) {
                if found.is_none() {
                    found = Some(j);
                } else {
                    self.closing.push(j);
                }
            }
        }
        let expected = found.is_some();
        let (col, color) = match found {
            Some(c) => (c, self.lanes[c].unwrap().color),
            None => {
                let c = match self.lanes.iter().position(Option::is_none) {
                    Some(c) => c,
                    None => {
                        self.lanes.push(None);
                        self.lanes.len() - 1
                    }
                };
                (c, self.new_color())
            }
        };

        // Branches qui ne font que passer.
        for j in 0..self.lanes.len() {
            if j == col || self.closing.contains(&j) {
                continue;
            }
            if let Some(lane) = self.lanes[j] {
                *self.cell(2 * j) = Cell { bits: UP | DOWN, color: lane.color };
            }
        }

        *self.cell(2 * col) = Cell { bits: COMMIT | if expected { UP } else { 0 }, color };

        // Fermetures : arrondi vers le commit.
        for i in 0..self.closing.len() {
            let j = self.closing[i];
            let lane_color = self.lanes[j].unwrap().color;
            let c = self.cell(2 * j);
            c.bits |= UP | if j > col { LEFT } else { RIGHT };
            c.color = lane_color;
            self.hline(col, j, lane_color);
            self.lanes[j] = None;
        }

        // Le premier parent hérite de la colonne, les autres en rejoignent ou en ouvrent une.
        match parents.first() {
            None => self.lanes[col] = None,
            Some(first) => {
                self.lanes[col] = Some(Lane { oid: *first, color });
                self.cell(2 * col).bits |= DOWN;
            }
        }
        for (n, p) in parents.iter().enumerate().skip(1) {
            if parents[..n].contains(p) {
                continue;
            }
            let side = |k: usize| if k > col { LEFT } else { RIGHT };
            let existing = self
                .lanes
                .iter()
                .position(|l| l.is_some_and(|l| l.oid == *p))
                .filter(|&k| k != col);
            if let Some(k) = existing {
                let lane_color = self.lanes[k].unwrap().color;
                self.cell(2 * k).bits |= UP | DOWN | side(k);
                self.hline(col, k, lane_color);
            } else {
                // Une colonne libérée sur cette ligne n'est pas réutilisée tout de suite.
                let free = (0..self.lanes.len())
                    .find(|k| self.lanes[*k].is_none() && *k != col && !self.closing.contains(k));
                let k = match free {
                    Some(k) => k,
                    None => {
                        self.lanes.push(None);
                        self.lanes.len() - 1
                    }
                };
                let lane_color = self.new_color();
                self.lanes[k] = Some(Lane { oid: *p, color: lane_color });
                let c = self.cell(2 * k);
                c.bits |= DOWN | side(k);
                c.color = lane_color;
                self.hline(col, k, lane_color);
            }
        }

        while matches!(self.lanes.last(), Some(None)) {
            self.lanes.pop();
        }
        (col as u16, &self.row)
    }
}

/// Position d'une ligne dans les tableaux partagés : 8 octets par commit.
#[derive(Clone, Copy)]
struct RowRec {
    start: u32,
    len: u16,
    col: u16,
}

/// Lot de lignes calculées par le thread de lecture.
#[derive(Default)]
pub struct Batch {
    pub oid_len: usize,
    pub oids: Vec<u8>,
    /// (colonne du commit, nombre de cellules) par ligne.
    pub rows: Vec<(u16, u16)>,
    pub cells: Vec<Cell>,
}

impl Batch {
    pub fn push(&mut self, oid: &Oid, col: u16, cells: &[Cell]) {
        self.oid_len = oid.as_bytes().len();
        self.oids.extend_from_slice(oid.as_bytes());
        let len = cells.len().min(u16::MAX as usize);
        self.rows.push((col, len as u16));
        self.cells.extend_from_slice(&cells[..len]);
    }
}

/// Toutes les lignes reçues, sans aucun texte : identifiant, colonne, cellules.
#[derive(Default)]
pub struct GraphStore {
    oid_len: usize,
    oids: Vec<u8>,
    rows: Vec<RowRec>,
    cells: Vec<Cell>,
}

impl GraphStore {
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn append(&mut self, batch: Batch) {
        if batch.rows.is_empty() {
            return;
        }
        self.oid_len = batch.oid_len;
        self.oids.extend_from_slice(&batch.oids);
        let mut start = self.cells.len();
        self.rows.reserve(batch.rows.len());
        for (col, len) in batch.rows {
            self.rows.push(RowRec { start: start as u32, len, col });
            start += len as usize;
        }
        self.cells.extend_from_slice(&batch.cells);
    }

    pub fn oid(&self, row: usize) -> Oid {
        Oid::from_bytes(&self.oids[row * self.oid_len..(row + 1) * self.oid_len])
    }

    pub fn cells(&self, row: usize) -> &[Cell] {
        let r = self.rows[row];
        &self.cells[r.start as usize..r.start as usize + r.len as usize]
    }

    /// Cherche un commit parmi les lignes chargées, à partir de la ligne `from`.
    pub fn find(&self, oid: &Oid, from: usize) -> Option<usize> {
        if self.oid_len == 0 || from >= self.rows.len() {
            return None;
        }
        self.oids[from * self.oid_len..]
            .chunks_exact(self.oid_len)
            .position(|candidate| candidate == oid.as_bytes())
            .map(|i| i + from)
    }

    /// Mémoire réservée par les lignes, en octets.
    #[cfg(test)]
    pub fn memory_bytes(&self) -> usize {
        self.oids.capacity()
            + self.rows.capacity() * size_of::<RowRec>()
            + self.cells.capacity() * size_of::<Cell>()
    }

    /// Indice de couleur de la branche du commit.
    pub fn color(&self, row: usize) -> u8 {
        let r = self.rows[row];
        self.cells[r.start as usize + 2 * r.col as usize].color
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(n: u8) -> Oid {
        Oid::from_bytes(&[n; 20])
    }

    /// Rend un historique `(commit, parents)` en lignes de texte.
    fn render(history: &[(u8, &[u8])]) -> Vec<String> {
        let mut layout = Layout::default();
        history
            .iter()
            .map(|(id, parents)| {
                let parents: Vec<Oid> = parents.iter().map(|p| oid(*p)).collect();
                let (_, cells) = layout.push(&oid(*id), &parents);
                let line: String = cells.iter().map(|c| c.glyph()).collect();
                line.trim_end().to_string()
            })
            .collect()
    }

    #[test]
    fn linear() {
        assert_eq!(render(&[(3, &[2]), (2, &[1]), (1, &[])]), ["●", "●", "●"]);
    }

    #[test]
    fn mockup_history() {
        let rows = render(&[
            (9, &[7, 8]),
            (8, &[6]),
            (7, &[3]),
            (6, &[4]),
            (5, &[3]),
            (4, &[3]),
            (3, &[2]),
            (2, &[1]),
            (1, &[]),
        ]);
        assert_eq!(
            rows,
            ["●─╮", "│ ●", "● │", "│ ●", "│ │ ●", "│ ● │", "●─┴─╯", "●", "●"]
        );
    }

    #[test]
    fn octopus_merge() {
        let rows = render(&[(9, &[1, 2, 3]), (3, &[]), (2, &[]), (1, &[])]);
        assert_eq!(rows, ["●─┬─╮", "│ │ ●", "│ ●", "●"]);
    }

    #[test]
    fn merge_into_existing_lane_on_the_left() {
        // 5 attend 1 dans la colonne 0 ; le merge 4 (colonne 1) rejoint cette colonne.
        let rows = render(&[(5, &[1]), (4, &[2, 1]), (2, &[1]), (1, &[])]);
        assert_eq!(rows, ["●", "├─●", "│ ●", "●─╯"]);
    }

    #[test]
    fn crossing_keeps_the_vertical_lane() {
        // 8 passe par la colonne 1 pendant que 7 ouvre une colonne au-delà.
        let rows = render(&[(9, &[7, 8]), (7, &[5, 6]), (8, &[5]), (6, &[5]), (5, &[])]);
        assert_eq!(rows, ["●─╮", "●─┼─╮", "│ ● │", "│ │ ●", "●─┴─╯"]);
    }

    #[test]
    fn freed_lane_is_not_reused_on_the_same_row() {
        // 3 ferme la colonne 1 et ouvre une colonne pour son second parent : ce sera la 2.
        let rows = render(&[(5, &[3]), (4, &[3]), (3, &[1, 2]), (2, &[]), (1, &[])]);
        assert_eq!(rows, ["●", "│ ●", "●─┴─╮", "│   ●", "●"]);
    }

    #[test]
    fn store_roundtrip() {
        let mut layout = Layout::default();
        let mut batch = Batch::default();
        for (id, parents) in [(3u8, vec![oid(1), oid(2)]), (2, vec![]), (1, vec![])] {
            let (col, cells) = layout.push(&oid(id), &parents);
            batch.push(&oid(id), col, cells);
        }
        let mut store = GraphStore::default();
        store.append(batch);
        assert_eq!(store.len(), 3);
        assert_eq!(store.oid(1), oid(2));
        assert_eq!(store.cells(0).len(), 3);
        assert_eq!(store.color(1), store.cells(0)[2].color);
    }

    #[test]
    fn oid_hex_roundtrip() {
        let hex = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(Oid::from_hex(hex).unwrap().to_hex(), hex);
        assert_eq!(Oid::from_hex(hex).unwrap().short(), "0123456");
        assert!(Oid::from_hex("xyz").is_none());
    }
}
