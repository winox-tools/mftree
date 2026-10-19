use std::cmp::Reverse;
use std::collections::BinaryHeap;

use rayon::prelude::*;
use regex::Regex;

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
}

pub struct Row {
    pub path: String,
    pub size: u64,
    pub is_dir: bool,
}

impl Store {
    fn name(&self, e: &FileEntry) -> &[u16] {
        let start = e.name_off as usize;
        self.names.get(start..start + e.name_len as usize).unwrap_or(&[])
    }

    fn visible(&self, idx: usize) -> bool {
        idx != ROOT_FRN && self.entries[idx].state & HAS_NAME != 0
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
        Row { path: self.path_of(idx), size: e.size, is_dir: e.state & IS_DIR != 0 }
    }

    fn files(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.entries.len())
            .filter(|&i| self.visible(i) && self.entries[i].state & IS_DIR == 0)
    }

    pub fn top(&self, n: usize) -> Vec<Row> {
        let mut heap: BinaryHeap<Reverse<(u64, usize)>> = BinaryHeap::with_capacity(n + 1);
        for i in self.files() {
            let size = self.entries[i].size;
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
        v.into_iter().map(|(_, i)| self.row(i)).collect()
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
                matched.then(|| (self.entries[i].size, i))
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
            .filter(|&(s, _)| s > 0)
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
