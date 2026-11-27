//! Fusion à trois versions d'un fichier : la base commune, la nôtre (à gauche)
//! et celle qui arrive (à droite). Le résultat est un document découpé en
//! segments : les passages que les trois versions partagent, et les blocs où un
//! côté au moins a changé. Chaque bloc garde en regard les lignes de gauche, du
//! résultat et de droite, ce qui permet de les aligner à l'écran et de prendre
//! un côté ou l'autre. Le résultat s'édite librement : un passage commun que
//! l'on touche devient un bloc retouché à la main.

use std::ops::Range;
use std::rc::Rc;

/// Au-delà de ce nombre de différences, deux textes sont tenus pour entièrement
/// différents : la recherche du plus court chemin coûterait trop de mémoire.
const MAX_EDITS: usize = 2000;

/// Passages qui diffèrent entre `a` et `b`, dans l'ordre : la plage de `a`, puis
/// celle de `b` qui la remplace. Algorithme de Myers, après avoir écarté le
/// début et la fin que les deux partagent.
pub fn changes<T: PartialEq>(a: &[T], b: &[T]) -> Vec<(Range<usize>, Range<usize>)> {
    let mut out = shortest(a, b);
    // Un ajout ou un retrait au milieu de lignes qui se répètent peut se lire à
    // plusieurs endroits : il glisse aussi bas que possible, comme le fait git.
    // Une fonction ajoutée commence alors à sa première ligne, pas à l'accolade
    // qui ferme la précédente.
    for at in 0..out.len() {
        let limit = out.get(at + 1).map_or((a.len(), b.len()), |next| (next.0.start, next.1.start));
        let (in_a, in_b) = &mut out[at];
        while in_a.end < limit.0 && in_b.end < limit.1 {
            let (added, removed) = (in_a.start == in_a.end, in_b.start == in_b.end);
            let slides = (added && b[in_b.start] == b[in_b.end]) || (removed && a[in_a.start] == a[in_a.end]);
            if !slides {
                break;
            }
            (*in_a, *in_b) = (in_a.start + 1..in_a.end + 1, in_b.start + 1..in_b.end + 1);
        }
    }
    // Deux passages qui se touchent après avoir glissé n'en font qu'un.
    let mut joined: Vec<(Range<usize>, Range<usize>)> = Vec::new();
    for (in_a, in_b) in out {
        match joined.last_mut() {
            Some(prev) if prev.0.end == in_a.start && prev.1.end == in_b.start => (prev.0.end, prev.1.end) = (in_a.end, in_b.end),
            _ => joined.push((in_a, in_b)),
        }
    }
    joined
}

/// Le plus court ensemble de passages à changer pour passer de `a` à `b`.
fn shortest<T: PartialEq>(a: &[T], b: &[T]) -> Vec<(Range<usize>, Range<usize>)> {
    let head = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let tail = a[head..].iter().rev().zip(b[head..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (a, b) = (&a[head..a.len() - tail], &b[head..b.len() - tail]);
    let (n, m) = (a.len(), b.len());
    if n == 0 && m == 0 {
        return Vec::new();
    }
    let whole = vec![(head..head + n, head..head + m)];
    if n == 0 || m == 0 {
        return whole;
    }

    // `ends[d]` : pour chaque diagonale k = x - y, de -d à d, le x le plus loin
    // atteint en d différences.
    let offset = (n + m) as isize;
    let mut reach = vec![0isize; 2 * (n + m) + 2];
    let mut ends: Vec<Vec<isize>> = Vec::new();
    let mut found = None;
    'search: for d in 0..=(n + m).min(MAX_EDITS) as isize {
        for k in (-d..=d).step_by(2) {
            let at = (offset + k) as usize;
            let down = k == -d || (k != d && reach[at - 1] < reach[at + 1]);
            let mut x = if down { reach[at + 1] } else { reach[at - 1] + 1 };
            let mut y = x - k;
            while (x as usize) < n && (y as usize) < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            reach[at] = x;
            if x as usize >= n && y as usize >= m {
                ends.push(reach[(offset - d) as usize..=(offset + d) as usize].to_vec());
                found = Some(d);
                break 'search;
            }
        }
        ends.push(reach[(offset - d) as usize..=(offset + d) as usize].to_vec());
    }
    let Some(last) = found else { return whole };

    // Le chemin se remonte de la fin au début : chaque pas est une ligne ôtée
    // de `a` ou ajoutée de `b`, les diagonales entre deux pas sont communes.
    let mut steps: Vec<(usize, usize, bool)> = Vec::new();
    let (mut x, mut y) = (n as isize, m as isize);
    for d in (1..=last).rev() {
        let k = x - y;
        let before = &ends[(d - 1) as usize];
        let get = |k: isize| before[(k + d - 1) as usize];
        let down = k == -d || (k != d && get(k - 1) < get(k + 1));
        let from_k = if down { k + 1 } else { k - 1 };
        let from_x = get(from_k);
        let from_y = from_x - from_k;
        // Position du pas lui-même, avant la diagonale qui le suit.
        steps.push((from_x as usize, from_y as usize, down));
        (x, y) = (from_x, from_y);
    }
    steps.reverse();

    let mut out: Vec<(Range<usize>, Range<usize>)> = Vec::new();
    for (x, y, added) in steps {
        let (x, y) = (head + x, head + y);
        let (in_a, in_b) = if added { (x..x, y..y + 1) } else { (x..x + 1, y..y) };
        match out.last_mut() {
            // Deux pas qui se suivent sans ligne commune entre eux font un seul passage.
            Some(prev) if prev.0.end == in_a.start && prev.1.end == in_b.start => {
                prev.0.end = in_a.end;
                prev.1.end = in_b.end;
            }
            _ => out.push((in_a, in_b)),
        }
    }
    out
}

/// Nature d'un segment du document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Les trois versions sont identiques.
    Equal,
    /// Seul notre côté a changé : le changement est pris d'office.
    Left,
    /// Seul le côté qui arrive a changé : il est pris d'office.
    Right,
    /// Les deux côtés ont fait le même changement.
    Both,
    /// Les deux côtés ont changé le même passage, différemment.
    Conflict,
    /// Passage commun retouché à la main dans le résultat.
    Manual,
}

/// Côté d'où vient un changement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    pub fn other(self) -> Side {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }
}

/// Ce qu'il est advenu du changement d'un côté.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Conflit sur lequel rien n'est encore décidé.
    Pending,
    /// Ses lignes sont dans le résultat.
    Applied,
    /// Écarté.
    Ignored,
}

/// Un passage du document : ses lignes à gauche et à droite, par leur plage dans
/// chaque version, et celles du résultat, qui lui appartiennent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    pub kind: Kind,
    pub left: Range<usize>,
    pub right: Range<usize>,
    /// Lignes de la base : ce que vaut le résultat tant qu'un conflit attend.
    pub base: Vec<String>,
    pub lines: Vec<String>,
    pub left_state: State,
    pub right_state: State,
}

impl Segment {
    /// Conflit qui attend encore une décision.
    pub fn pending(&self) -> bool {
        self.kind == Kind::Conflict && (self.left_state == State::Pending || self.right_state == State::Pending)
    }

    /// Rangées qu'occupe le segment à l'écran : la hauteur de sa plus longue colonne.
    pub fn rows(&self) -> usize {
        self.left.len().max(self.right.len()).max(self.lines.len())
    }

    pub fn state(&self, side: Side) -> State {
        match side {
            Side::Left => self.left_state,
            Side::Right => self.right_state,
        }
    }

    /// Ce côté a changé le passage : c'est lui qu'on peut prendre ou écarter.
    pub fn changed(&self, side: Side) -> bool {
        match self.kind {
            Kind::Equal | Kind::Manual => false,
            Kind::Left => side == Side::Left,
            Kind::Right => side == Side::Right,
            Kind::Both | Kind::Conflict => true,
        }
    }

    fn state_mut(&mut self, side: Side) -> &mut State {
        match side {
            Side::Left => &mut self.left_state,
            Side::Right => &mut self.right_state,
        }
    }
}

/// Place dans le document : un segment, et une rangée de ce segment. Au-delà des
/// lignes du résultat, c'est une rangée vide en face de lignes de côté.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Pos {
    pub seg: usize,
    pub line: usize,
}

/// Rangée du document, telle qu'on la parcourt pour la dessiner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    pub row: usize,
    pub seg: usize,
    /// Rang de la rangée dans son segment.
    pub line: usize,
    /// Rang de sa ligne dans le résultat, si elle en a une.
    pub number: Option<usize>,
}

/// Découpe un texte en lignes, sans leur fin de ligne. Renvoie aussi si elles
/// finissent par CR LF, et si le texte se termine par une fin de ligne.
pub fn split_lines(text: &str) -> (Vec<String>, bool, bool) {
    let crlf = text.contains("\r\n");
    let newline = text.ends_with('\n');
    let mut lines: Vec<String> = text.split('\n').map(|line| line.strip_suffix('\r').unwrap_or(line).to_string()).collect();
    if newline || text.is_empty() {
        lines.pop();
    }
    (lines, crlf, newline)
}

/// État du résultat à un instant : de quoi y revenir. Les segments intacts sont
/// partagés d'un état à l'autre.
pub type Snapshot = Vec<Rc<Segment>>;

/// Le document d'une fusion : nos lignes, celles qui arrivent, et le résultat en segments.
#[derive(Clone, Debug)]
pub struct Doc {
    pub ours: Vec<String>,
    pub theirs: Vec<String>,
    pub segs: Vec<Rc<Segment>>,
    /// Les lignes du fichier finissent par CR LF.
    pub crlf: bool,
    newline: bool,
}

impl Doc {
    /// Fusionne les trois versions. Les changements d'un seul côté sont pris
    /// d'office, comme le fait git ; un conflit garde les lignes de la base.
    pub fn new(base: &str, ours: &str, theirs: &str) -> Doc {
        let (base, _, _) = split_lines(base);
        let (ours, crlf, newline) = split_lines(ours);
        let (theirs, _, their_newline) = split_lines(theirs);
        let mut segs: Vec<Rc<Segment>> = Vec::new();
        // Fin du dernier bloc, dans la base et dans chaque version.
        let (mut pos, mut left, mut right) = (0usize, 0usize, 0usize);
        for block in blocks(&base, &ours, &theirs) {
            if block.base.start > pos {
                segs.push(Rc::new(equal(&base[pos..block.base.start], left, right)));
            }
            let kind = match (block.left_changed, block.right_changed) {
                (true, false) => Kind::Left,
                (false, true) => Kind::Right,
                _ if ours[block.left.clone()] == theirs[block.right.clone()] => Kind::Both,
                _ => Kind::Conflict,
            };
            let lines = match kind {
                Kind::Right => theirs[block.right.clone()].to_vec(),
                Kind::Conflict => base[block.base.clone()].to_vec(),
                _ => ours[block.left.clone()].to_vec(),
            };
            let (left_state, right_state) = match kind {
                Kind::Left => (State::Applied, State::Ignored),
                Kind::Right => (State::Ignored, State::Applied),
                Kind::Conflict => (State::Pending, State::Pending),
                _ => (State::Applied, State::Applied),
            };
            (pos, left, right) = (block.base.end, block.left.end, block.right.end);
            let base = base[block.base].to_vec();
            segs.push(Rc::new(Segment { kind, left: block.left, right: block.right, base, lines, left_state, right_state }));
        }
        if pos < base.len() {
            segs.push(Rc::new(equal(&base[pos..], left, right)));
        }
        Doc { ours, theirs, segs, crlf, newline: newline || their_newline }
    }

    /// Lignes du résultat, dans l'ordre.
    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.segs.iter().flat_map(|seg| &seg.lines).map(String::as_str)
    }

    /// Texte du résultat, avec les fins de ligne du fichier d'origine.
    pub fn text(&self) -> String {
        let eol = if self.crlf { "\r\n" } else { "\n" };
        let lines: Vec<&str> = self.lines().collect();
        let mut text = lines.join(eol);
        if self.newline && !lines.is_empty() {
            text.push_str(eol);
        }
        text
    }

    /// Conflits sur lesquels rien n'est encore décidé.
    pub fn unresolved(&self) -> usize {
        self.segs.iter().filter(|seg| seg.pending()).count()
    }

    /// Nombre de rangées du document à l'écran.
    pub fn rows(&self) -> usize {
        self.segs.iter().map(|seg| seg.rows()).sum()
    }

    /// Première rangée d'un segment.
    pub fn seg_row(&self, seg: usize) -> usize {
        self.segs[..seg.min(self.segs.len())].iter().map(|seg| seg.rows()).sum()
    }

    pub fn row_of(&self, pos: Pos) -> usize {
        self.seg_row(pos.seg) + pos.line
    }

    /// Segment et rangée dans ce segment d'une rangée du document.
    pub fn at_row(&self, row: usize) -> Pos {
        let mut first = 0;
        for (seg, segment) in self.segs.iter().enumerate() {
            if row < first + segment.rows() {
                return Pos { seg, line: row - first };
            }
            first += segment.rows();
        }
        match self.segs.len() {
            0 => Pos::default(),
            len => Pos { seg: len - 1, line: self.segs[len - 1].rows().saturating_sub(1) },
        }
    }

    /// Ligne du résultat à cette place, s'il y en a une.
    pub fn line(&self, pos: Pos) -> Option<&str> {
        self.segs.get(pos.seg)?.lines.get(pos.line).map(String::as_str)
    }

    /// Rang, à partir de zéro, de la ligne du résultat à cette place ; sur une
    /// rangée vide, celui de la ligne qui suivrait.
    pub fn line_number(&self, pos: Pos) -> usize {
        let before: usize = self.segs[..pos.seg.min(self.segs.len())].iter().map(|seg| seg.lines.len()).sum();
        before + self.segs.get(pos.seg).map_or(0, |seg| pos.line.min(seg.lines.len()))
    }

    /// Place de la ligne du résultat de rang `number`.
    pub fn pos_of_line(&self, number: usize) -> Pos {
        let mut first = 0;
        for (seg, segment) in self.segs.iter().enumerate() {
            if number < first + segment.lines.len() {
                return Pos { seg, line: number - first };
            }
            first += segment.lines.len();
        }
        self.at_row(usize::MAX)
    }

    /// Les rangées du document à partir de la rangée `from`.
    pub fn walk(&self, from: usize) -> impl Iterator<Item = Row> + '_ {
        let rows = self.rows();
        let mut pos = self.at_row(from);
        let mut number = self.line_number(pos);
        let mut row = from;
        std::iter::from_fn(move || {
            if row >= rows {
                return None;
            }
            while pos.line >= self.segs[pos.seg].rows() {
                pos = Pos { seg: pos.seg + 1, line: 0 };
            }
            let has_line = pos.line < self.segs[pos.seg].lines.len();
            let out = Row { row, seg: pos.seg, line: pos.line, number: has_line.then_some(number) };
            number += usize::from(has_line);
            row += 1;
            pos.line += 1;
            Some(out)
        })
    }

    /// Un conflit qui se règle de lui-même, mot à mot : les deux côtés ont changé
    /// les mêmes lignes, mais pas les mêmes mots. Donne les lignes de la fusion.
    pub fn simple(&self, seg: usize) -> Option<Vec<String>> {
        let segment = self.segs.get(seg).filter(|segment| segment.pending())?;
        let join = |lines: &[String]| lines.join("\n");
        let ours = join(&self.ours[segment.left.clone()]);
        let theirs = join(&self.theirs[segment.right.clone()]);
        let merged = merge_words(&join(&segment.base), &ours, &theirs)?;
        Some(if merged.is_empty() { Vec::new() } else { merged.split('\n').map(str::to_string).collect() })
    }

    /// Règle un conflit en fusionnant mot à mot ; faux s'il n'a rien d'évident.
    pub fn merge_simple(&mut self, seg: usize) -> bool {
        let Some(lines) = self.simple(seg) else { return false };
        let segment = self.seg_mut(seg);
        segment.lines = lines;
        (segment.left_state, segment.right_state) = (State::Applied, State::Applied);
        self.prune();
        true
    }

    pub fn snapshot(&self) -> Snapshot {
        self.segs.clone()
    }

    pub fn restore(&mut self, snapshot: Snapshot) {
        self.segs = snapshot;
    }

    /// Le résultat est tel qu'à cet instant : aucun segment n'a été touché depuis.
    pub fn same_as(&self, snapshot: &Snapshot) -> bool {
        self.segs.len() == snapshot.len() && self.segs.iter().zip(snapshot).all(|(a, b)| Rc::ptr_eq(a, b))
    }

    fn seg_mut(&mut self, seg: usize) -> &mut Segment {
        Rc::make_mut(&mut self.segs[seg])
    }

    /// Détache une ligne d'un passage commun pour qu'on puisse la modifier, et
    /// renvoie sa nouvelle place : dans le bloc retouché qui la touche, s'il y en
    /// a un, sinon dans un bloc à elle. Ailleurs, rien ne change.
    fn isolate(&mut self, pos: Pos) -> Pos {
        if self.segs[pos.seg].kind != Kind::Equal {
            return pos;
        }
        let len = self.segs[pos.seg].lines.len();
        let manual = |seg: Option<&Rc<Segment>>| seg.is_some_and(|seg| seg.kind == Kind::Manual);
        if pos.line == 0 && pos.seg > 0 && manual(self.segs.get(pos.seg - 1)) {
            let from = self.seg_mut(pos.seg);
            let line = from.lines.remove(0);
            (from.left.start, from.right.start) = (from.left.start + 1, from.right.start + 1);
            if len == 1 {
                self.segs.remove(pos.seg);
            }
            let above = self.seg_mut(pos.seg - 1);
            (above.left.end, above.right.end) = (above.left.end + 1, above.right.end + 1);
            above.base.push(line.clone());
            above.lines.push(line);
            return Pos { seg: pos.seg - 1, line: above.lines.len() - 1 };
        }
        if pos.line + 1 == len && manual(self.segs.get(pos.seg + 1)) {
            let from = self.seg_mut(pos.seg);
            let line = from.lines.remove(len - 1);
            (from.left.end, from.right.end) = (from.left.end - 1, from.right.end - 1);
            let below = self.seg_mut(pos.seg + 1);
            (below.left.start, below.right.start) = (below.left.start - 1, below.right.start - 1);
            below.base.insert(0, line.clone());
            below.lines.insert(0, line);
            if len == 1 {
                self.segs.remove(pos.seg);
                return Pos { seg: pos.seg, line: 0 };
            }
            return Pos { seg: pos.seg + 1, line: 0 };
        }
        let whole = self.segs.remove(pos.seg);
        let (at, mut seg) = (pos.line, pos.seg);
        if at > 0 {
            self.segs.insert(seg, Rc::new(equal(&whole.lines[..at], whole.left.start, whole.right.start)));
            seg += 1;
        }
        let line = whole.lines[at].clone();
        let block = Segment {
            kind: Kind::Manual,
            left: whole.left.start + at..whole.left.start + at + 1,
            right: whole.right.start + at..whole.right.start + at + 1,
            base: vec![line.clone()],
            lines: vec![line],
            left_state: State::Applied,
            right_state: State::Applied,
        };
        self.segs.insert(seg, Rc::new(block));
        if at + 1 < whole.lines.len() {
            let rest = equal(&whole.lines[at + 1..], whole.left.start + at + 1, whole.right.start + at + 1);
            self.segs.insert(seg + 1, Rc::new(rest));
        }
        Pos { seg, line: 0 }
    }

    /// Réunit les segments de `first` à `last` en un seul bloc.
    fn merge(&mut self, first: usize, last: usize) {
        let parts: Vec<Rc<Segment>> = self.segs.drain(first..=last).collect();
        let state = |side: Side| {
            let waits = parts.iter().any(|part| part.kind == Kind::Conflict && part.state(side) == State::Pending);
            if waits { State::Pending } else { State::Applied }
        };
        let merged = Segment {
            kind: if parts.iter().any(|part| part.kind == Kind::Conflict) { Kind::Conflict } else { Kind::Manual },
            left: parts[0].left.start..parts[parts.len() - 1].left.end,
            right: parts[0].right.start..parts[parts.len() - 1].right.end,
            left_state: state(Side::Left),
            right_state: state(Side::Right),
            base: parts.iter().flat_map(|part| if part.kind == Kind::Equal { &part.lines } else { &part.base }).cloned().collect(),
            lines: parts.iter().flat_map(|part| &part.lines).cloned().collect(),
        };
        self.segs.insert(first, Rc::new(merged));
    }

    /// Ôte les blocs qui n'ont plus aucune ligne, dans aucune colonne.
    fn prune(&mut self) {
        self.segs.retain(|seg| seg.rows() > 0);
    }

    /// Place d'une ligne du résultat qu'on peut modifier : sur une rangée vide,
    /// une ligne est ajoutée à la fin du bloc.
    fn editable(&mut self, pos: Pos) -> Pos {
        if self.segs.is_empty() {
            self.segs.push(Rc::new(manual(0, 0)));
            return Pos::default();
        }
        let pos = self.isolate(pos);
        let seg = self.seg_mut(pos.seg);
        if pos.line >= seg.lines.len() {
            seg.lines.push(String::new());
            return Pos { seg: pos.seg, line: seg.lines.len() - 1 };
        }
        pos
    }

    /// Insère un caractère ; renvoie la place de la ligne et la colonne qui suit.
    pub fn insert_char(&mut self, pos: Pos, col: usize, ch: char) -> (Pos, usize) {
        let pos = self.editable(pos);
        let line = &mut self.seg_mut(pos.seg).lines[pos.line];
        let col = col.min(line.chars().count());
        line.insert(byte_of(line, col), ch);
        (pos, col + 1)
    }

    /// Supprime le caractère sous le curseur ; en fin de ligne, la suivante remonte.
    pub fn remove_char(&mut self, pos: Pos, col: usize) -> (Pos, usize) {
        let Some(len) = self.line(pos).map(|line| line.chars().count()) else { return (pos, col) };
        if col >= len {
            return match self.next_line(pos) {
                Some(next) => self.backspace(next, 0),
                None => (pos, col),
            };
        }
        let pos = self.isolate(pos);
        let line = &mut self.seg_mut(pos.seg).lines[pos.line];
        line.remove(byte_of(line, col));
        (pos, col)
    }

    /// Supprime le caractère avant le curseur ; en début de ligne, elle rejoint la précédente.
    pub fn backspace(&mut self, pos: Pos, col: usize) -> (Pos, usize) {
        if self.line(pos).is_none() {
            return (pos, col);
        }
        if col > 0 {
            let pos = self.isolate(pos);
            let line = &mut self.seg_mut(pos.seg).lines[pos.line];
            let col = col.min(line.chars().count());
            line.remove(byte_of(line, col - 1));
            return (pos, col - 1);
        }
        // La ligne du bas d'abord : détacher celle du haut ne change rien après elle,
        // et celle du bas se retrouve en la suivant.
        let low = self.isolate(pos);
        let Some(high) = self.prev_line(low) else { return (low, 0) };
        let high = self.isolate(high);
        let low = self.next_line(high).expect("the line below is still there");
        if high.seg != low.seg {
            self.merge(high.seg, low.seg);
        }
        let seg = self.seg_mut(high.seg);
        let below = seg.lines.remove(high.line + 1);
        let col = seg.lines[high.line].chars().count();
        seg.lines[high.line].push_str(&below);
        (high, col)
    }

    /// Coupe la ligne au curseur ; renvoie la place de la nouvelle ligne.
    pub fn split_line(&mut self, pos: Pos, col: usize) -> (Pos, usize) {
        let pos = self.editable(pos);
        let seg = self.seg_mut(pos.seg);
        let line = &mut seg.lines[pos.line];
        let rest = line.split_off(byte_of(line, col.min(line.chars().count())));
        seg.lines.insert(pos.line + 1, rest);
        (Pos { seg: pos.seg, line: pos.line + 1 }, 0)
    }

    /// Ouvre une ligne vide sous la rangée (`below`) ou au-dessus ; renvoie sa place.
    pub fn open_line(&mut self, pos: Pos, below: bool) -> Pos {
        if self.segs.is_empty() {
            self.segs.push(Rc::new(manual(0, 0)));
            return Pos::default();
        }
        if self.segs[pos.seg].kind != Kind::Equal {
            let lines = &mut self.seg_mut(pos.seg).lines;
            let at = (pos.line + usize::from(below)).min(lines.len());
            lines.insert(at, String::new());
            return Pos { seg: pos.seg, line: at };
        }
        // Dans un passage commun : la ligne rejoint le bloc retouché voisin, ou
        // s'ouvre dans un bloc à elle, glissé entre deux lignes communes.
        let whole = self.segs[pos.seg].clone();
        let cut = pos.line + usize::from(below);
        let manual_at = |segs: &[Rc<Segment>], seg: usize| segs.get(seg).is_some_and(|seg| seg.kind == Kind::Manual);
        if cut == 0 && pos.seg > 0 && manual_at(&self.segs, pos.seg - 1) {
            let above = self.seg_mut(pos.seg - 1);
            above.lines.push(String::new());
            return Pos { seg: pos.seg - 1, line: above.lines.len() - 1 };
        }
        if cut == whole.lines.len() && manual_at(&self.segs, pos.seg + 1) {
            self.seg_mut(pos.seg + 1).lines.insert(0, String::new());
            return Pos { seg: pos.seg + 1, line: 0 };
        }
        self.segs.remove(pos.seg);
        let mut seg = pos.seg;
        if cut > 0 {
            self.segs.insert(seg, Rc::new(equal(&whole.lines[..cut], whole.left.start, whole.right.start)));
            seg += 1;
        }
        self.segs.insert(seg, Rc::new(manual(whole.left.start + cut, whole.right.start + cut)));
        if cut < whole.lines.len() {
            let rest = equal(&whole.lines[cut..], whole.left.start + cut, whole.right.start + cut);
            self.segs.insert(seg + 1, Rc::new(rest));
        }
        Pos { seg, line: 0 }
    }

    /// Supprime une ligne du résultat ; faux s'il n'y en a pas à cette place.
    pub fn delete_line(&mut self, pos: Pos) -> bool {
        if self.line(pos).is_none() {
            return false;
        }
        let pos = self.isolate(pos);
        self.seg_mut(pos.seg).lines.remove(pos.line);
        self.prune();
        true
    }

    /// Prend les lignes d'un côté dans le résultat du bloc, à la place de ce qui
    /// s'y trouve. Dans un conflit où l'autre côté est déjà pris, elles
    /// s'ajoutent à sa suite : le résultat garde les deux. Faux hors d'un bloc.
    pub fn take(&mut self, seg: usize, side: Side) -> bool {
        let Some(segment) = self.segs.get(seg).filter(|segment| segment.kind != Kind::Equal) else { return false };
        let lines = match side {
            Side::Left => self.ours[segment.left.clone()].to_vec(),
            Side::Right => self.theirs[segment.right.clone()].to_vec(),
        };
        let segment = self.seg_mut(seg);
        let other = side.other();
        if segment.kind == Kind::Conflict && segment.state(other) == State::Applied && segment.state(side) != State::Applied {
            segment.lines.extend(lines);
        } else {
            segment.lines = lines;
            if segment.kind != Kind::Both {
                *segment.state_mut(other) = State::Ignored;
            }
        }
        *segment.state_mut(side) = State::Applied;
        self.prune();
        true
    }

    /// Tient un conflit pour réglé tel qu'il est : ce qui attendait est écarté.
    pub fn dismiss(&mut self, seg: usize) -> bool {
        if !self.segs.get(seg).is_some_and(|segment| segment.pending()) {
            return false;
        }
        let segment = self.seg_mut(seg);
        for side in [Side::Left, Side::Right] {
            if segment.state(side) == State::Pending {
                *segment.state_mut(side) = State::Ignored;
            }
        }
        true
    }

    /// Place de la ligne du résultat qui suit celle-ci.
    fn next_line(&self, pos: Pos) -> Option<Pos> {
        if pos.line + 1 < self.segs[pos.seg].lines.len() {
            return Some(Pos { seg: pos.seg, line: pos.line + 1 });
        }
        let seg = (pos.seg + 1..self.segs.len()).find(|&seg| !self.segs[seg].lines.is_empty())?;
        Some(Pos { seg, line: 0 })
    }

    /// Place de la ligne du résultat qui précède celle-ci.
    fn prev_line(&self, pos: Pos) -> Option<Pos> {
        if pos.line > 0 {
            return Some(Pos { seg: pos.seg, line: pos.line - 1 });
        }
        let seg = (0..pos.seg).rev().find(|&seg| !self.segs[seg].lines.is_empty())?;
        Some(Pos { seg, line: self.segs[seg].lines.len() - 1 })
    }
}

/// Un passage de la base qu'au moins un côté a changé, et ce que chaque côté met
/// à sa place.
struct Block {
    base: Range<usize>,
    left: Range<usize>,
    right: Range<usize>,
    left_changed: bool,
    right_changed: bool,
}

/// Les passages changés, dans l'ordre. Un bloc s'étend tant qu'un changement d'un
/// côté touche ce qu'il couvre déjà : les deux côtés s'y opposent alors.
fn blocks<T: PartialEq>(base: &[T], ours: &[T], theirs: &[T]) -> Vec<Block> {
    let ours_hunks = changes(base, ours);
    let theirs_hunks = changes(base, theirs);
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    // Décalage d'un élément de la base à sa place dans chaque version, hors des blocs.
    let (mut left_shift, mut right_shift) = (0isize, 0isize);
    let at = |pos: usize, shift: isize| (pos as isize + shift) as usize;
    loop {
        let start = match (ours_hunks.get(i), theirs_hunks.get(j)) {
            (None, None) => return out,
            (Some(a), None) => a.0.start,
            (None, Some(b)) => b.0.start,
            (Some(a), Some(b)) => a.0.start.min(b.0.start),
        };
        let (first_i, first_j, mut end) = (i, j, start);
        loop {
            let before = (i, j);
            while ours_hunks.get(i).is_some_and(|hunk| hunk.0.start <= end) {
                end = end.max(ours_hunks[i].0.end);
                i += 1;
            }
            while theirs_hunks.get(j).is_some_and(|hunk| hunk.0.start <= end) {
                end = end.max(theirs_hunks[j].0.end);
                j += 1;
            }
            if before == (i, j) {
                break;
            }
        }
        // Ce que chaque côté met à la place de la base, sur toute l'étendue du bloc.
        let span = |hunks: &[(Range<usize>, Range<usize>)], shift: isize| match (hunks.first(), hunks.last()) {
            (Some(first), Some(last)) => first.1.start - (first.0.start - start)..last.1.end + (end - last.0.end),
            _ => at(start, shift)..at(end, shift),
        };
        let left = span(&ours_hunks[first_i..i], left_shift);
        let right = span(&theirs_hunks[first_j..j], right_shift);
        left_shift = left.end as isize - end as isize;
        right_shift = right.end as isize - end as isize;
        out.push(Block { base: start..end, left, right, left_changed: i > first_i, right_changed: j > first_j });
    }
}

/// Au-delà de ce nombre de mots, un bloc est trop gros pour qu'on cherche à le
/// fusionner mot à mot.
const MAX_WORDS: usize = 20_000;

/// Découpe un texte en mots : une suite de lettres et de chiffres, une suite
/// d'espaces et de tabulations, ou tout autre caractère seul, le passage à la
/// ligne compris. Recollés, les mots rendent le texte d'origine.
fn words(text: &str) -> Vec<&str> {
    // Les blancs forment un mot à eux : un côté peut changer le retrait d'une
    // ligne sans toucher à ce que l'autre y a écrit.
    let kind = |c: char| match c {
        c if c.is_alphanumeric() || c == '_' => 1,
        ' ' | '\t' => 2,
        _ => 0,
    };
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(first) = rest.chars().next() {
        let len = match kind(first) {
            0 => first.len_utf8(),
            k => rest.chars().take_while(|&c| kind(c) == k).map(char::len_utf8).sum(),
        };
        let (word, tail) = rest.split_at(len);
        out.push(word);
        rest = tail;
    }
    out
}

/// Fusionne trois textes mot à mot, comme git le fait ligne à ligne : le
/// changement d'un seul côté est pris. `None` si les deux côtés ont changé les
/// mêmes mots, car il n'y a alors pas de fusion évidente.
pub fn merge_words(base: &str, ours: &str, theirs: &str) -> Option<String> {
    let (base, ours, theirs) = (words(base), words(ours), words(theirs));
    if base.len() + ours.len() + theirs.len() > MAX_WORDS {
        return None;
    }
    let mut out = String::new();
    let mut pos = 0;
    for block in blocks(&base, &ours, &theirs) {
        out.extend(base[pos..block.base.start].iter().copied());
        let taken = match (block.left_changed, block.right_changed) {
            (true, true) if ours[block.left.clone()] != theirs[block.right.clone()] => return None,
            (false, true) => &theirs[block.right],
            _ => &ours[block.left],
        };
        out.extend(taken.iter().copied());
        pos = block.base.end;
    }
    out.extend(base[pos..].iter().copied());
    Some(out)
}

/// Passage commun : mêmes lignes à gauche, à droite et dans le résultat.
fn equal(lines: &[String], left: usize, right: usize) -> Segment {
    Segment {
        kind: Kind::Equal,
        left: left..left + lines.len(),
        right: right..right + lines.len(),
        base: Vec::new(),
        lines: lines.to_vec(),
        left_state: State::Applied,
        right_state: State::Applied,
    }
}

/// Bloc ajouté à la main, sans rien en face : une seule ligne vide.
fn manual(left: usize, right: usize) -> Segment {
    Segment {
        kind: Kind::Manual,
        left: left..left,
        right: right..right,
        base: Vec::new(),
        lines: vec![String::new()],
        left_state: State::Applied,
        right_state: State::Applied,
    }
}

/// Octet où commence le caractère de rang `col`.
fn byte_of(line: &str, col: usize) -> usize {
    line.char_indices().nth(col).map_or(line.len(), |(at, _)| at)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(doc: &Doc) -> Vec<Kind> {
        doc.segs.iter().map(|seg| seg.kind).collect()
    }

    /// Applique les passages modifiés à `a` : on doit retrouver `b`.
    fn patched(a: &[&str], b: &[&str]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut at = 0;
        for (in_a, in_b) in changes(a, b) {
            out.extend(a[at..in_a.start].iter().map(|s| s.to_string()));
            out.extend(b[in_b].iter().map(|s| s.to_string()));
            at = in_a.end;
        }
        out.extend(a[at..].iter().map(|s| s.to_string()));
        out
    }

    #[test]
    fn changes_rebuild_the_other_text() {
        let cases: &[(&[&str], &[&str])] = &[
            (&[], &[]),
            (&["a"], &[]),
            (&[], &["a", "b"]),
            (&["a", "b", "c"], &["a", "c"]),
            (&["a", "b", "c"], &["a", "x", "y", "c", "d"]),
            (&["a", "b", "c", "a", "b", "b", "a"], &["c", "b", "a", "b", "a", "c"]),
            (&["x", "a", "b", "c", "y"], &["p", "a", "q", "c", "r"]),
        ];
        for (a, b) in cases {
            assert_eq!(patched(a, b), b.to_vec(), "{a:?} → {b:?}");
        }
        // Un seul passage, au plus court : la ligne du milieu.
        assert_eq!(changes(&["a", "b", "c"], &["a", "B", "c"]), vec![(1..2, 1..2)]);
        // Deux passages que sépare une ligne commune restent distincts.
        assert_eq!(changes(&["a", "b", "c"], &["A", "b", "C"]), vec![(0..1, 0..1), (2..3, 2..3)]);
    }

    #[test]
    fn an_added_block_starts_at_its_first_line() {
        // Une fonction ajoutée entre deux autres : c'est elle qui ressort, pas
        // l'accolade de la précédente suivie du début de la nouvelle.
        let a = ["fn a() {", "}", "", "fn c() {", "}"];
        let b = ["fn a() {", "}", "", "fn b() {", "}", "", "fn c() {", "}"];
        assert_eq!(changes(&a, &b), vec![(3..3, 3..6)]);
        assert_eq!(changes(&b, &a), vec![(3..6, 3..3)]);
        assert_eq!(patched(&a, &b), b);
        // En glissant, un ajout rejoint le changement qui le suit.
        assert_eq!(changes(&["x", "k"], &["x", "x", "K"]), vec![(1..2, 1..3)]);
    }

    #[test]
    fn long_texts_rebuild_too() {
        // Suites pseudo-aléatoires sur un petit alphabet : beaucoup de lignes communes.
        let mut seed = 12345u32;
        let mut next = move || {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (seed >> 16) % 7
        };
        for _ in 0..40 {
            let a: Vec<String> = (0..60).map(|_| next().to_string()).collect();
            let b: Vec<String> = (0..55).map(|_| next().to_string()).collect();
            let (a, b): (Vec<&str>, Vec<&str>) = (a.iter().map(String::as_str).collect(), b.iter().map(String::as_str).collect());
            assert_eq!(patched(&a, &b), b);
        }
    }

    const BASE: &str = "one\ntwo\nthree\nfour\nfive\nsix\nseven\n";

    #[test]
    fn one_sided_changes_are_taken() {
        // À gauche la ligne 2 change ; à droite la 6 disparaît et une ligne s'ajoute à la fin.
        let ours = "one\nTWO\nthree\nfour\nfive\nsix\nseven\n";
        let theirs = "one\ntwo\nthree\nfour\nfive\nseven\neight\n";
        let doc = Doc::new(BASE, ours, theirs);
        assert_eq!(kinds(&doc), [Kind::Equal, Kind::Left, Kind::Equal, Kind::Right, Kind::Equal, Kind::Right]);
        assert_eq!(doc.unresolved(), 0);
        assert_eq!(doc.text(), "one\nTWO\nthree\nfour\nfive\nseven\neight\n");
        // Chaque segment sait où sont ses lignes de chaque côté.
        assert_eq!((doc.segs[1].left.clone(), doc.segs[1].right.clone()), (1..2, 1..2));
        assert_eq!((doc.segs[3].left.clone(), doc.segs[3].right.clone()), (5..6, 5..5));
        assert_eq!((doc.segs[4].left.clone(), doc.segs[4].right.clone()), (6..7, 5..6));
        assert_eq!((doc.segs[5].left.clone(), doc.segs[5].right.clone()), (7..7, 6..7));
        // La ligne retirée garde sa rangée, face à celle de gauche.
        assert_eq!(doc.rows(), 8);
    }

    #[test]
    fn conflicts_wait_with_the_base() {
        let ours = "one\ntwo\nTHREE\nfour\nfive\nsix\nseven\n";
        let theirs = "one\ntwo\n3\n3 bis\nfour\nfive\nsix\nseven\n";
        let mut doc = Doc::new(BASE, ours, theirs);
        assert_eq!(kinds(&doc), [Kind::Equal, Kind::Conflict, Kind::Equal]);
        assert_eq!(doc.unresolved(), 1);
        assert_eq!(doc.segs[1].lines, ["three"]);
        assert_eq!(doc.segs[1].rows(), 2);

        // Notre côté, puis l'autre à la suite : le résultat garde les deux.
        assert!(doc.take(1, Side::Left));
        assert_eq!(doc.unresolved(), 0);
        assert_eq!(doc.segs[1].lines, ["THREE"]);
        assert_eq!(doc.segs[1].right_state, State::Ignored);
        assert!(doc.take(1, Side::Right));
        assert_eq!(doc.segs[1].lines, ["THREE", "3", "3 bis"]);
        // Repris seul, un côté remplace tout.
        assert!(doc.take(1, Side::Right));
        assert_eq!(doc.segs[1].lines, ["3", "3 bis"]);
        assert_eq!(doc.text(), "one\ntwo\n3\n3 bis\nfour\nfive\nsix\nseven\n");
        assert!(!doc.take(0, Side::Left), "un passage commun n'a rien à prendre");
    }

    #[test]
    fn same_change_on_both_sides_is_no_conflict() {
        let both = "one\n2\nthree\nfour\nfive\nsix\nseven\n";
        let doc = Doc::new(BASE, both, both);
        assert_eq!(kinds(&doc), [Kind::Equal, Kind::Both, Kind::Equal]);
        assert_eq!(doc.text(), both);
    }

    #[test]
    fn touching_changes_conflict() {
        // Deux changements voisins, sans ligne commune entre eux : git en fait un conflit.
        let ours = "one\nTWO\nthree\nfour\nfive\nsix\nseven\n";
        let theirs = "one\ntwo\nTHREE\nfour\nfive\nsix\nseven\n";
        let doc = Doc::new(BASE, ours, theirs);
        assert_eq!(kinds(&doc), [Kind::Equal, Kind::Conflict, Kind::Equal]);
        assert_eq!((doc.segs[1].left.clone(), doc.segs[1].right.clone()), (1..3, 1..3));
    }

    #[test]
    fn files_added_on_both_sides() {
        let doc = Doc::new("", "a\nb\n", "a\nc\n");
        assert_eq!(kinds(&doc), [Kind::Conflict]);
        assert!(doc.segs[0].lines.is_empty());
        assert_eq!(doc.rows(), 2);
    }

    #[test]
    fn dismissing_keeps_the_result() {
        let mut doc = Doc::new(BASE, "one\n2a\nthree\nfour\nfive\nsix\nseven\n", "one\n2b\nthree\nfour\nfive\nsix\nseven\n");
        assert!(doc.dismiss(1));
        assert_eq!(doc.unresolved(), 0);
        assert_eq!(doc.text(), BASE);
        assert!(!doc.dismiss(1));
    }

    #[test]
    fn typing_in_a_common_passage_makes_a_block() {
        let mut doc = Doc::new(BASE, BASE, BASE);
        assert_eq!(kinds(&doc), [Kind::Equal]);
        let (pos, col) = doc.insert_char(Pos { seg: 0, line: 2 }, 5, '!');
        assert_eq!((pos, col), (Pos { seg: 1, line: 0 }, 6));
        assert_eq!(kinds(&doc), [Kind::Equal, Kind::Manual, Kind::Equal]);
        assert_eq!(doc.segs[1].lines, ["three!"]);
        // La ligne voisine rejoint le même bloc.
        let (pos, _) = doc.insert_char(Pos { seg: 2, line: 0 }, 0, '>');
        assert_eq!(pos, Pos { seg: 1, line: 1 });
        assert_eq!(kinds(&doc), [Kind::Equal, Kind::Manual, Kind::Equal]);
        assert_eq!(doc.segs[1].lines, ["three!", ">four"]);
        assert_eq!((doc.segs[1].left.clone(), doc.segs[2].left.clone()), (2..4, 4..7));
        let (pos, _) = doc.insert_char(Pos { seg: 0, line: 1 }, 0, '<');
        assert_eq!(pos, Pos { seg: 1, line: 0 });
        assert_eq!(doc.text(), "one\n<two\nthree!\n>four\nfive\nsix\nseven\n");
        // Reprendre un côté défait les retouches.
        assert!(doc.take(1, Side::Left));
        assert_eq!(doc.text(), BASE);
    }

    #[test]
    fn lines_split_and_join() {
        let mut doc = Doc::new(BASE, BASE, BASE);
        let (pos, col) = doc.split_line(Pos { seg: 0, line: 1 }, 1);
        assert_eq!((pos, col), (Pos { seg: 1, line: 1 }, 0));
        assert_eq!(doc.text(), "one\nt\nwo\nthree\nfour\nfive\nsix\nseven\n");
        let (pos, col) = doc.backspace(pos, 0);
        assert_eq!((pos, col), (Pos { seg: 1, line: 0 }, 1));
        assert_eq!(doc.text(), BASE);
        // Au début d'un passage commun, la ligne rejoint la dernière du précédent.
        let (pos, col) = doc.backspace(Pos { seg: 2, line: 0 }, 0);
        assert_eq!((doc.line(pos), col), (Some("twothree"), 3));
        // En fin de ligne, la suppression fait remonter la suivante.
        let (pos, col) = doc.remove_char(pos, 8);
        assert_eq!((doc.line(pos), col), (Some("twothreefour"), 8));
        assert_eq!(doc.text(), "one\ntwothreefour\nfive\nsix\nseven\n");
        assert_eq!(doc.backspace(Pos { seg: 0, line: 0 }, 0), (Pos { seg: 0, line: 0 }, 0));
    }

    #[test]
    fn joining_across_a_conflict_keeps_it_pending() {
        let mut doc = Doc::new(BASE, "one\n2a\nthree\nfour\nfive\nsix\nseven\n", "one\n2b\nthree\nfour\nfive\nsix\nseven\n");
        // « three » remonte contre « two », la ligne de base du conflit.
        let (pos, col) = doc.backspace(Pos { seg: 2, line: 0 }, 0);
        assert_eq!((doc.line(pos), col), (Some("twothree"), 3));
        assert_eq!(kinds(&doc), [Kind::Equal, Kind::Conflict, Kind::Equal]);
        assert_eq!(doc.unresolved(), 1);
        assert_eq!((doc.segs[1].left.clone(), doc.segs[1].right.clone()), (1..3, 1..3));
    }

    #[test]
    fn opening_and_deleting_lines() {
        let mut doc = Doc::new(BASE, BASE, BASE);
        let pos = doc.open_line(Pos { seg: 0, line: 0 }, true);
        assert_eq!(pos, Pos { seg: 1, line: 0 });
        assert_eq!(doc.text(), "one\n\ntwo\nthree\nfour\nfive\nsix\nseven\n");
        assert_eq!((doc.segs[1].left.clone(), doc.segs[1].rows()), (1..1, 1));
        let pos = doc.open_line(Pos { seg: 0, line: 0 }, false);
        assert_eq!(pos, Pos { seg: 0, line: 0 });
        assert!(doc.text().starts_with("\none\n\ntwo"));
        // La ligne ouverte s'en va sans laisser de bloc vide.
        assert!(doc.delete_line(pos));
        assert!(doc.delete_line(Pos { seg: 1, line: 0 }));
        assert_eq!(doc.text(), BASE);
        assert_eq!(kinds(&doc), [Kind::Equal, Kind::Equal]);
        // Une ligne commune supprimée garde sa rangée, vide, face aux côtés.
        assert!(doc.delete_line(Pos { seg: 1, line: 0 }));
        assert_eq!(doc.text(), "one\nthree\nfour\nfive\nsix\nseven\n");
        assert_eq!(doc.rows(), 7);
        assert!(!doc.delete_line(Pos { seg: 1, line: 0 }));
    }

    #[test]
    fn typing_on_an_empty_row_adds_a_line() {
        // Le conflit n'a pas de ligne dans le résultat : la base est vide.
        let mut doc = Doc::new("", "a\n", "b\n");
        let (pos, col) = doc.insert_char(Pos { seg: 0, line: 0 }, 0, 'c');
        assert_eq!((pos, col, doc.text().as_str()), (Pos { seg: 0, line: 0 }, 1, "c\n"));
        assert_eq!(doc.unresolved(), 1);
    }

    #[test]
    fn snapshots_share_and_restore() {
        let mut doc = Doc::new(BASE, "one\n2a\nthree\nfour\nfive\nsix\nseven\n", "one\n2b\nthree\nfour\nfive\nsix\nseven\n");
        let before = doc.snapshot();
        assert!(doc.same_as(&before));
        doc.take(1, Side::Right);
        assert!(!doc.same_as(&before));
        // Seul le bloc touché a été copié.
        assert!(Rc::ptr_eq(&doc.segs[0], &before[0]) && !Rc::ptr_eq(&doc.segs[1], &before[1]));
        doc.restore(before);
        assert_eq!(doc.unresolved(), 1);
        assert_eq!(doc.text(), BASE);
    }

    #[test]
    fn rows_and_places() {
        let doc = Doc::new(BASE, "one\ntwo\nTHREE\nfour\nfive\nsix\nseven\n", "one\ntwo\n3\n3 bis\nfour\nfive\nsix\nseven\n");
        assert_eq!(doc.rows(), 8);
        assert_eq!(doc.at_row(3), Pos { seg: 1, line: 1 });
        assert_eq!(doc.row_of(Pos { seg: 2, line: 0 }), 4);
        assert_eq!(doc.at_row(99), Pos { seg: 2, line: 3 });
        // La rangée vide du conflit porte le rang de la ligne qui suivrait.
        assert_eq!(doc.line_number(Pos { seg: 1, line: 1 }), 3);
        assert_eq!(doc.pos_of_line(3), Pos { seg: 2, line: 0 });
    }

    #[test]
    fn walking_the_rows() {
        let doc = Doc::new(BASE, "one\ntwo\nTHREE\nfour\nfive\nsix\nseven\n", "one\ntwo\n3\n3 bis\nfour\nfive\nsix\nseven\n");
        let rows: Vec<Row> = doc.walk(1).take(4).collect();
        assert_eq!(rows[0], Row { row: 1, seg: 0, line: 1, number: Some(1) });
        assert_eq!(rows[1], Row { row: 2, seg: 1, line: 0, number: Some(2) });
        // La seconde rangée du conflit n'a pas de ligne dans le résultat.
        assert_eq!(rows[2], Row { row: 3, seg: 1, line: 1, number: None });
        assert_eq!(rows[3], Row { row: 4, seg: 2, line: 0, number: Some(3) });
        assert_eq!(doc.walk(0).count(), 8);
        assert_eq!(doc.walk(8).count(), 0);
    }

    #[test]
    fn words_cut_the_text_without_losing_it() {
        assert_eq!(words("let x = f(a);"), ["let", " ", "x", " ", "=", " ", "f", "(", "a", ")", ";"]);
        assert_eq!(words("\tif x {\n  y\n"), ["\t", "if", " ", "x", " ", "{", "\n", "  ", "y", "\n"]);
        for text in ["", "a", "  ", "été_1 + b", "a\n\nb"] {
            assert_eq!(words(text).concat(), text);
        }
    }

    #[test]
    fn words_merge_when_the_two_sides_touch_different_ones() {
        // Le début et la fin de la même ligne, chacun changé d'un côté.
        assert_eq!(merge_words("foo(a, b)", "foo(a, b, c)", "bar(a, b)").as_deref(), Some("bar(a, b, c)"));
        // Le même mot changé des deux côtés : rien d'évident.
        assert_eq!(merge_words("foo(a)", "foo(b)", "foo(c)"), None);
        // Un côté change le retrait, l'autre le texte.
        assert_eq!(merge_words("  x = 1;", "\tx = 1;", "  x = 2;").as_deref(), Some("\tx = 2;"));
        // Un côté supprime ce que l'autre modifie.
        assert_eq!(merge_words("a b c", "a c", "a B c"), None);
        // La limite du critère : deux mots différents de la même ligne se fusionnent,
        // même quand le résultat n'a pas de sens.
        assert_eq!(merge_words("let a = f(x);", "let a = g(x);", "let b = f(x);").as_deref(), Some("let b = g(x);"));
        // Sur plusieurs lignes, les lignes ajoutées d'un côté tiennent avec le mot changé de l'autre.
        assert_eq!(merge_words("a\nb", "a\nb\nc", "A\nb").as_deref(), Some("A\nb\nc"));
        assert_eq!(merge_words("", "a", "b"), None);
    }

    #[test]
    fn a_simple_conflict_merges_itself() {
        let base = "one\nlet total = sum(items);\nthree\n";
        let ours = "one\nlet total = sum(items, 0);\nthree\n";
        let theirs = "one\nlet count = sum(items);\nthree\n";
        let mut doc = Doc::new(base, ours, theirs);
        assert_eq!(kinds(&doc), [Kind::Equal, Kind::Conflict, Kind::Equal]);
        assert_eq!(doc.simple(1).as_deref(), Some(["let count = sum(items, 0);".to_string()].as_slice()));
        assert!(doc.merge_simple(1));
        assert_eq!(doc.text(), "one\nlet count = sum(items, 0);\nthree\n");
        // Le conflit est réglé, et les deux côtés sont tenus pour pris.
        assert_eq!(doc.unresolved(), 0);
        assert_eq!((doc.segs[1].kind, doc.segs[1].left_state), (Kind::Conflict, State::Applied));
        // Une fois réglé, il n'y a plus rien à fusionner ; prendre un côté reste possible.
        assert!(!doc.merge_simple(1) && doc.simple(1).is_none());
        assert!(doc.take(1, Side::Right));
        assert_eq!(doc.text(), "one\nlet count = sum(items);\nthree\n");

        // Les deux côtés sur le même mot : rien ne se fusionne de lui-même.
        let mut doc = Doc::new(base, "one\nlet total = sum(items, 0);\nthree\n", "one\nlet total = sum(items, 1);\nthree\n");
        assert!(doc.simple(1).is_none() && !doc.merge_simple(1));
        assert_eq!(doc.unresolved(), 1);
        // Hors d'un conflit, il n'y a rien à faire.
        assert!(!doc.merge_simple(0));
    }

    #[test]
    fn line_endings_are_kept() {
        let doc = Doc::new("a\r\nb\r\nc\r\n", "A\r\nb\r\nc\r\n", "a\r\nb\r\nc\r\nd\r\n");
        assert_eq!(doc.text(), "A\r\nb\r\nc\r\nd\r\n");
        let doc = Doc::new("a\nb", "a\nB", "a\nb");
        assert_eq!(doc.text(), "a\nB");
        assert_eq!(split_lines(""), (vec![], false, false));
        assert_eq!(split_lines("\n"), (vec![String::new()], false, true));
    }
}
