//! Small helpers that reproduce the Python behaviour Runner Fleet relied on,
//! so scripts, messages and parsing stay byte-compatible with the original.

use serde_json::Value;

/// `shlex.quote`.
pub fn sh_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".into();
    }
    let safe = value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if safe {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }
}

/// PowerShell single-quoted literal. PowerShell also ends a single-quoted
/// string at the typographic quotes U+2018 to U+201B, so each of those is
/// doubled like `'`.
pub fn ps_quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for c in value.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// A remote POSIX path where a leading `~` expands on the worker.
pub fn sh_remote_path(path: &str) -> String {
    if path == "~" {
        return "\"$HOME\"".into();
    }
    if let Some(suffix) = path.strip_prefix("~/") {
        return format!("\"$HOME\"/{}", sh_quote(suffix));
    }
    sh_quote(path)
}

/// Python's `repr()` of a string, used in error messages.
pub fn repr(value: &str) -> String {
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(value.len() + 2);
    out.push(quote);
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python's `str()` of a JSON value.
pub fn py_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Null => "None".into(),
        other => other.to_string(),
    }
}

/// Python's `bool()` of a JSON value.
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Bool(b) => *b,
        Value::Null => false,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `str.splitlines()`.
pub fn splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let breaks = matches!(
            c,
            '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        if !breaks {
            continue;
        }
        lines.push(&text[start..i]);
        let mut end = i + c.len_utf8();
        if c == '\r'
            && let Some(&(j, '\n')) = chars.peek()
        {
            chars.next();
            end = j + 1;
        }
        start = end;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// Which path flavour a host uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathStyle {
    Posix,
    Windows,
}

/// One component of a Python `PurePosixPath`/`PureWindowsPath`, enough for
/// the inventory's checks: separators collapsed, `.` dropped, `..` kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurePath {
    pub style: PathStyle,
    /// Drive (`C:`) or UNC share (`\\server\share`) for Windows paths.
    pub drive: String,
    /// The path starts at a root separator.
    pub rooted: bool,
    pub parts: Vec<String>,
}

impl PurePath {
    pub fn posix(value: &str) -> Self {
        let rooted = value.starts_with('/');
        let parts = value
            .split('/')
            .filter(|p| !p.is_empty() && *p != ".")
            .map(str::to_string)
            .collect();
        Self {
            style: PathStyle::Posix,
            drive: String::new(),
            rooted,
            parts,
        }
    }

    pub fn windows(value: &str) -> Self {
        let normalized = value.replace('/', "\\");
        let mut rest = normalized.as_str();
        let mut drive = String::new();
        if let Some(unc) = rest.strip_prefix("\\\\") {
            let mut pieces = unc.splitn(3, '\\');
            let server = pieces.next().unwrap_or("");
            let share = pieces.next().unwrap_or("");
            if !server.is_empty() && !share.is_empty() {
                drive = format!("\\\\{server}\\{share}");
                rest = &rest[drive.len()..];
            }
        } else if rest.len() >= 2 && rest.as_bytes()[1] == b':' && rest.as_bytes()[0].is_ascii_alphabetic() {
            drive = rest[..2].to_string();
            rest = &rest[2..];
        }
        let rooted = rest.starts_with('\\') || drive.starts_with("\\\\");
        let parts = rest
            .split('\\')
            .filter(|p| !p.is_empty() && *p != ".")
            .map(str::to_string)
            .collect();
        Self {
            style: PathStyle::Windows,
            drive,
            rooted,
            parts,
        }
    }

    pub fn new(value: &str, style: PathStyle) -> Self {
        match style {
            PathStyle::Windows => Self::windows(value),
            PathStyle::Posix => Self::posix(value),
        }
    }

    fn separator(&self) -> &'static str {
        match self.style {
            PathStyle::Windows => "\\",
            PathStyle::Posix => "/",
        }
    }

    pub fn is_absolute(&self) -> bool {
        match self.style {
            PathStyle::Windows => !self.drive.is_empty() && self.rooted,
            PathStyle::Posix => self.rooted,
        }
    }

    /// Python's `.parts`: the anchor (if any) counts as the first part.
    pub fn all_parts(&self) -> Vec<String> {
        let mut out = Vec::new();
        let anchor = self.anchor();
        if !anchor.is_empty() {
            out.push(anchor);
        }
        out.extend(self.parts.iter().cloned());
        out
    }

    fn anchor(&self) -> String {
        let sep = self.separator();
        match (self.drive.is_empty(), self.rooted) {
            (true, true) => sep.into(),
            (true, false) => String::new(),
            (false, true) => format!("{}{sep}", self.drive),
            (false, false) => self.drive.clone(),
        }
    }

    pub fn join(&self, component: &str) -> String {
        let mut joined = self.clone();
        joined.parts.extend(Self::new(component, self.style).parts);
        joined.to_string()
    }
}

impl std::fmt::Display for PurePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sep = self.separator();
        let anchor = self.anchor();
        let body = self.parts.join(sep);
        if anchor.is_empty() && body.is_empty() {
            return f.write_str(".");
        }
        write!(f, "{anchor}{body}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shlex_quote_matches_python() {
        assert_eq!(sh_quote(""), "''");
        assert_eq!(sh_quote("runner-01"), "runner-01");
        assert_eq!(sh_quote("SoloTerm trusted builders"), "'SoloTerm trusted builders'");
        assert_eq!(sh_quote("it's"), "'it'\"'\"'s'");
    }

    #[test]
    fn powershell_quotes_double_every_single_quote_character() {
        assert_eq!(ps_quote(""), "''");
        assert_eq!(ps_quote("C:\\gh-runners\\a b"), "'C:\\gh-runners\\a b'");
        assert_eq!(ps_quote("it's"), "'it''s'");
        for quote in ['\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}'] {
            assert_eq!(ps_quote(&format!("a{quote}b")), format!("'a{quote}{quote}b'"));
        }
        assert_eq!(
            ps_quote("x\u{2019}; Remove-Item C:\\ #"),
            "'x\u{2019}\u{2019}; Remove-Item C:\\ #'"
        );
    }

    #[test]
    fn remote_paths_expand_home_on_the_worker() {
        assert_eq!(
            sh_remote_path("~/gh-runners/soloterm/runner-01"),
            "\"$HOME\"/gh-runners/soloterm/runner-01"
        );
        assert_eq!(sh_remote_path("~"), "\"$HOME\"");
        assert_eq!(sh_remote_path("/opt/a b"), "'/opt/a b'");
    }

    #[test]
    fn repr_matches_python() {
        assert_eq!(repr("abc"), "'abc'");
        assert_eq!(repr("it's"), "\"it's\"");
        assert_eq!(repr("C:\\x"), "'C:\\\\x'");
    }

    #[test]
    fn splitlines_matches_python() {
        assert_eq!(splitlines("a\r\nb\rc\n"), vec!["a", "b", "c"]);
        assert_eq!(splitlines(""), Vec::<&str>::new());
        assert_eq!(splitlines("a\n\nb"), vec!["a", "", "b"]);
    }

    #[test]
    fn pure_paths_match_python() {
        assert_eq!(
            PurePath::windows("C:\\gh-runners\\soloterm").join("runner-01"),
            "C:\\gh-runners\\soloterm\\runner-01"
        );
        assert_eq!(
            PurePath::windows("C:/gh-runners").all_parts(),
            vec!["C:\\", "gh-runners"]
        );
        assert!(PurePath::windows("C:\\actions-runner").is_absolute());
        assert!(!PurePath::windows("\\actions-runner").is_absolute());
        assert!(!PurePath::windows("C:actions-runner").is_absolute());
        assert_eq!(
            PurePath::posix("~/gh-runners//soloterm/").join("runner-01"),
            "~/gh-runners/soloterm/runner-01"
        );
        assert_eq!(PurePath::posix("../runner-01").parts, vec!["..", "runner-01"]);
        assert_eq!(PurePath::posix("/").all_parts(), vec!["/"]);
        assert_eq!(
            PurePath::windows("\\\\srv\\share\\x").all_parts(),
            vec!["\\\\srv\\share\\", "x"]
        );
    }
}
