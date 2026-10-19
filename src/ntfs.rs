//! NTFS structure parsing. All functions work on untrusted data and never panic.

pub const ATTR_FILE_NAME: u32 = 0x30;
pub const ATTR_DATA: u32 = 0x80;
const ATTR_ATTRIBUTE_LIST: u32 = 0x20;
const ATTR_END: u32 = 0xFFFF_FFFF;
const FLAG_IN_USE: u16 = 0x01;
const FLAG_DIR: u16 = 0x02;
const USA_STRIDE: usize = 512;

pub fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o.checked_add(2)?)?.try_into().ok()?))
}

pub fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o.checked_add(4)?)?.try_into().ok()?))
}

pub fn u64_at(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o.checked_add(8)?)?.try_into().ok()?))
}

pub const NOT_NTFS: &str = "not an NTFS volume";

pub struct Boot {
    pub bytes_per_sector: u32,
    pub cluster_size: u64,
    pub mft_lcn: u64,
    pub record_size: usize,
}

pub fn parse_boot(sector: &[u8]) -> Result<Boot, &'static str> {
    if sector.get(3..11) != Some(b"NTFS    ") {
        return Err(NOT_NTFS);
    }
    let bps = u16_at(sector, 0x0B).ok_or("corrupt VBR")? as u32;
    let spc = *sector.get(0x0D).ok_or("corrupt VBR")? as u64;
    if bps < 512 || !bps.is_power_of_two() || spc == 0 {
        return Err("invalid VBR geometry");
    }
    let mft_lcn = u64_at(sector, 0x30).ok_or("corrupt VBR")?;
    let raw = *sector.get(0x40).ok_or("corrupt VBR")? as i8;
    let cluster_size = bps as u64 * spc;
    let record_size = if raw < 0 {
        let shift = -(raw as i32) as u32;
        if shift > 16 {
            return Err("invalid MFT record size");
        }
        1usize << shift
    } else {
        (raw as u64 * cluster_size) as usize
    };
    if record_size < 512 || record_size > 65536 {
        return Err("invalid MFT record size");
    }
    Ok(Boot { bytes_per_sector: bps, cluster_size, mft_lcn, record_size })
}

/// Removes the Update Sequence; false if the record is corrupt.
pub fn apply_fixup(rec: &mut [u8]) -> bool {
    if rec.get(0..4) != Some(b"FILE") {
        return false;
    }
    let (Some(off), Some(count)) = (u16_at(rec, 4), u16_at(rec, 6)) else {
        return false;
    };
    let (off, count) = (off as usize, count as usize);
    if count < 2 || off + count * 2 > rec.len() || (count - 1) * USA_STRIDE > rec.len() {
        return false;
    }
    let usn = [rec[off], rec[off + 1]];
    for i in 1..count {
        let end = i * USA_STRIDE;
        let (a, b) = (end - 2, end - 1);
        if rec[a] != usn[0] || rec[b] != usn[1] {
            return false;
        }
        rec[a] = rec[off + i * 2];
        rec[b] = rec[off + i * 2 + 1];
    }
    true
}

pub struct Attr<'a> {
    pub kind: u32,
    pub data: &'a [u8],
}

impl<'a> Attr<'a> {
    pub fn non_resident(&self) -> bool {
        self.data.get(8).copied().unwrap_or(0) != 0
    }

    pub fn is_unnamed(&self) -> bool {
        self.data.get(9).copied() == Some(0)
    }

    pub fn resident_content(&self) -> Option<&'a [u8]> {
        let len = u32_at(self.data, 0x10)? as usize;
        let off = u16_at(self.data, 0x14)? as usize;
        self.data.get(off..off.checked_add(len)?)
    }
}

pub struct AttrIter<'a> {
    rec: &'a [u8],
    pos: usize,
}

pub fn attrs(rec: &[u8]) -> AttrIter<'_> {
    let pos = u16_at(rec, 0x14).unwrap_or(u16::MAX) as usize;
    AttrIter { rec, pos }
}

impl<'a> Iterator for AttrIter<'a> {
    type Item = Attr<'a>;

    fn next(&mut self) -> Option<Attr<'a>> {
        let kind = u32_at(self.rec, self.pos)?;
        if kind == ATTR_END {
            return None;
        }
        let len = u32_at(self.rec, self.pos + 4)? as usize;
        if len < 16 || len % 8 != 0 {
            self.pos = usize::MAX;
            return None;
        }
        let data = self.rec.get(self.pos..self.pos.checked_add(len)?);
        let Some(data) = data else {
            self.pos = usize::MAX;
            return None;
        };
        self.pos += len;
        Some(Attr { kind, data })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Extent {
    /// None for a sparse run.
    pub disk_offset: Option<u64>,
    pub length: u64,
}

pub fn parse_runs(mut runs: &[u8], cluster_size: u64) -> Vec<Extent> {
    let mut out = Vec::new();
    let mut lcn: i64 = 0;
    while let Some(&h) = runs.first() {
        if h == 0 {
            break;
        }
        let (len_n, off_n) = ((h & 0x0F) as usize, (h >> 4) as usize);
        if len_n == 0 || len_n > 8 || off_n > 8 || runs.len() < 1 + len_n + off_n {
            break;
        }
        let mut len: u64 = 0;
        for (i, b) in runs[1..1 + len_n].iter().enumerate() {
            len |= (*b as u64) << (8 * i);
        }
        let disk_offset = if off_n == 0 {
            None
        } else {
            let raw = &runs[1 + len_n..1 + len_n + off_n];
            let mut v: i64 = 0;
            for (i, b) in raw.iter().enumerate() {
                v |= (*b as i64) << (8 * i);
            }
            if raw[off_n - 1] & 0x80 != 0 && off_n < 8 {
                v |= -1i64 << (8 * off_n);
            }
            lcn = match lcn.checked_add(v) {
                Some(l) if l >= 0 => l,
                _ => break,
            };
            (lcn as u64).checked_mul(cluster_size)
        };
        let Some(length) = len.checked_mul(cluster_size) else { break };
        out.push(Extent { disk_offset, length });
        runs = &runs[1 + len_n + off_n..];
    }
    out
}

/// Extents and real size of `$MFT` from record 0 (already after fixup).
pub fn mft_extents(rec: &[u8], cluster_size: u64) -> Option<(Vec<Extent>, u64)> {
    let attr = attrs(rec).find(|a| a.kind == ATTR_DATA && a.is_unnamed() && a.non_resident())?;
    let runs_off = u16_at(attr.data, 0x20)? as usize;
    let real = u64_at(attr.data, 0x30)?;
    let extents = parse_runs(attr.data.get(runs_off..)?, cluster_size);
    if extents.is_empty() {
        return None;
    }
    Some((extents, real))
}

#[derive(Default, Clone, Copy)]
pub struct Parsed {
    pub base: u64,
    pub parent: u64,
    pub size: u64,
    pub has_size: bool,
    pub is_dir: bool,
    pub has_name: bool,
    pub name_off: u32,
    pub name_len: u16,
    pub attrs: u32,
    /// Number of additional (non-DOS) names of the record, i.e. hard links.
    pub extra_links: u32,
    pub has_attr_list: bool,
}

/// Parses a record after fixup; the name is appended to `names`.
pub fn parse_record(rec: &[u8], names: &mut Vec<u16>) -> Option<Parsed> {
    let flags = u16_at(rec, 0x16)?;
    if flags & FLAG_IN_USE == 0 {
        return None;
    }
    let mut p = Parsed {
        base: u64_at(rec, 0x20)? & 0xFFFF_FFFF_FFFF,
        is_dir: flags & FLAG_DIR != 0,
        ..Default::default()
    };
    let mut best_ns = u8::MAX;
    let mut best: Option<&[u8]> = None;
    let mut non_dos_names: u32 = 0;
    for a in attrs(rec) {
        match a.kind {
            ATTR_ATTRIBUTE_LIST => p.has_attr_list = true,
            ATTR_FILE_NAME if !a.non_resident() => {
                let Some(c) = a.resident_content() else { continue };
                let (Some(&n), Some(&ns)) = (c.get(0x40), c.get(0x41)) else { continue };
                // Priority: Win32/Win32+DOS/POSIX over DOS (2).
                let rank = if ns == 2 { 1 } else { 0 };
                if rank == 0 {
                    non_dos_names += 1;
                }
                if rank < best_ns {
                    best_ns = rank;
                    best = c.get(..0x42 + n as usize * 2);
                }
            }
            ATTR_DATA if a.is_unnamed() && !p.has_size => {
                if a.non_resident() {
                    if u64_at(a.data, 0x10) == Some(0) {
                        if let Some(s) = u64_at(a.data, 0x30) {
                            p.size = s;
                            p.has_size = true;
                        }
                    }
                } else if let Some(c) = a.resident_content() {
                    p.size = c.len() as u64;
                    p.has_size = true;
                }
            }
            _ => {}
        }
    }
    p.extra_links = if p.base != 0 { non_dos_names } else { non_dos_names.saturating_sub(1) };
    if let Some(c) = best {
        p.parent = u64_at(c, 0)? & 0xFFFF_FFFF_FFFF;
        p.attrs = u32_at(c, 0x38)?;
        if !p.has_size {
            p.size = u64_at(c, 0x30)?;
            p.has_size = true;
        }
        let n = c[0x40] as usize;
        p.name_off = u32::try_from(names.len()).ok()?;
        p.name_len = n as u16;
        names.extend(
            c[0x42..0x42 + n * 2]
                .chunks_exact(2)
                .map(|w| u16::from_le_bytes([w[0], w[1]])),
        );
        p.has_name = true;
    }
    if !p.has_name && !p.has_size && !p.has_attr_list && p.base == 0 {
        return None;
    }
    Some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_signed_offset() {
        let r = parse_runs(&[0x21, 0x10, 0x00, 0x01, 0x21, 0x04, 0xF0, 0xFF, 0x00], 4096);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].disk_offset, Some(0x100 * 4096));
        assert_eq!(r[0].length, 0x10 * 4096);
        assert_eq!(r[1].disk_offset, Some((0x100 - 0x10) * 4096));
    }

    #[test]
    fn runs_sparse_and_garbage() {
        let r = parse_runs(&[0x01, 0x08, 0x31, 0x01], 4096);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].disk_offset, None);
    }

    #[test]
    fn fixup_mismatch() {
        let mut rec = vec![0u8; 1024];
        rec[0..4].copy_from_slice(b"FILE");
        rec[4] = 0x30;
        rec[6] = 3;
        rec[0x30] = 0xAA;
        rec[0x31] = 0xBB;
        rec[0x32] = 1;
        rec[0x33] = 2;
        rec[0x34] = 3;
        rec[0x35] = 4;
        rec[510] = 0xAA;
        rec[511] = 0xBB;
        rec[1022] = 0xAA;
        rec[1023] = 0xBB;
        assert!(apply_fixup(&mut rec));
        assert_eq!(&rec[510..512], &[1, 2]);
        assert_eq!(&rec[1022..1024], &[3, 4]);
        let mut bad = vec![0u8; 1024];
        bad[0..4].copy_from_slice(b"FILE");
        bad[4] = 0x30;
        bad[6] = 3;
        bad[510] = 1;
        assert!(!apply_fixup(&mut bad));
    }

    #[test]
    fn garbage_does_not_panic() {
        let mut names = Vec::new();
        for n in 0..2048usize {
            let mut r = vec![0xFFu8; 1024];
            r[0..4].copy_from_slice(b"FILE");
            r[0x14] = (n % 200) as u8;
            let _ = parse_record(&r, &mut names);
        }
    }
}
