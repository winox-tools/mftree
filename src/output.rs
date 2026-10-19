use std::io::{self, Write};

use crate::store::Row;

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Format {
    Table,
    Json,
    Csv,
}

pub fn human(size: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = size as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{size} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

fn json_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

fn csv_escape(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

pub struct SummaryOut<'a> {
    pub volume: String,
    pub total: Option<u64>,
    pub free: Option<u64>,
    pub files: u64,
    pub dirs: u64,
    pub files_size: u64,
    pub top_roots: &'a [Row],
    pub mft_read_ms: f64,
    pub parse_ms: f64,
}

fn opt_json(v: Option<u64>) -> String {
    v.map_or("null".to_string(), |v| v.to_string())
}

pub fn print_summary(s: &SummaryOut, format: Format, compact: bool, sink: impl Write) -> io::Result<()> {
    let mut out = io::BufWriter::new(sink);
    let used = s.total.zip(s.free).map(|(t, f)| t.saturating_sub(f));
    match format {
        Format::Table => {
            let h = |v: Option<u64>| v.map_or("unknown".to_string(), human);
            writeln!(out, "Volume:       {}", s.volume)?;
            writeln!(out, "Size:         {}", h(s.total))?;
            writeln!(out, "Used:         {}", h(used))?;
            writeln!(out, "Free:         {}", h(s.free))?;
            writeln!(out, "Files:        {}", s.files)?;
            writeln!(out, "Directories:  {}", s.dirs)?;
            writeln!(out, "Files total:  {}", human(s.files_size))?;
            writeln!(out, "MFT read:     {:.0} ms", s.mft_read_ms)?;
            writeln!(out, "Parse:        {:.0} ms", s.parse_ms)?;
            writeln!(out, "Top root folders:")?;
            for r in s.top_roots {
                writeln!(out, "{:>12}  {}", human(r.size), r.path)?;
            }
        }
        Format::Json => {
            let (nl, ind) = if compact { ("", "") } else { ("\n", "  ") };
            let roots: Vec<String> = s
                .top_roots
                .iter()
                .map(|r| format!("{{\"path\":\"{}\",\"size\":{}}}", json_escape(&r.path), r.size))
                .collect();
            write!(out, "{{{nl}")?;
            write!(out, "{ind}\"volume\":\"{}\",{nl}", json_escape(&s.volume))?;
            write!(out, "{ind}\"total_bytes\":{},{nl}", opt_json(s.total))?;
            write!(out, "{ind}\"used_bytes\":{},{nl}", opt_json(used))?;
            write!(out, "{ind}\"free_bytes\":{},{nl}", opt_json(s.free))?;
            write!(out, "{ind}\"files\":{},{nl}", s.files)?;
            write!(out, "{ind}\"directories\":{},{nl}", s.dirs)?;
            write!(out, "{ind}\"files_size_bytes\":{},{nl}", s.files_size)?;
            write!(out, "{ind}\"mft_read_ms\":{:.1},{nl}", s.mft_read_ms)?;
            write!(out, "{ind}\"parse_ms\":{:.1},{nl}", s.parse_ms)?;
            write!(out, "{ind}\"top_root_folders\":[{}]{nl}", roots.join(","))?;
            writeln!(out, "}}")?;
        }
        Format::Csv => {
            writeln!(out, "key,value")?;
            writeln!(out, "volume,{}", csv_escape(&s.volume))?;
            writeln!(out, "total_bytes,{}", s.total.map_or(String::new(), |v| v.to_string()))?;
            writeln!(out, "used_bytes,{}", used.map_or(String::new(), |v| v.to_string()))?;
            writeln!(out, "free_bytes,{}", s.free.map_or(String::new(), |v| v.to_string()))?;
            writeln!(out, "files,{}", s.files)?;
            writeln!(out, "directories,{}", s.dirs)?;
            writeln!(out, "files_size_bytes,{}", s.files_size)?;
            writeln!(out, "mft_read_ms,{:.1}", s.mft_read_ms)?;
            writeln!(out, "parse_ms,{:.1}", s.parse_ms)?;
            for r in s.top_roots {
                writeln!(out, "{},{}", csv_escape(&format!("root:{}", r.path)), r.size)?;
            }
        }
    }
    out.flush()
}

pub fn print(rows: &[Row], format: Format, compact: bool, sink: impl Write) -> io::Result<()> {
    let mut out = io::BufWriter::with_capacity(1 << 20, sink);
    match format {
        Format::Table => {
            writeln!(out, "{:>12}  {:<4} {}", "SIZE", "TYPE", "PATH")?;
            for r in rows {
                let kind = if r.is_dir { "DIR" } else { "FILE" };
                let label = match r.depth {
                    0 => r.path.as_str(),
                    d => {
                        let name = r.path.rsplit('\\').next().unwrap_or(&r.path);
                        write!(out, "{:>12}  {:<4} {}", human(r.size), kind, "  ".repeat(d))?;
                        writeln!(out, "{name}")?;
                        continue;
                    }
                };
                writeln!(out, "{:>12}  {:<4} {}", human(r.size), kind, label)?;
            }
        }
        Format::Json => {
            let (nl, ind) = if compact { ("", "") } else { ("\n", "  ") };
            write!(out, "[{nl}")?;
            for (i, r) in rows.iter().enumerate() {
                let comma = if i + 1 < rows.len() { "," } else { "" };
                write!(
                    out,
                    "{ind}{{\"path\":\"{}\",\"size\":{},\"is_dir\":{}}}{comma}{nl}",
                    json_escape(&r.path),
                    r.size,
                    r.is_dir
                )?;
            }
            writeln!(out, "]")?;
        }
        Format::Csv => {
            writeln!(out, "path,size,is_dir")?;
            for r in rows {
                writeln!(out, "{},{},{}", csv_escape(&r.path), r.size, r.is_dir)?;
            }
        }
    }
    out.flush()
}
