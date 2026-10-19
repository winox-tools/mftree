use regex::{Regex, RegexBuilder};

/// Parses sizes such as `500KB`, `100MB`, `1.5GB` (units are powers of 1024) into bytes.
pub fn parse_size(s: &str) -> Result<u64, String> {
    let t = s.trim();
    let split = t.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let n: f64 = num.parse().map_err(|_| format!("invalid size: {s}"))?;
    let mult = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "k" | "kb" | "kib" => 1024.0,
        "m" | "mb" | "mib" => 1024f64.powi(2),
        "g" | "gb" | "gib" => 1024f64.powi(3),
        "t" | "tb" | "tib" => 1024f64.powi(4),
        _ => return Err(format!("unknown size unit in: {s}")),
    };
    Ok((n * mult) as u64)
}

/// Path exclusion rule; a matching folder hides everything below it.
pub enum Matcher {
    /// Entry name equals the pattern (case-insensitive), e.g. `.git`.
    Name(String),
    /// Full path starts with the pattern on a folder boundary, e.g. `C:\Windows`.
    Prefix(String),
    /// Glob or `re:` regex matched against the full path.
    Re(Regex),
}

impl Matcher {
    pub fn needs_path(&self) -> bool {
        !matches!(self, Matcher::Name(_))
    }

    pub fn is_match(&self, name: &str, path: &str) -> bool {
        match self {
            Matcher::Name(n) => name.to_lowercase() == *n,
            Matcher::Prefix(p) => {
                let lower = path.to_lowercase();
                lower.strip_prefix(p.as_str()).is_some_and(|rest| rest.is_empty() || rest.starts_with('\\'))
            }
            Matcher::Re(re) => re.is_match(path),
        }
    }
}

fn build(re: &str) -> Result<Matcher, String> {
    RegexBuilder::new(re)
        .case_insensitive(true)
        .build()
        .map(Matcher::Re)
        .map_err(|e| format!("invalid pattern `{re}`: {e}"))
}

fn glob_to_regex(p: &str) -> String {
    let p = p.replace('/', "\\");
    // `dir\*` also matches `dir` itself, so the folder is hidden along with its content.
    let (body, dir_suffix) = match p.strip_suffix("\\*") {
        Some(b) if !b.contains(['*', '?']) => (b.to_string(), true),
        _ => (p.clone(), false),
    };
    let mut re = String::from("^");
    for c in body.chars() {
        match c {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            c => re.push_str(&regex::escape(&c.to_string())),
        }
    }
    if dir_suffix {
        re.push_str(r"(\\.*)?");
    }
    re.push('$');
    re
}

/// `re:<regex>` is a regex, a pattern with `*`/`?` is a glob, anything else is a literal
/// name (no path separator) or a path prefix (with a separator).
pub fn parse_pattern(p: &str) -> Result<Matcher, String> {
    if let Some(re) = p.strip_prefix("re:") {
        return build(re);
    }
    if p.contains(['*', '?']) {
        return build(&glob_to_regex(p));
    }
    if p.contains(['\\', '/']) {
        let norm = p.replace('/', "\\").trim_end_matches('\\').to_lowercase();
        return Ok(Matcher::Prefix(norm));
    }
    Ok(Matcher::Name(p.to_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("500KB"), Ok(500 * 1024));
        assert_eq!(parse_size("100mb"), Ok(100 << 20));
        assert_eq!(parse_size("1.5GB"), Ok(3 << 29));
        assert_eq!(parse_size("42"), Ok(42));
        assert!(parse_size("abc").is_err());
        assert!(parse_size("-1MB").is_err());
        assert!(parse_size("5XB").is_err());
    }

    #[test]
    fn patterns() {
        let name = parse_pattern(".git").unwrap();
        assert!(name.is_match(".GIT", "C:\\x\\.GIT"));
        assert!(!name.is_match(".github", "C:\\x\\.github"));

        let prefix = parse_pattern("C:\\Windows").unwrap();
        assert!(prefix.is_match("Windows", "C:\\windows"));
        assert!(prefix.is_match("System32", "C:\\Windows\\System32"));
        assert!(!prefix.is_match("Windows.old", "C:\\Windows.old"));

        let glob = parse_pattern("C:\\Windows\\*").unwrap();
        assert!(glob.is_match("Windows", "C:\\Windows"));
        assert!(glob.is_match("a", "C:\\Windows\\a\\b"));
        assert!(!glob.is_match("Windows.old", "C:\\Windows.old"));

        let ext = parse_pattern("*.tmp").unwrap();
        assert!(ext.is_match("a.tmp", "a.tmp"));

        let re = parse_pattern("re:\\.(obj|pdb)$").unwrap();
        assert!(re.is_match("a.pdb", "C:\\b\\a.pdb"));
        assert!(parse_pattern("re:(").is_err());
    }
}
