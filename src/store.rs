use std::cmp::Reverse;
use std::collections::BinaryHeap;

use rayon::prelude::*;
use regex::Regex;

use crate::filter::Matcher;

pub const HAS_NAME: u8 = 1;
pub const IS_DIR: u8 = 2;
pub const HAS_SIZE: u8 = 4;
/// Size came from $DATA of an extension record and must not be overwritten by the base record's fallback.
pub const EXT_SIZE: u8 = 8;

const ROOT_FRN: usize = 5;
const MAX_DEPTH: usize = 1024;

/// MFT record; the FRN is the index in the vector.
#[derive(Clone, Copy, Default)]
pub struct FileEntry {
    pub parent: u32,
    pub name_off: u32,
    pub size: u64,
    pub flags: u32,
    pub name_len: u16,
    pub state: u8,
}

pub struct Store {
    pub entries: Vec<FileEntry>,
    pub names: Vec<u16>,
    pub drive: char,
    /// Entries removed from every query by `--exclude`/`--exclude-meta`; empty means none.
    pub hidden: Vec<bool>,
    /// Entries smaller than this are left out of the result lists (`--min-size`).
    pub min_size: u64,
}

pub struct Summary {
    pub files: u64,
    pub dirs: u64,
    pub files_size: u64,
    pub top_roots: Vec<Row>,
}

pub struct Row {
    pub path: String,
    pub size: u64,
    pub is_dir: bool,
    /// Nesting level in tree mode, otherwise 0.
    pub depth: usize,
}

/// Children of each entry in CSR format: `list[start[i]..start[i + 1]]`.
struct Children {
    start: Vec<u32>,
    list: Vec<u32>,
}

impl Children {
    fn of(&self, idx: usize) -> &[u32] {
        match (self.start.get(idx), self.start.get(idx + 1)) {
            (Some(&a), Some(&b)) => &self.list[a as usize..b as usize],
            _ => &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Store {
        let mut names: Vec<u16> = Vec::new();
        let mut entries = vec![FileEntry::default(); 10];
        let mut add = |idx: usize, name: &str, parent: u32, size: u64, dir: bool| {
            let off = names.len() as u32;
            names.extend(name.encode_utf16());
            let state = HAS_NAME | HAS_SIZE | if dir { IS_DIR } else { 0 };
            entries[idx] =
                FileEntry { parent, name_off: off, size, flags: 0, name_len: name.len() as u16, state };
        };
        add(5, ".", 5, 0, true);
        add(6, "a", 5, 0, true);
        add(7, "f.txt", 6, 100, false);
        add(8, "b", 6, 0, true);
        add(9, "g.bin", 8, 50, false);
        Store { entries, names, drive: 'C', hidden: Vec::new(), min_size: 0 }
    }

    #[test]
    fn dir_sizes_roll_up() {
        let s = sample();
        let sizes = s.dir_sizes();
        assert_eq!((sizes[5], sizes[6], sizes[8]), (150, 150, 50));
    }

    #[test]
    fn tree_walks_by_path() {
        let s = sample();
        let rows = s.tree("C:\\A", 2, 10, true).unwrap();
        let got: Vec<_> = rows.iter().map(|r| (r.path.as_str(), r.size, r.depth)).collect();
        assert_eq!(
            got,
            [("C:\\a", 150, 0), ("C:\\a\\f.txt", 100, 1), ("C:\\a\\b", 50, 1), ("C:\\a\\b\\g.bin", 50, 2)]
        );
        assert!(s.tree("C:\\nope", 1, 10, true).is_none());
        let full = s.tree("C:\\", usize::MAX, usize::MAX, false).unwrap();
        assert_eq!(full.len(), 5);
        assert_eq!(full[2].path, "f.txt");
    }

    #[test]
    fn exclusions_and_min_size() {
        let mut s = sample();
        s.apply_exclusions(&[crate::filter::parse_pattern("b").unwrap()], false);
        assert_eq!(s.dir_sizes()[6], 100);
        s.min_size = 101;
        assert!(s.top(10).is_empty());

        // Metafile hiding only touches root-level `$...` and `System Volume Information`.
        let mut s = sample();
        let off = s.names.len() as u32;
        s.names.extend("$Extend".encode_utf16());
        s.entries[6].name_off = off;
        s.entries[6].name_len = 7;
        s.apply_exclusions(&[], true);
        assert_eq!(s.dir_sizes()[5], 0);
        assert!(!s.visible(6) && !s.visible(7));

        let mut s = sample();
        s.apply_exclusions(&[], true);
        assert_eq!(s.dir_sizes()[5], 150);
    }

    #[test]
    fn summary_counts() {
        let s = sample().summary(5);
        assert_eq!((s.files, s.dirs, s.files_size), (2, 2, 150));
        assert_eq!(s.top_roots.len(), 1);
        assert_eq!(s.top_roots[0].size, 150);
    }

    #[test]
    fn broken_parent_cycle_terminates() {
        let mut s = sample();
        s.entries[6].parent = 8;
        let _ = s.dir_sizes();
        let _ = s.path_of(9);
    }
}

/// The `n` largest (size, index) pairs in descending size order.
fn top_n(items: impl Iterator<Item = (u64, usize)>, n: usize) -> Vec<(u64, usize)> {
    if n >= 4096 {
        let mut v: Vec<_> = items.collect();
        v.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        v.truncate(n);
        return v;
    }
    let mut heap: BinaryHeap<Reverse<(u64, usize)>> = BinaryHeap::with_capacity(n + 1);
    for (size, i) in items {
        if heap.len() < n {
            heap.push(Reverse((size, i)));
        } else if let Some(Reverse((min, _))) = heap.peek() {
            if size > *min {
                heap.pop();
                heap.push(Reverse((size, i)));
            }
        }
    }
    let mut v: Vec<_> = heap.into_iter().map(|Reverse(x)| x).collect();
    v.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    v
}

impl Store {
    fn name(&self, e: &FileEntry) -> &[u16] {
        let start = e.name_off as usize;
        self.names.get(start..start + e.name_len as usize).unwrap_or(&[])
    }

    fn visible(&self, idx: usize) -> bool {
        idx != ROOT_FRN
            && self.entries[idx].state & HAS_NAME != 0
            && !self.hidden.get(idx).copied().unwrap_or(false)
    }

    /// Hides entries matching `patterns` (and everything below them); with `hide_meta`, also the
    /// NTFS metafiles in the volume root (`$MFT`, `$LogFile`, `$Recycle.Bin`, `System Volume Information`).
    pub fn apply_exclusions(&mut self, patterns: &[Matcher], hide_meta: bool) {
        if patterns.is_empty() && !hide_meta {
            return;
        }
        let children = self.children_index();
        let need_path = patterns.iter().any(Matcher::needs_path);
        let mut hidden = vec![false; self.entries.len()];
        let mut stack = vec![(ROOT_FRN, format!("{}:", self.drive), false)];
        while let Some((idx, path, inherited)) = stack.pop() {
            let mut hide = inherited;
            if idx != ROOT_FRN && !hide {
                let name = String::from_utf16_lossy(self.name(&self.entries[idx]));
                let in_root = self.entries[idx].parent as usize == ROOT_FRN;
                hide = (hide_meta
                    && in_root
                    && (name.starts_with('$') || name.eq_ignore_ascii_case("System Volume Information")))
                    || patterns.iter().any(|m| m.is_match(&name, &path));
            }
            hidden[idx] = hide;
            for &c in children.of(idx) {
                let c = c as usize;
                let child_path = if need_path {
                    format!("{path}\\{}", String::from_utf16_lossy(self.name(&self.entries[c])))
                } else {
                    String::new()
                };
                stack.push((c, child_path, hide));
            }
        }
        self.hidden = hidden;
    }

    pub fn summary(&self, roots: usize) -> Summary {
        let sizes = self.dir_sizes();
        let children = self.children_index();
        let (mut files, mut dirs, mut files_size) = (0u64, 0u64, 0u64);
        for i in (0..self.entries.len()).filter(|&i| self.visible(i)) {
            if self.entries[i].state & IS_DIR != 0 {
                dirs += 1;
            } else {
                files += 1;
                files_size = files_size.saturating_add(self.entries[i].size);
            }
        }
        let kids = children
            .of(ROOT_FRN)
            .iter()
            .map(|&c| c as usize)
            .filter(|&c| self.entries[c].state & IS_DIR != 0)
            .map(|c| (sizes[c], c));
        let top_roots = top_n(kids, roots)
            .into_iter()
            .map(|(size, i)| Row { size, ..self.row(i) })
            .collect();
        Summary { files, dirs, files_size, top_roots }
    }

    /// Builds the full path from leaf to root; a broken chain is marked with `?`.
    pub fn path_of(&self, idx: usize) -> String {
        let mut parts: Vec<String> = Vec::new();
        let mut cur = idx;
        let mut broken = false;
        for _ in 0..MAX_DEPTH {
            let Some(e) = self.entries.get(cur) else {
                broken = true;
                break;
            };
            if cur == ROOT_FRN {
                break;
            }
            if e.state & HAS_NAME == 0 {
                broken = true;
                break;
            }
            parts.push(String::from_utf16_lossy(self.name(e)));
            let parent = e.parent as usize;
            if parent == cur {
                broken = true;
                break;
            }
            cur = parent;
        }
        if parts.len() == MAX_DEPTH {
            broken = true;
        }
        let mut out = format!("{}:", self.drive);
        if broken {
            out.push_str("\\?");
        }
        for p in parts.iter().rev() {
            out.push('\\');
            out.push_str(p);
        }
        out
    }

    fn row(&self, idx: usize) -> Row {
        let e = &self.entries[idx];
        Row { path: self.path_of(idx), size: e.size, is_dir: e.state & IS_DIR != 0, depth: 0 }
    }

    /// Size of each folder including everything nested; the index is the FRN.
    fn dir_sizes(&self) -> Vec<u64> {
        let mut sizes = vec![0u64; self.entries.len()];
        for i in self.files() {
            let size = self.entries[i].size;
            let mut cur = self.entries[i].parent as usize;
            for _ in 0..MAX_DEPTH {
                let Some(e) = self.entries.get(cur) else { break };
                if cur != ROOT_FRN && e.state & HAS_NAME == 0 {
                    break;
                }
                sizes[cur] = sizes[cur].saturating_add(size);
                if cur == ROOT_FRN || e.parent as usize == cur {
                    break;
                }
                cur = e.parent as usize;
            }
        }
        sizes
    }

    fn children_index(&self) -> Children {
        let n = self.entries.len();
        let mut start = vec![0u32; n + 1];
        for i in (0..n).filter(|&i| self.visible(i)) {
            let p = self.entries[i].parent as usize;
            if p < n {
                start[p + 1] += 1;
            }
        }
        for i in 0..n {
            start[i + 1] += start[i];
        }
        let mut cursor = start.clone();
        let mut list = vec![0u32; start[n] as usize];
        for i in (0..n).filter(|&i| self.visible(i)) {
            let p = self.entries[i].parent as usize;
            if p < n {
                list[cursor[p] as usize] = i as u32;
                cursor[p] += 1;
            }
        }
        Children { start, list }
    }

    fn resolve(&self, path: &str, children: &Children) -> Option<usize> {
        let rest = match path.as_bytes() {
            [d, b':', ..] if d.is_ascii_alphabetic() => &path[2..],
            _ => path,
        };
        let mut cur = ROOT_FRN;
        for comp in rest.split(['\\', '/']).filter(|c| !c.is_empty()) {
            let want = comp.to_lowercase();
            cur = children.of(cur).iter().map(|&c| c as usize).find(|&c| {
                String::from_utf16_lossy(self.name(&self.entries[c])).to_lowercase() == want
            })?;
        }
        Some(cur)
    }

    pub fn top_dirs(&self, n: usize) -> Vec<Row> {
        let sizes = self.dir_sizes();
        let dirs = (0..self.entries.len())
            .filter(|&i| self.visible(i) && self.entries[i].state & IS_DIR != 0)
            .map(|i| (sizes[i], i))
            .filter(|&(s, _)| s >= self.min_size);
        top_n(dirs, n)
            .into_iter()
            .map(|(size, i)| Row { size, ..self.row(i) })
            .collect()
    }

    /// Tree from `path`: the `n` largest children per level, no deeper than `depth`.
    /// Without `full_paths`, children carry only the name in `path` (saves memory on large trees).
    pub fn tree(&self, path: &str, depth: usize, n: usize, full_paths: bool) -> Option<Vec<Row>> {
        let sizes = self.dir_sizes();
        let children = self.children_index();
        let root = self.resolve(path, &children)?;
        let mut rows = Vec::new();
        self.walk(root, 0, depth, n, full_paths, &sizes, &children, &mut rows);
        Some(rows)
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(
        &self,
        idx: usize,
        level: usize,
        max_depth: usize,
        n: usize,
        full_paths: bool,
        sizes: &[u64],
        children: &Children,
        rows: &mut Vec<Row>,
    ) {
        let e = &self.entries[idx];
        let is_dir = idx == ROOT_FRN || e.state & IS_DIR != 0;
        let size = if is_dir { sizes[idx] } else { e.size };
        let path = if level == 0 || full_paths {
            self.path_of(idx)
        } else {
            String::from_utf16_lossy(self.name(e))
        };
        rows.push(Row { path, size, is_dir, depth: level });
        if !is_dir || level >= max_depth {
            return;
        }
        let kids = children.of(idx).iter().map(|&c| {
            let c = c as usize;
            let k = &self.entries[c];
            (if k.state & IS_DIR != 0 { sizes[c] } else { k.size }, c)
        })
        .filter(|&(s, _)| s >= self.min_size);
        for (_, c) in top_n(kids, n) {
            self.walk(c, level + 1, max_depth, n, full_paths, sizes, children, rows);
        }
    }

    fn files(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.entries.len())
            .filter(|&i| self.visible(i) && self.entries[i].state & IS_DIR == 0)
    }

    pub fn top(&self, n: usize) -> Vec<Row> {
        let files = self
            .files()
            .map(|i| (self.entries[i].size, i))
            .filter(|&(s, _)| s >= self.min_size);
        top_n(files, n).into_iter().map(|(_, i)| self.row(i)).collect()
    }

    pub fn search(&self, pattern: &str, n: usize) -> Vec<Row> {
        let re = Regex::new(&format!("(?i){pattern}"))
            .or_else(|_| Regex::new(&format!("(?i){}", regex::escape(pattern))))
            .ok();
        let needle = pattern.to_lowercase();
        let mut hits: Vec<(u64, usize)> = (0..self.entries.len())
            .into_par_iter()
            .filter(|&i| self.visible(i))
            .map_init(String::new, |buf, i| {
                buf.clear();
                buf.extend(
                    char::decode_utf16(self.name(&self.entries[i]).iter().copied())
                        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER)),
                );
                let matched = match &re {
                    Some(re) => re.is_match(buf),
                    None => buf.to_lowercase().contains(&needle),
                };
                (matched && self.entries[i].size >= self.min_size).then(|| (self.entries[i].size, i))
            })
            .flatten()
            .collect();
        hits.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        hits.truncate(n);
        hits.into_iter().map(|(_, i)| self.row(i)).collect()
    }

    /// Groups of files with equal size; takes the `n` groups with the largest potential savings.
    pub fn duplicates(&self, n: usize) -> Vec<Row> {
        let mut pairs: Vec<(u64, usize)> = self
            .files()
            .map(|i| (self.entries[i].size, i))
            .filter(|&(s, _)| s > 0 && s >= self.min_size)
            .collect();
        pairs.sort_unstable();
        let mut groups: Vec<(u64, &[(u64, usize)])> = Vec::new();
        let mut start = 0;
        while start < pairs.len() {
            let size = pairs[start].0;
            let mut end = start + 1;
            while end < pairs.len() && pairs[end].0 == size {
                end += 1;
            }
            if end - start > 1 {
                let wasted = size.saturating_mul((end - start - 1) as u64);
                groups.push((wasted, &pairs[start..end]));
            }
            start = end;
        }
        groups.sort_unstable_by(|a, b| b.0.cmp(&a.0));
        groups
            .into_iter()
            .take(n)
            .flat_map(|(_, g)| g.iter().map(|&(_, i)| self.row(i)))
            .collect()
    }
}
