mod ntfs;
mod output;
mod scan;
mod store;
mod volume;

use std::process::ExitCode;
use std::time::Instant;

use clap::Parser;
use windows_sys::Win32::System::Console::SetConsoleOutputCP;

use output::Format;

#[derive(Parser)]
#[command(name = "mft-scan", about = "Fast NTFS volume scanner that parses $MFT directly")]
struct Args {
    /// Drive letter to scan (for example, C:, D:)
    volume: String,

    /// Search files by substring / regex (case-insensitive; results are capped by --top)
    #[arg(short, long)]
    search: Option<String>,

    /// Find potential duplicates by size
    #[arg(short, long)]
    duplicates: bool,

    /// Show the top N largest files
    #[arg(short, long, default_value_t = 20)]
    top: usize,

    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Table)]
    format: Format,

    /// Print per-stage timings
    #[arg(long)]
    benchmark: bool,
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
        eprintln!("Invalid drive letter: {}", args.volume);
        return ExitCode::from(2);
    };

    if !volume::is_elevated() {
        eprintln!("Administrator privileges required (elevated shell)");
        return ExitCode::from(1);
    }

    match run(&args, letter) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::from(2)
        }
    }
}

fn run(args: &Args, letter: char) -> std::io::Result<()> {
    let total = Instant::now();
    let t_open = Instant::now();
    let vol = volume::Volume::open(letter)?;
    let open_duration = t_open.elapsed();
    let res = scan::scan(&vol, letter, open_duration)?;

    let t = Instant::now();
    let rows = if args.duplicates {
        res.store.duplicates(args.top)
    } else if let Some(pattern) = &args.search {
        res.store.search(pattern, args.top)
    } else {
        res.store.top(args.top)
    };
    let resolve = t.elapsed();

    output::print(&rows, args.format)?;

    if args.benchmark {
        let t = &res.timings;
        let named = res.store.entries.iter().filter(|e| e.state & store::HAS_NAME != 0).count();
        eprintln!("--- benchmark ---");
        eprintln!("MFT records:     {}", res.records);
        eprintln!("named entries:   {named}");
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
