//! `ml.wer` (jiwer behaviour), Python's `difflib.SequenceMatcher` and the pipeline's word
//! coverage checks (`_word_coverage`, `_final_word_missing`, `_first_word_missing`).

use super::{normalize_for_wer, py};
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::LazyLock;

/// `difflib.SequenceMatcher(None, a, b, autojunk)`: no junk function, so only the
/// "popular element" rule can drop elements of `b` from the index.
pub struct SequenceMatcher<'a, T> {
    a: &'a [T],
    b: &'a [T],
    b2j: HashMap<&'a T, Vec<usize>>,
}

impl<'a, T: Eq + Hash> SequenceMatcher<'a, T> {
    pub fn new(a: &'a [T], b: &'a [T], autojunk: bool) -> Self {
        let mut b2j: HashMap<&T, Vec<usize>> = HashMap::new();
        for (i, elt) in b.iter().enumerate() {
            b2j.entry(elt).or_default().push(i);
        }
        let n = b.len();
        if autojunk && n >= 200 {
            let ntest = n / 100 + 1;
            let popular: HashSet<&T> = b2j.iter().filter(|(_, idx)| idx.len() > ntest).map(|(&e, _)| e).collect();
            b2j.retain(|e, _| !popular.contains(e));
        }
        SequenceMatcher { a, b, b2j }
    }

    /// `find_longest_match(alo, ahi, blo, bhi)` -> (i, j, size); earliest i, then earliest j.
    pub fn find_longest_match(&self, alo: usize, ahi: usize, blo: usize, bhi: usize) -> (usize, usize, usize) {
        let (a, b) = (self.a, self.b);
        let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0);
        let mut j2len: HashMap<usize, usize> = HashMap::new();
        for i in alo..ahi {
            let mut newj2len = HashMap::new();
            if let Some(js) = self.b2j.get(&a[i]) {
                for &j in js {
                    if j < blo {
                        continue;
                    }
                    if j >= bhi {
                        break;
                    }
                    let k = j.checked_sub(1).and_then(|p| j2len.get(&p)).copied().unwrap_or(0) + 1;
                    newj2len.insert(j, k);
                    if k > bestsize {
                        (besti, bestj, bestsize) = (i + 1 - k, j + 1 - k, k);
                    }
                }
            }
            j2len = newj2len;
        }
        // Extend with elements the index dropped (popular ones); there is no junk.
        while besti > alo && bestj > blo && a[besti - 1] == b[bestj - 1] {
            (besti, bestj, bestsize) = (besti - 1, bestj - 1, bestsize + 1);
        }
        while besti + bestsize < ahi && bestj + bestsize < bhi && a[besti + bestsize] == b[bestj + bestsize] {
            bestsize += 1;
        }
        (besti, bestj, bestsize)
    }

    /// `get_matching_blocks()`: difflib's exact algorithm (LIFO queue, sort, merge of adjacent
    /// blocks), ending with the `(len(a), len(b), 0)` block.
    pub fn get_matching_blocks(&self) -> Vec<(usize, usize, usize)> {
        let (la, lb) = (self.a.len(), self.b.len());
        let mut queue = vec![(0, la, 0, lb)];
        let mut blocks = Vec::new();
        while let Some((alo, ahi, blo, bhi)) = queue.pop() {
            let (i, j, k) = self.find_longest_match(alo, ahi, blo, bhi);
            if k > 0 {
                blocks.push((i, j, k));
                if alo < i && blo < j {
                    queue.push((alo, i, blo, j));
                }
                if i + k < ahi && j + k < bhi {
                    queue.push((i + k, ahi, j + k, bhi));
                }
            }
        }
        blocks.sort();
        let (mut i1, mut j1, mut k1) = (0, 0, 0);
        let mut out = Vec::new();
        for (i2, j2, k2) in blocks {
            if i1 + k1 == i2 && j1 + k1 == j2 {
                k1 += k2;
            } else {
                if k1 > 0 {
                    out.push((i1, j1, k1));
                }
                (i1, j1, k1) = (i2, j2, k2);
            }
        }
        if k1 > 0 {
            out.push((i1, j1, k1));
        }
        out.push((la, lb, 0));
        out
    }

    /// The total size of the matching blocks.
    pub fn matches(&self) -> usize {
        self.get_matching_blocks().iter().map(|b| b.2).sum()
    }

    pub fn ratio(&self) -> f64 {
        let length = self.a.len() + self.b.len();
        if length == 0 {
            return 1.0;
        }
        2.0 * self.matches() as f64 / length as f64
    }
}

/// `difflib.SequenceMatcher(None, a, b).ratio()` on two strings (code points).
pub fn ratio(a: &str, b: &str) -> f64 {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    SequenceMatcher::new(&a, &b, true).ratio()
}

/// Unit-cost edit distance, which is what jiwer's opcodes count as S + D + I.
fn edit_distance<T: PartialEq>(r: &[T], h: &[T]) -> usize {
    let mut row: Vec<usize> = (0..=h.len()).collect();
    for (i, x) in r.iter().enumerate() {
        let mut diag = row[0];
        row[0] = i + 1;
        for (j, y) in h.iter().enumerate() {
            let sub = diag + usize::from(x != y);
            diag = row[j + 1];
            row[j + 1] = sub.min(row[j] + 1).min(diag + 1);
        }
    }
    row[h.len()]
}

static UNBRACKET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[()\[\]{}]").unwrap());
static MULTI_SPACE: LazyLock<Regex> = LazyLock::new(|| py::re(r"[{SPACE}][{SPACE}]+"));

/// jiwer's default word transform: RemoveMultipleSpaces, Strip, split on " ".
fn jiwer_words(s: &str) -> Vec<String> {
    let s = MULTI_SPACE.replace_all(s, " ");
    py::strip(&s).split(' ').filter(|w| !w.is_empty()).map(String::from).collect()
}

/// Word error rate, forgiving of spelling/format variants that sound identical.
///
/// Whisper's normalizer drops (parenthesised) text, so brackets become plain words;
/// and a spacing-insensitive character error rate catches "hide-working" vs
/// "hideworking" and similar, which are not real mistakes.
pub fn wer(reference: &str, hypothesis: &str) -> f64 {
    let r = normalize_for_wer(&UNBRACKET.replace_all(reference, " "));
    let h = normalize_for_wer(&UNBRACKET.replace_all(hypothesis, " "));
    if py::strip(&r).is_empty() {
        return if py::strip(&h).is_empty() { 0.0 } else { 1.0 };
    }
    if py::strip(&h).is_empty() {
        return 1.0;
    }
    let (rw, hw) = (jiwer_words(&r), jiwer_words(&h));
    let w = edit_distance(&rw, &hw) as f64 / rw.len() as f64;
    let rc: Vec<char> = py::strip(&r.replace(' ', "")).chars().collect();
    let hc: Vec<char> = py::strip(&h.replace(' ', "")).chars().collect();
    let c = edit_distance(&rc, &hc) as f64 / rc.len() as f64;
    w.min(c * 2.0)
}

/// Align the script and transcript, consuming each heard token only once.
///
/// A similar preceding word ("come") cannot also stand in for a missing "home".
/// Name spellings and splits ("Scadutree" / "Skadu tree") remain tolerated.
/// This is token coverage, not proof that ASR heard every phoneme correctly.
pub fn word_coverage(text: &str, heard: &str) -> Vec<bool> {
    let (exp_s, got_s) = (normalize_for_wer(text), normalize_for_wer(heard));
    let (exp, got) = (py::split(&exp_s), py::split(&got_s));
    let (n, m) = (exp.len(), got.len());
    let mut costs = vec![vec![f64::INFINITY; m + 1]; n + 1];
    let mut prev: Vec<Vec<Option<(usize, usize, bool)>>> = vec![vec![None; m + 1]; n + 1];
    costs[0][0] = 0.0;
    let mut offer = |costs: &mut Vec<Vec<f64>>, i: usize, j: usize, ni: usize, nj: usize, cost: f64, covered: bool| {
        let value = costs[i][j] + cost;
        if value < costs[ni][nj] {
            costs[ni][nj] = value;
            prev[ni][nj] = Some((i, j, covered));
        }
    };
    for i in 0..=n {
        for j in 0..=m {
            if i < n && j < m {
                for k in 1..=3.min(m - j) {
                    let similarity = ratio(exp[i], &got[j..j + k].concat());
                    if k == 1 || similarity >= 0.6 {
                        let cost = 1.0 - similarity + 0.02 * (k - 1) as f64;
                        offer(&mut costs, i, j, i + 1, j + k, cost, similarity >= 0.6);
                    }
                }
            }
            if i < n {
                offer(&mut costs, i, j, i + 1, j, 1.0, false);
            }
            if j < m {
                offer(&mut costs, i, j, i, j + 1, 1.0, false);
            }
        }
    }
    let mut covered = vec![false; n];
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        let (pi, pj, matched) = prev[i][j].expect("every cell is reachable");
        if i > pi {
            covered[pi] = matched;
        }
        (i, j) = (pi, pj);
    }
    covered
}

pub fn final_word_missing(text: &str, heard: &str) -> bool {
    word_coverage(text, heard).last().is_some_and(|&c| !c)
}

pub fn first_word_missing(text: &str, heard: &str) -> bool {
    word_coverage(text, heard).first().is_some_and(|&c| !c)
}
