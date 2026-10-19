# AGENTS.md

Usage guide for `mftree`, a CLI scanner for NTFS volumes that reads `$MFT` directly. Windows and NTFS only.

## Requirements

- Must run from an **elevated PowerShell/cmd**. Without admin rights it prints `Administrator privileges required (elevated shell)` and exits with code `1`. An agent without elevation cannot scan a volume: do not try to work around it, ask the user to run the command and send the output.
- Build: `cargo build --release`; binary: `target\release\mftree.exe`.

## Syntax

```
mftree.exe <VOLUME> [options]
```

`<VOLUME>` is a drive letter: `C:`, `C`, `D:\`.

| Option | What it does |
|---|---|
| (none) | top-N largest files |
| `-t, --top <N>`, `--limit <N>` | N results (default 20); with `--tree`, children per level; with `-d`, number of groups |
| `--dirs` | top-N folders by size, including everything nested |
| `--summary` | volume summary instead of a list: size, used, free, file and folder counts, top-5 root folders, MFT read and parse times |
| `--tree <PATH>` | tree from a folder (`C:\Users`, `\Users`, `""` for the root), case-insensitive |
| `--depth <N>`, `--max-depth <N>` | depth for `--tree` (default 2) |
| `--min-size <SIZE>` | skip entries smaller than SIZE: `500KB`, `100MB`, `1.5GB`, plain bytes (units are powers of 1024) |
| `--exclude <PATTERN>` | hide a path and everything below it; repeatable. A glob (`C:\Windows\*`, `*.tmp`), `re:<regex>` (matched against the full path), a name without separators (`.git`, matches any entry with that name), or a path prefix (`C:\Windows`). Case-insensitive |
| `--exclude-meta`, `--hide-system` | hide NTFS metafiles in the volume root: `$MFT`, `$LogFile`, `$Recycle.Bin`, `System Volume Information`, etc. |
| `--full` | with `--tree`: full depth and all children; `--depth` and `--top` are ignored |
| `-s, --search <PATTERN>` | search by name: regex, case-insensitive; an invalid regex falls back to a substring match. Results are capped by `-t` |
| `-d, --duplicates` | files of equal size (duplicate candidates; contents are not compared) |
| `--format <table\|json\|csv>` | output format (default `table`) |
| `--json`, `--json-compact` | shortcuts for `--format json`; `--json-compact` prints JSON on a single line |
| `-o, --output <FILE>` | write to a file (UTF-8) instead of the console |
| `--benchmark` | per-stage timings, peak RAM and record counters, printed to **stderr** |

Mode precedence when several are given: `--summary` > `--tree` > `--dirs` > `--duplicates` > `--search` > top files. `--min-size`, `--exclude` and `--exclude-meta` apply to every mode, including folder sizes (excluded files do not count towards them).

## Examples

```powershell
# largest files on the disk
mftree.exe C: -t 50

# what takes the space: top-level folders and their contents
mftree.exe C: --tree C:\ --depth 2 -t 15

# full tree to a file
mftree.exe C: --tree C:\ --full -o tree.txt

# regex search, machine-readable output
mftree.exe C: -s "\.vhdx?$" -t 100 --format json -o found.json

# duplicates by size
mftree.exe C: -d -t 30 --format csv
```

## Output format

- `table`: columns `SIZE`, `TYPE` (`FILE`/`DIR`), `PATH`. In `--tree`, children print an indented name instead of a path.
- `json`: array of `{"path", "size", "is_dir"}` objects, size in bytes.
- `csv`: header `path,size,is_dir`; fields with commas or quotes are escaped.
- With `json` and `csv`, `--tree` rows carry the full path and are in depth-first order.
- `--summary` with `json`: one object `{"volume", "total_bytes", "used_bytes", "free_bytes", "files", "directories", "files_size_bytes", "mft_read_ms", "parse_ms", "top_root_folders": [{"path", "size"}]}`; with `csv`: `key,value` rows. Volume sizes are `null`/empty if the query fails. `mft_read_ms` is the time the pipeline spent waiting for the disk, `parse_ms` the CPU parse and merge time.
- Only data goes to stdout; every message and error goes to stderr (`mftree: error: ...`), so `mftree.exe C: --json | jq ...` is safe.
- To parse results, use `json`/`csv` with `-o`: stdout stays clean, and `--benchmark` output goes to stderr.

## What to know about the results

- **NTFS metafiles appear in the output** (`C:\$MFT`, `System Volume Information`, etc.). They are real volume data but not user files.
- **Hard links:** each file is shown once, under one (the best) name. Folder sizes can therefore be smaller than in tools that expand every link (noticeable in `C:\Windows\WinSxS`). WizTree reports ~7% more files for this reason.
- **File size** is the size of the unnamed `$DATA` attribute (logical, not size on disk). Folder size is the sum of the file sizes inside, not disk usage.
- The volume is read live: numbers may differ slightly between runs.
- If an item's path cannot be resolved up to the root, it contains `\?`.
- `--tree` with a nonexistent path fails with `path not found` and exit code `2`.
- `--summary`, `--min-size`, `--exclude`, `--exclude-meta`, `--json` and the exit codes were checked on a real system volume (e.g. `--exclude .git` removed all 44 `.git` paths and `--exclude "C:\Windows\*"` all 2884 paths under `C:\Windows` from a 1 MB+ listing) in addition to unit tests.

## Exit codes

- `0` — success
- `1` — not running as administrator
- `2` — volume or `--tree` path not found, not an NTFS volume, or invalid arguments (bad drive letter, `--min-size`, `--exclude`)
- `3` — I/O error or corrupt `$MFT` while reading the raw volume

## Performance and volume

- Scanning a volume with ~3.5M MFT records takes about 1.6 s with ~270 MB peak RAM (measured on NVMe). The first run after idle can be about 0.3 s slower.
- `--tree --full` on a whole disk produces millions of lines: always use `-o` instead of printing to the console.

# Development

For anyone changing the `mftree` code.

## Commands

- Build: `cargo build --release`
- All tests: `cargo test --release`; single test: `cargo test --release ntfs::tests::fixup_mismatch`
- Lint: `cargo clippy --release`
- Run: `.\target\release\mftree.exe C: --benchmark -t 20` — must run from an **elevated** shell. The volume scan cannot be exercised from a non-elevated session, only the unit tests in `src/ntfs.rs` and `src/store.rs`.

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
5. `store.rs` resolves paths lazily (`path_of` walks parent links to FRN 5, with a depth cap and `\?` marking broken chains) and implements the queries (`top`, `top_dirs`, `tree`, `search`, `duplicates`). `output.rs` renders table/json/csv; `--benchmark` stats go to stderr so json/csv on stdout stay clean.

## Invariants

- Parsing code handles untrusted disk bytes: no `unwrap`/direct indexing on record data; use the bounds-checked `u16_at`/`u32_at`/`u64_at` helpers and return `None` for corrupt records.
- File size comes from the unnamed `$DATA` attribute; `$FILE_NAME` size is only a fallback because it is often stale. DOS (8.3) names are used only if no other name exists.
- Folder sizes are not stored: `Store::dir_sizes` rolls file sizes up the parent chain on demand (`--dirs`, `--tree`); each file counts once under its best name, so hard links are not double-counted. The tree uses a lazily built CSR children index.
- `-t/--top` also caps `--search` results, and for `--duplicates` it counts groups, not files.
