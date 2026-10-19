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

pub fn print(rows: &[Row], format: Format, sink: impl Write) -> io::Result<()> {
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
            writeln!(out, "[")?;
            for (i, r) in rows.iter().enumerate() {
                let comma = if i + 1 < rows.len() { "," } else { "" };
                writeln!(
                    out,
                    "  {{\"path\":\"{}\",\"size\":{},\"is_dir\":{}}}{comma}",
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
