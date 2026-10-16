use similar::{Algorithm, DiffTag, capture_diff_slices};
use std::ops::Range;

const CONTEXT: usize = 3;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineKind {
    Context,
    /// Only in the local file.
    Local,
    /// Only in the remote file.
    Remote,
}

#[derive(Clone, Debug)]
pub struct Hunk {
    pub local: Range<usize>,
    pub remote: Range<usize>,
    /// Lines to show, with their index in the local file (context and
    /// local lines) or in the remote file (remote lines).
    pub lines: Vec<(LineKind, usize)>,
}

impl Hunk {
    pub fn header(&self) -> String {
        format!(
            "@@ local {},{}  remote {},{} @@",
            self.local.start + 1,
            self.local.len(),
            self.remote.start + 1,
            self.remote.len()
        )
    }
}

fn lines(s: &str) -> Vec<&str> {
    s.split_inclusive('\n').collect()
}

/// Line diff from `local` (old) to `remote` (new). Every run of changed
/// lines is its own hunk, so each change can be taken or pushed on its own;
/// surrounding context is shown but is not part of the hunk.
pub fn hunks(local: &str, remote: &str) -> Vec<Hunk> {
    let a = lines(local);
    let b = lines(remote);
    let ops: Vec<_> = capture_diff_slices(Algorithm::Myers, &a, &b).iter().map(|op| op.as_tag_tuple()).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < ops.len() {
        if ops[i].0 == DiffTag::Equal {
            i += 1;
            continue;
        }
        let first = i;
        while i < ops.len() && ops[i].0 != DiffTag::Equal {
            i += 1;
        }
        let local = ops[first].1.start..ops[i - 1].1.end;
        let remote = ops[first].2.start..ops[i - 1].2.end;
        let mut lines = Vec::new();
        lines.extend((local.start.saturating_sub(CONTEXT)..local.start).map(|i| (LineKind::Context, i)));
        lines.extend(local.clone().map(|i| (LineKind::Local, i)));
        lines.extend(remote.clone().map(|i| (LineKind::Remote, i)));
        lines.extend((local.end..(local.end + CONTEXT).min(a.len())).map(|i| (LineKind::Context, i)));
        out.push(Hunk { local, remote, lines });
    }
    out
}

/// Local text with this hunk replaced by the remote version.
pub fn take_remote(local: &str, remote: &str, h: &Hunk) -> String {
    let a = lines(local);
    let b = lines(remote);
    let mut s = String::with_capacity(local.len() + remote.len());
    a[..h.local.start].iter().chain(&b[h.remote.clone()]).chain(&a[h.local.end..]).for_each(|l| s.push_str(l));
    s
}

/// Remote text with this hunk replaced by the local version.
pub fn take_local(local: &str, remote: &str, h: &Hunk) -> String {
    let a = lines(local);
    let b = lines(remote);
    let mut s = String::with_capacity(local.len() + remote.len());
    b[..h.remote.start].iter().chain(&a[h.local.clone()]).chain(&b[h.remote.end..]).for_each(|l| s.push_str(l));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const L: &str = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm\n";
    const R: &str = "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\nk\nL\nm\nn\n";

    #[test]
    fn hunks_are_split() {
        assert_eq!(hunks(L, R).len(), 3);
        assert!(hunks(L, L).is_empty());
    }

    #[test]
    fn taking_every_hunk_converges() {
        let mut local = L.to_string();
        while let Some(h) = hunks(&local, R).first().cloned() {
            local = take_remote(&local, R, &h);
        }
        assert_eq!(local, R);

        let mut remote = R.to_string();
        while let Some(h) = hunks(L, &remote).last().cloned() {
            remote = take_local(L, &remote, &h);
        }
        assert_eq!(remote, L);
    }

    #[test]
    fn close_changes_are_separate_hunks() {
        let hs = hunks("a\nb\nc\nd\n", "A\nb\nC\nd\n");
        assert_eq!(hs.len(), 2);
        assert_eq!(take_remote("a\nb\nc\nd\n", "A\nb\nC\nd\n", &hs[1]), "a\nb\nC\nd\n");
    }

    #[test]
    fn single_hunk_only() {
        let hs = hunks(L, R);
        let out = take_remote(L, R, &hs[0]);
        assert!(out.contains("B\n") && !out.contains("L\n"));
    }
}
