//! A small line-based 3-way merge (diff3), used by `dots adopt`.
//!
//! `base` is what dots last rendered, `ours` the repo source and `theirs` the edited
//! destination. A region only one side changed is taken from that side; a region both changed
//! differently is a [`Conflict`]. Nothing here writes anything.

/// One region both sides changed differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    /// 1-based first line of the region in `ours`.
    pub ours_line: usize,
    pub ours: String,
    pub theirs: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merged {
    /// The merge with conflicts written as `<<<<<<<` / `=======` / `>>>>>>>` blocks.
    pub text: String,
    pub conflicts: Vec<Conflict>,
}

impl Merged {
    pub fn is_clean(&self) -> bool {
        self.conflicts.is_empty()
    }
}

fn lines(s: &str) -> Vec<&str> {
    s.split_inclusive('\n').collect()
}

/// For each line of `a`, the index of its partner in `b` (longest common subsequence).
fn matches(a: &[&str], b: &[&str]) -> Vec<Option<usize>> {
    let mut out = vec![None; a.len()];
    let mut head = 0;
    while head < a.len() && head < b.len() && a[head] == b[head] {
        out[head] = Some(head);
        head += 1;
    }
    let (mut ea, mut eb) = (a.len(), b.len());
    while ea > head && eb > head && a[ea - 1] == b[eb - 1] {
        ea -= 1;
        eb -= 1;
        out[ea] = Some(eb);
    }
    let (ma, mb) = (&a[head..ea], &b[head..eb]);
    if ma.is_empty() || mb.is_empty() {
        return out;
    }
    let w = mb.len() + 1;
    let mut t = vec![0u32; (ma.len() + 1) * w];
    for i in (0..ma.len()).rev() {
        for j in (0..mb.len()).rev() {
            t[i * w + j] = if ma[i] == mb[j] {
                t[(i + 1) * w + j + 1] + 1
            } else {
                t[(i + 1) * w + j].max(t[i * w + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < ma.len() && j < mb.len() {
        if ma[i] == mb[j] {
            out[head + i] = Some(head + j);
            i += 1;
            j += 1;
        } else if t[(i + 1) * w + j] >= t[i * w + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// Merge `ours` and `theirs` against their common ancestor `base`.
pub fn merge3(base: &str, ours: &str, theirs: &str) -> Merged {
    if ours == theirs || base == theirs {
        return Merged {
            text: ours.to_string(),
            conflicts: vec![],
        };
    }
    if base == ours {
        return Merged {
            text: theirs.to_string(),
            conflicts: vec![],
        };
    }
    let (b, o, t) = (lines(base), lines(ours), lines(theirs));
    let (mo, mt) = (matches(&b, &o), matches(&b, &t));
    let mut text = String::new();
    let mut conflicts = Vec::new();
    let (mut ib, mut io, mut it) = (0, 0, 0);
    // Base lines both sides kept are the sync points; between two of them each side either
    // left the region alone or rewrote it.
    let mut k = 0;
    loop {
        while k < b.len() && !(mo[k].is_some() && mt[k].is_some()) {
            k += 1;
        }
        let (eo, et) = if k < b.len() {
            (mo[k].unwrap_or(o.len()), mt[k].unwrap_or(t.len()))
        } else {
            (o.len(), t.len())
        };
        let (cb, co, ct) = (&b[ib..k], &o[io..eo], &t[it..et]);
        if co == ct || ct == cb {
            co.iter().for_each(|l| text.push_str(l));
        } else if co == cb {
            ct.iter().for_each(|l| text.push_str(l));
        } else {
            let (so, st) = (co.concat(), ct.concat());
            conflicts.push(Conflict {
                ours_line: io + 1,
                ours: so.clone(),
                theirs: st.clone(),
            });
            let nl = |s: &str| {
                if s.is_empty() || s.ends_with('\n') {
                    ""
                } else {
                    "\n"
                }
            };
            text.push_str(&format!("<<<<<<< repo source\n{so}{}", nl(&so)));
            text.push_str(&format!("=======\n{st}{}>>>>>>> edited file\n", nl(&st)));
        }
        if k >= b.len() {
            break;
        }
        text.push_str(o[eo]);
        ib = k + 1;
        io = eo + 1;
        it = et + 1;
        k += 1;
    }
    Merged { text, conflicts }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_the_side_that_changed() {
        let m = merge3("a\nb\nc\n", "a\nb\nc\n", "a\nB\nc\n");
        assert_eq!(m.text, "a\nB\nc\n");
        let m = merge3("a\nb\nc\n", "a\nb\nC\n", "a\nb\nc\n");
        assert_eq!(m.text, "a\nb\nC\n");
    }

    #[test]
    fn disjoint_edits_both_survive() {
        let m = merge3(
            "x\n1\ny\nz\n2\nw\n",
            "x\n{{ one }}\ny\nz\n2\nw\n",
            "x\n1\ny\nz\n22\nw\n",
        );
        assert!(m.is_clean(), "{m:?}");
        assert_eq!(m.text, "x\n{{ one }}\ny\nz\n22\nw\n");
    }

    #[test]
    fn same_edit_on_both_sides_is_not_a_conflict() {
        let m = merge3("a\nb\n", "a\nX\n", "a\nX\n");
        assert!(m.is_clean());
        assert_eq!(m.text, "a\nX\n");
    }

    #[test]
    fn overlapping_edits_conflict_with_markers() {
        let m = merge3("a\nb\nc\n", "a\nours\nc\n", "a\ntheirs\nc\n");
        assert_eq!(m.conflicts.len(), 1);
        assert_eq!(m.conflicts[0].ours_line, 2);
        assert_eq!(
            m.text,
            "a\n<<<<<<< repo source\nours\n=======\ntheirs\n>>>>>>> edited file\nc\n"
        );
    }

    #[test]
    fn insertions_and_deletions() {
        let m = merge3("a\nb\nc\nd\n", "a\nb\nc\nd\n", "a\nc\nd\ne\n");
        assert_eq!(m.text, "a\nc\nd\ne\n");
        let m = merge3("a\nb\nc\nd\ne\n", "a\nb\nc\nd\nE\n", "A\nb\nc\nd\ne\n");
        assert_eq!(m.text, "A\nb\nc\nd\nE\n");
    }

    #[test]
    fn missing_trailing_newline() {
        let m = merge3("a\nb", "a\nb", "a\nb\nc");
        assert_eq!(m.text, "a\nb\nc");
    }
}
