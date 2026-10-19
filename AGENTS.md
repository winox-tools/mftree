# AGENTS.md

Usage guide for `mft-scan`, a CLI scanner for NTFS volumes that reads `$MFT` directly. Windows and NTFS only.

## Requirements

- Must run from an **elevated PowerShell/cmd**. Without admin rights it prints `Administrator privileges required (elevated shell)` and exits with code `1`. An agent without elevation cannot scan a volume: do not try to work around it, ask the user to run the command and send the output.
- Build: `cargo build --release`; binary: `target\release\mft-scan.exe`.

## Syntax

```
mft-scan.exe <VOLUME> [options]
```

`<VOLUME>` is a drive letter: `C:`, `C`, `D:\`.

| Option | What it does |
|---|---|
| (none) | top-N largest files |
| `-t, --top <N>` | N results (default 20); with `--tree`, children per level; with `-d`, number of groups |
| `--dirs` | top-N folders by size, including everything nested |
| `--tree <PATH>` | tree from a folder (`C:\Users`, `\Users`, `""` for the root), case-insensitive |
| `--depth <N>` | depth for `--tree` (default 2) |
| `--full` | with `--tree`: full depth and all children; `--depth` and `--top` are ignored |
| `-s, --search <PATTERN>` | search by name: regex, case-insensitive; an invalid regex falls back to a substring match. Results are capped by `-t` |
| `-d, --duplicates` | files of equal size (duplicate candidates; contents are not compared) |
| `--format <table\|json\|csv>` | output format (default `table`) |
| `-o, --output <FILE>` | write to a file (UTF-8) instead of the console |
| `--benchmark` | per-stage timings, peak RAM and record counters, printed to **stderr** |

Mode precedence when several are given: `--tree` > `--dirs` > `--duplicates` > `--search` > top files.

## Examples

```powershell
# largest files on the disk
mft-scan.exe C: -t 50

# what takes the space: top-level folders and their contents
mft-scan.exe C: --tree C:\ --depth 2 -t 15

# full tree to a file
mft-scan.exe C: --tree C:\ --full -o tree.txt

# regex search, machine-readable output
mft-scan.exe C: -s "\.vhdx?$" -t 100 --format json -o found.json

# duplicates by size
mft-scan.exe C: -d -t 30 --format csv
```

## Output format

- `table`: columns `SIZE`, `TYPE` (`FILE`/`DIR`), `PATH`. In `--tree`, children print an indented name instead of a path.
- `json`: array of `{"path", "size", "is_dir"}` objects, size in bytes.
- `csv`: header `path,size,is_dir`; fields with commas or quotes are escaped.
- With `json` and `csv`, `--tree` rows carry the full path and are in depth-first order.
- To parse results, use `json`/`csv` with `-o`: stdout stays clean, and `--benchmark` output goes to stderr.

## What to know about the results

- **NTFS metafiles appear in the output** (`C:\$MFT`, `System Volume Information`, etc.). They are real volume data but not user files.
- **Hard links:** each file is shown once, under one (the best) name. Folder sizes can therefore be smaller than in tools that expand every link (noticeable in `C:\Windows\WinSxS`). WizTree reports ~7% more files for this reason.
- **File size** is the size of the unnamed `$DATA` attribute (logical, not size on disk). Folder size is the sum of the file sizes inside, not disk usage.
- The volume is read live: numbers may differ slightly between runs.
- If an item's path cannot be resolved up to the root, it contains `\?`.
- `--tree` with a nonexistent path fails with `path not found` and exit code `2`.

## Exit codes

- `0` — success
- `1` — not running as administrator
- `2` — error (invalid drive letter, not an NTFS volume, corrupt `$MFT`, read error, path not found)

## Performance and volume

- Scanning a volume with ~3.5M MFT records takes about 1.7 s with ~270 MB peak RAM (measured on NVMe). The first run after idle can be about 0.3 s slower.
- `--tree --full` on a whole disk produces millions of lines: always use `-o` instead of printing to the console.
