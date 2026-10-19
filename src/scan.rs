use std::io;
use std::sync::mpsc::{channel, sync_channel};
use std::time::{Duration, Instant};

use rayon::prelude::*;

use crate::ntfs::{self, Extent, Parsed};
use crate::store::{FileEntry, Store, EXT_SIZE, HAS_NAME, HAS_SIZE, IS_DIR};
use crate::volume::{AlignedBuf, Volume};

const BUF_SIZE: usize = 4 << 20;
const POOL: usize = 4;
const RECORDS_PER_TASK: usize = 512;

#[derive(Default)]
pub struct Timings {
    pub open: Duration,
    pub read_vbr: Duration,
    pub read_rec0: Duration,
    pub read_rec0_repeat: Duration,
    pub parse_rec0: Duration,
    pub io_wait: Duration,
    pub parse: Duration,
    pub merge: Duration,
}

pub struct ScanResult {
    pub store: Store,
    pub records: u64,
    pub extra_links: u64,
    /// Base records with `$ATTRIBUTE_LIST` that have no `$FILE_NAME` in the record itself.
    pub attr_list_nameless: u64,
    /// Extension records that contain `$FILE_NAME`.
    pub ext_with_names: u64,
    pub timings: Timings,
}

struct Chunk {
    buf: AlignedBuf,
    valid: usize,
    first_record: u64,
}

struct Batch {
    items: Vec<(u64, Parsed)>,
    names: Vec<u16>,
}

fn err(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

pub fn scan(vol: &Volume, drive: char, open_duration: Duration) -> io::Result<ScanResult> {
    let mut timings = Timings { open: open_duration, ..Default::default() };

    let t = Instant::now();
    let mut sector = AlignedBuf::new(4096);
    vol.read_at(0, &mut sector, 4096)?;
    timings.read_vbr = t.elapsed();

    let boot = ntfs::parse_boot(sector.as_slice()).map_err(err)?;
    let rs = boot.record_size;

    let t = Instant::now();
    let read_len = rs.next_multiple_of(boot.bytes_per_sector as usize);
    let mut rec0 = AlignedBuf::new(read_len);
    let got = vol.read_at(boot.mft_lcn.saturating_mul(boot.cluster_size), &mut rec0, read_len)?;
    if got < rs {
        return Err(err("failed to read MFT record 0"));
    }
    timings.read_rec0 = t.elapsed();

    let t = Instant::now();
    vol.read_at(boot.mft_lcn.saturating_mul(boot.cluster_size), &mut rec0, read_len)?;
    timings.read_rec0_repeat = t.elapsed();

    let t = Instant::now();
    let rec0 = &mut rec0.as_mut_slice()[..rs];
    if !ntfs::apply_fixup(rec0) {
        return Err(err("$MFT record 0 is corrupt"));
    }
    let (extents, mft_size) =
        ntfs::mft_extents(rec0, boot.cluster_size).ok_or_else(|| err("$MFT has no $DATA attribute"))?;
    timings.parse_rec0 = t.elapsed();

    let total_records = (mft_size / rs as u64) as usize;
    if total_records == 0 || total_records > u32::MAX as usize {
        return Err(err("invalid $MFT size"));
    }

    let mut entries = vec![FileEntry::default(); total_records];
    let mut names: Vec<u16> = Vec::with_capacity(total_records * 12);
    let mut extra_links: u64 = 0;
    let mut attr_list_nameless: u64 = 0;
    let mut ext_with_names: u64 = 0;
    let bps = boot.bytes_per_sector as usize;

    let (tx, rx) = sync_channel::<Chunk>(POOL);
    let (free_tx, free_rx) = channel::<AlignedBuf>();
    for _ in 0..POOL {
        let _ = free_tx.send(AlignedBuf::new(BUF_SIZE));
    }

    let io_result = std::thread::scope(|s| -> io::Result<()> {
        let reader = s.spawn(move || read_mft(vol, &extents, mft_size, rs, bps, tx, free_rx));

        loop {
            let t = Instant::now();
            let Ok(mut chunk) = rx.recv() else { break };
            timings.io_wait += t.elapsed();

            let t = Instant::now();
            let batches = parse_chunk(&mut chunk, rs);
            timings.parse += t.elapsed();

            let t = Instant::now();
            for b in &batches {
                for (_, p) in &b.items {
                    extra_links += p.extra_links as u64;
                    attr_list_nameless += (p.base == 0 && p.has_attr_list && !p.has_name) as u64;
                    ext_with_names += (p.base != 0 && p.has_name) as u64;
                }
                merge(&mut entries, &mut names, b);
            }
            timings.merge += t.elapsed();
            let _ = free_tx.send(chunk.buf);
        }
        drop(free_tx);
        reader.join().unwrap_or_else(|_| Err(err("I/O thread panicked")))
    });
    io_result?;

    Ok(ScanResult {
        store: Store { entries, names, drive },
        records: total_records as u64,
        extra_links,
        attr_list_nameless,
        ext_with_names,
        timings,
    })
}

fn read_mft(
    vol: &Volume,
    extents: &[Extent],
    mft_size: u64,
    rs: usize,
    bps: usize,
    tx: std::sync::mpsc::SyncSender<Chunk>,
    free_rx: std::sync::mpsc::Receiver<AlignedBuf>,
) -> io::Result<()> {
    let mut logical: u64 = 0;
    for ext in extents {
        let mut left = ext.length.min(mft_size.saturating_sub(logical));
        let Some(mut disk) = ext.disk_offset else {
            logical += ext.length;
            continue;
        };
        let ext_logical_end = logical + ext.length;
        while left > 0 {
            let Ok(mut buf) = free_rx.recv() else { return Ok(()) };
            let take = (left as usize).min(buf.len());
            let read_len = take.next_multiple_of(bps).min(buf.len());
            let got = vol.read_at(disk, &mut buf, read_len)?;
            let valid = got.min(take) / rs * rs;
            if valid == 0 {
                return Err(err("empty read of $MFT"));
            }
            let first_record = logical / rs as u64;
            if tx.send(Chunk { buf, valid, first_record }).is_err() {
                return Ok(());
            }
            logical += valid as u64;
            disk += valid as u64;
            left -= valid as u64;
        }
        logical = ext_logical_end;
    }
    Ok(())
}

fn parse_chunk(chunk: &mut Chunk, rs: usize) -> Vec<Batch> {
    let first = chunk.first_record;
    let valid = chunk.valid;
    chunk.buf.as_mut_slice()[..valid]
        .par_chunks_mut(rs * RECORDS_PER_TASK)
        .enumerate()
        .map(|(task, block)| {
            let base_idx = first + (task * RECORDS_PER_TASK) as u64;
            let mut batch = Batch { items: Vec::with_capacity(RECORDS_PER_TASK), names: Vec::new() };
            for (i, rec) in block.chunks_exact_mut(rs).enumerate() {
                if !ntfs::apply_fixup(rec) {
                    continue;
                }
                if let Some(p) = ntfs::parse_record(rec, &mut batch.names) {
                    batch.items.push((base_idx + i as u64, p));
                }
            }
            batch
        })
        .collect()
}

fn merge(entries: &mut [FileEntry], names: &mut Vec<u16>, b: &Batch) {
    for &(idx, p) in &b.items {
        let target = if p.base != 0 { p.base } else { idx } as usize;
        let Some(e) = entries.get_mut(target) else { continue };
        let is_ext = p.base != 0;

        if p.has_size {
            if is_ext {
                e.size = p.size;
                e.state |= HAS_SIZE | EXT_SIZE;
            } else if e.state & EXT_SIZE == 0 {
                e.size = p.size;
                e.state |= HAS_SIZE;
            }
        }
        if !is_ext && p.is_dir {
            e.state |= IS_DIR;
        }
        if p.has_name && (!is_ext || e.state & HAS_NAME == 0) {
            let src = b
                .names
                .get(p.name_off as usize..p.name_off as usize + p.name_len as usize);
            let (Some(src), Ok(off)) = (src, u32::try_from(names.len())) else { continue };
            e.name_off = off;
            e.name_len = p.name_len;
            names.extend_from_slice(src);
            e.parent = p.parent as u32;
            e.flags = p.attrs;
            e.state |= HAS_NAME;
        }
    }
}
