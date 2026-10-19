# mftree

A fast CLI scanner for NTFS volumes. It reads and parses the `$MFT` (Master File Table) directly instead of walking directories with `FindFirstFile`/`FindNextFile`, so a full scan of a volume with millions of files takes a couple of seconds.

On the author's machine (NVMe SSD, ~3.5M MFT records, ~2.4M named entries) a full scan takes about 1.7 s with ~270 MB peak RAM. Your numbers will vary with the disk and the system state; run with `--benchmark` to see per-stage timings.

## Features

- Top-N largest files and folders (folder sizes include everything nested)
- Directory tree from any folder, up to the full tree of the volume
- Search by name (case-insensitive regex, falls back to substring)
- Duplicate candidates grouped by file size
- Output as table, JSON or CSV, to the console or a UTF-8 file

## Requirements

- Windows with an NTFS volume
- An **elevated** shell (Run as administrator): raw volume access needs it. Without elevation the tool prints an error and exits with code 1.
- Rust (edition 2021) to build from source

## Build

```powershell
cargo build --release
```

The binary is `target\release\mftree.exe`.

## Usage

```powershell
# 20 largest files on C:
mftree.exe C:

# 20 largest folders (including nested content)
mftree.exe C: --dirs

# tree from a folder, 3 levels deep, 10 largest children per level
mftree.exe C: --tree C:\Users --depth 3 -t 10

# the whole tree of the volume, written to a file
mftree.exe C: --tree C:\ --full -o tree.txt

# search by regex, JSON output
mftree.exe C: -s "\.vhdx?$" -t 100 --format json -o found.json

# duplicate candidates by size
mftree.exe C: -d -t 30

# per-stage timings and peak RAM (printed to stderr)
mftree.exe C: --benchmark
```

| Option | Description |
|---|---|
| `-t, --top <N>` | Number of results (default 20); children per level with `--tree`; groups with `-d` |
| `--dirs` | Largest folders |
| `--tree <PATH>`, `--depth <N>`, `--full` | Directory tree; `--full` ignores depth and the per-level limit |
| `-s, --search <PATTERN>` | Search by name |
| `-d, --duplicates` | Files with equal sizes |
| `--format <table\|json\|csv>` | Output format (default `table`) |
| `-o, --output <FILE>` | Write to a file instead of the console |
| `--benchmark` | Timings, peak RAM and record counters on stderr |

See [AGENTS.md](AGENTS.md) for the full reference, output formats and exit codes.

## How it works

1. Opens `\\.\X:` with `FILE_FLAG_NO_BUFFERING`, reads the boot sector and MFT record 0, and decodes the data runs of `$MFT` (so a fragmented `$MFT` is supported).
2. A dedicated I/O thread streams `$MFT` in 4 MB aligned blocks through a bounded channel.
3. A rayon worker pool applies the update-sequence fixup and parses each record's attributes (`$FILE_NAME`, `$DATA`, `$ATTRIBUTE_LIST`).
4. Entries are stored in a flat vector indexed by record number with a shared UTF-16 names arena; full paths and folder sizes are computed on demand.

Corrupt records are skipped; parsing code never trusts on-disk lengths.

## Limitations

- Each file is shown once, under one name. Hard links are not expanded, so file counts and some folder sizes differ from tools that list every link (e.g. WizTree shows ~7% more files on a system volume).
- NTFS metafiles (`$MFT`, `System Volume Information`, ...) are included in the output.
- Sizes are logical file sizes, not size on disk.
- The volume is read live, so results can change slightly between runs.
- Not tested on 4Kn drives yet; the code reads 4096-byte units for the boot sector for that reason.
- There is no GUI or treemap view.
