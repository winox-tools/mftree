mod filter;
mod ntfs;
mod output;
mod scan;
mod store;
mod volume;

use std::io::ErrorKind;
use std::process::ExitCode;
use std::time::Instant;

use clap::Parser;
use windows_sys::Win32::System::Console::SetConsoleOutputCP;

use output::Format;

/// Exit codes: 0 ok, 1 not elevated, 2 volume/path not found or not NTFS (or bad arguments), 3 I/O error.
#[derive(Parser)]
#[command(name = "mftree", about = "Fast NTFS volume scanner that parses $MFT directly")]
struct Args {
    /// Drive letter to scan (for example, C:, D:)
    volume: String,

    /// Search files by substring / regex (case-insensitive; results are capped by --top)
    #[arg(short, long)]
    search: Option<String>,

    /// Find potential duplicates by size
    #[arg(short, long)]
    duplicates: bool,

    /// Limit the output to N entries (files, folders, duplicate groups, tree children per level)
    #[arg(short, long, visible_alias = "limit", default_value_t = 20)]
    top: usize,

    /// Show the top N largest folders (including nested content)
    #[arg(long)]
    dirs: bool,

    /// Tree from the given folder (for example, C:\Users), top N at each level
    #[arg(long, value_name = "PATH")]
    tree: Option<String>,

    /// Depth for --tree
    #[arg(long, visible_alias = "max-depth", default_value_t = 2)]
    depth: usize,

    /// For --tree: full depth and all children (ignores --depth and --top)
    #[arg(long)]
    full: bool,

    /// Skip entries smaller than SIZE (for example, 500KB, 100MB, 1GB)
    #[arg(long, value_name = "SIZE")]
    min_size: Option<String>,

    /// Hide paths matching PATTERN and everything below them; repeatable. A glob (`C:\Windows\*`,
    /// `*.tmp`), `re:<regex>`, a folder/file name (`.git`) or a path prefix (`C:\Windows`)
    #[arg(long, value_name = "PATTERN")]
    exclude: Vec<String>,

    /// Hide NTFS metafiles in the volume root ($MFT, $LogFile, $Recycle.Bin, System Volume Information)
    #[arg(long, visible_alias = "hide-system")]
    exclude_meta: bool,

    /// Print a volume summary instead of a file list
    #[arg(long)]
    summary: bool,

    /// Write the result to a file (UTF-8) instead of the console
    #[arg(short, long, value_name = "FILE")]
    output: Option<String>,

    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Table)]
    format: Format,

    /// Shortcut for --format json
    #[arg(long)]
    json: bool,

    /// JSON on a single line, without formatting (implies --json)
    #[arg(long)]
    json_compact: bool,

    /// Print per-stage timings to stderr
    #[arg(long)]
    benchmark: bool,
}

fn usage_error(msg: &str) -> ExitCode {
    eprintln!("mftree: error: {msg}");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    unsafe { SetConsoleOutputCP(65001) };
    let args = Args::parse();

    let Some(letter) = args
        .volume
        .trim()
        .chars()
        .next()
        .filter(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_uppercase())
    else {
        return usage_error(&format!("invalid drive letter: {}", args.volume));
    };

    let min_size = match args.min_size.as_deref().map(filter::parse_size).transpose() {
        Ok(v) => v.unwrap_or(0),
        Err(e) => return usage_error(&e),
    };
    let patterns = match args.exclude.iter().map(|p| filter::parse_pattern(p)).collect::<Result<Vec<_>, _>>() {
        Ok(v) => v,
        Err(e) => return usage_error(&e),
    };

    if !volume::is_elevated() {
        eprintln!("mftree: error: Administrator privileges required (elevated shell)");
        return ExitCode::from(1);
    }

    match run(&args, letter, min_size, &patterns) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mftree: error: {e}");
            ExitCode::from(match e.kind() {
                ErrorKind::NotFound | ErrorKind::Unsupported => 2,
                _ => 3,
            })
        }
    }
}

fn run(args: &Args, letter: char, min_size: u64, patterns: &[filter::Matcher]) -> std::io::Result<()> {
    let total = Instant::now();
    let t_open = Instant::now();
    let vol = volume::Volume::open(letter)?;
    let open_duration = t_open.elapsed();
    let mut res = scan::scan(&vol, letter, open_duration)?;
    res.store.min_size = min_size;
    res.store.apply_exclusions(patterns, args.exclude_meta);

    let (format, compact) = if args.json_compact {
        (Format::Json, true)
    } else if args.json {
        (Format::Json, false)
    } else {
        (args.format, false)
    };

    let t = Instant::now();
    let mut sink: Box<dyn std::io::Write> = match &args.output {
        Some(file) => Box::new(std::fs::File::create(file)?),
        None => Box::new(std::io::stdout().lock()),
    };
    if args.summary {
        let s = res.store.summary(5);
        let (vol_total, vol_free) = volume::disk_space(letter).unzip();
        let tm = &res.timings;
        let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
        output::print_summary(
            &output::SummaryOut {
                volume: format!("{letter}:"),
                total: vol_total,
                free: vol_free,
                files: s.files,
                dirs: s.dirs,
                files_size: s.files_size,
                top_roots: &s.top_roots,
                mft_read_ms: ms(tm.read_vbr + tm.read_rec0 + tm.io_wait),
                parse_ms: ms(tm.parse + tm.merge),
            },
            format,
            compact,
            &mut sink,
        )?;
    } else {
        let rows = if let Some(path) = &args.tree {
            let (depth, per_level) =
                if args.full { (usize::MAX, usize::MAX) } else { (args.depth, args.top) };
            let full_paths = !matches!(format, Format::Table);
            res.store.tree(path, depth, per_level, full_paths).ok_or_else(|| {
                std::io::Error::new(ErrorKind::NotFound, format!("path not found: {path}"))
            })?
        } else if args.dirs {
            res.store.top_dirs(args.top)
        } else if args.duplicates {
            res.store.duplicates(args.top)
        } else if let Some(pattern) = &args.search {
            res.store.search(pattern, args.top)
        } else {
            res.store.top(args.top)
        };
        output::print(&rows, format, compact, &mut sink)?;
    }
    let resolve = t.elapsed();

    if args.benchmark {
        let t = &res.timings;
        let named = res.store.entries.iter().filter(|e| e.state & store::HAS_NAME != 0).count();
        let dirs = res
            .store
            .entries
            .iter()
            .filter(|e| e.state & store::IS_DIR != 0 && e.state & store::HAS_NAME != 0)
            .count();
        eprintln!("--- benchmark ---");
        eprintln!("MFT records:     {}", res.records);
        eprintln!("named entries:   {named} (dirs {dirs}, files {})", named - dirs);
        eprintln!("extra hard links: {}", res.extra_links);
        eprintln!("attr_list without base name: {}", res.attr_list_nameless);
        eprintln!("ext records with name:       {}", res.ext_with_names);
        eprintln!("peak RAM:         {}", output::human(volume::peak_memory_bytes() as u64));
        eprintln!("open:             {:?}", t.open);
        eprintln!("read VBR:         {:?}", t.read_vbr);
        eprintln!("read rec0:        {:?}", t.read_rec0);
        eprintln!("read rec0 (rep):  {:?}", t.read_rec0_repeat);
        eprintln!("parse rec0:       {:?}", t.parse_rec0);
        eprintln!("I/O (wait):       {:?}", t.io_wait);
        eprintln!("parse:            {:?}", t.parse);
        eprintln!("merge:            {:?}", t.merge);
        eprintln!("resolve/query:    {:?}", resolve);
        eprintln!("total:            {:?}", total.elapsed());
    }
    Ok(())
}
