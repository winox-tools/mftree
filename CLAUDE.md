# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

- Build: `cargo build --release`
- All tests: `cargo test --release`; single test: `cargo test --release ntfs::tests::fixup_mismatch`
- Lint: `cargo clippy --release`
- Run: `.\target\release\mft-scan.exe C: --benchmark -t 20` — must run from an **elevated** shell. Without admin rights it prints `Administrator privileges required (elevated shell)` and exits with code 1 (other errors exit 2). The volume scan cannot be exercised from a non-elevated session, only the unit tests in `src/ntfs.rs`.

## Conventions

- Commit messages: English, Conventional Commits, subject line only (no body).
- Code comments and user-facing messages are in English; keep comments rare and only for non-obvious logic.
- Windows-only crate (`windows-sys`); there is no cross-platform path.

## Architecture

Reads the raw NTFS volume (`\\.\C:`, `FILE_FLAG_NO_BUFFERING`) and parses `$MFT` directly instead of walking directories. The pipeline spans several files:

1. `scan.rs::scan` reads the boot sector and MFT record 0, then gets the `$MFT` extents and real size via `ntfs::mft_extents` (data runs, so a fragmented `$MFT` works; sparse runs only advance the logical offset).
2. `scan.rs::read_mft` runs on a dedicated I/O thread. It reads extents into 4 MB `AlignedBuf`s (4096-aligned, required by NO_BUFFERING) from a fixed pool of 4 buffers, handing filled chunks to the consumer over a bounded `sync_channel` and getting buffers back over a second channel. The logical MFT offset divided by record size gives the record number (FRN), which is why chunks carry `first_record`.
3. The consumer parses each chunk in parallel with rayon (`parse_chunk`): per record `ntfs::apply_fixup`, then `ntfs::parse_record`. Each rayon task returns a `Batch` with its own local UTF-16 names arena.
4. `merge` folds batches sequentially into `Store`: a flat `Vec<FileEntry>` indexed by FRN plus one shared `Vec<u16>` names arena (names are not `String`s, to keep RAM low). Extension records (`base != 0`, from `$ATTRIBUTE_LIST`) are merged into their base FRN; `EXT_SIZE`/`HAS_SIZE`/`HAS_NAME` flags in `FileEntry.state` resolve ordering between base and extension records, since either can arrive first.
5. `store.rs` resolves paths lazily (`path_of` walks parent links to FRN 5, with a depth cap and `\?` marking broken chains) and implements the queries (`top`, `search`, `duplicates`). `output.rs` renders table/json/csv; `--benchmark` stats go to stderr so json/csv on stdout stay clean.

## Invariants

- Parsing code handles untrusted disk bytes: no `unwrap`/direct indexing on record data; use the bounds-checked `u16_at`/`u32_at`/`u64_at` helpers and return `None` for corrupt records.
- File size comes from the unnamed `$DATA` attribute; `$FILE_NAME` size is only a fallback because it is often stale. DOS (8.3) names are used only if no other name exists.
- `-t/--top` also caps `--search` results, and for `--duplicates` it counts groups, not files.
