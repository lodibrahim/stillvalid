//! Indexer: extract code references (paths, symbols, error strings, stack
//! frames) from an issue's title and body. Pure text processing; later
//! heuristics match the results against the repo.

use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefKind {
    Path,
    Symbol,
    Error,
    Frame,
}

/// One reference found in issue text. `text` is the canonical form used for
/// deduplication: `path` or `path:line` for paths, the symbol name, the error
/// message, or `symbol at path:line` for frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub kind: RefKind,
    pub text: String,
    pub path: Option<String>,
    pub line: Option<u32>,
}

const MAX_ERROR_LEN: usize = 200;

const TRAILING_PUNCT: [char; 6] = ['.', ',', ':', ';', '!', '?'];

/// Extensions accepted for a bare `name.ext` with no directory part.
const CODE_EXTS: &[&str] = &[
    "rs", "py", "pyi", "js", "mjs", "cjs", "jsx", "ts", "tsx", "go", "java", "kt", "scala", "c",
    "h", "cc", "cpp", "hpp", "cs", "rb", "php", "swift", "sh", "bash", "zsh", "ps1", "toml", "yml",
    "yaml", "json", "md", "txt", "lock", "cfg", "ini", "conf", "xml", "html", "css", "scss", "sql",
    "proto", "gradle", "cmake", "mk",
];

/// Paths that can never be in the scanned repo (toolchains, dependencies).
const NOISE_PATHS: &[&str] = &[
    "/rustc/",
    ".cargo/registry",
    "site-packages",
    "node_modules",
    "/lib/python",
    "node:",
    "library/std/",
    "library/core/",
    "library/alloc/",
];

/// Runtime and standard-library symbol prefixes.
const NOISE_SYMBOLS: &[&str] = &[
    "std::",
    "core::",
    "alloc::",
    "tokio::",
    "__rust",
    "rust_begin_unwind",
    "runtime.",
    "testing.",
    "java.",
    "javax.",
    "jdk.",
    "sun.",
    "kotlin.",
];

static URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"https?://[^\s)\]>"'`]+"#).unwrap());
static BLOB_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^https?://github\.com/[^/]+/[^/]+/blob/[^/]+/([^#?]+)(?:#L(\d+))?").unwrap()
});
static TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\w.~@+\-/\\:]+").unwrap());
static LINE_SUFFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.*?):(\d+)(?::\d+)?$").unwrap());
static CODE_SPAN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`([^`]+)`").unwrap());
static IDENT_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_]\w*(?:(?:::|\.)[A-Za-z_]\w*)*(?:\(\))?$").unwrap());
static CAMEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[a-z][A-Z]").unwrap());

static RUST_FRAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d+:\s+(?:0x[0-9a-fA-F]+ - )?(<.+>\S*|\S+)$").unwrap());
static AT_LOCATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^at\s+(?:async\s+)?(?:new\s+)?(?:(\S+)\s+\()?(\S+?):(\d+)(?::\d+)?\)?$").unwrap()
});
static JAVA_FRAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^at\s+([\w$.<>/]+)\(([\w$]+\.(?:java|kt|scala|groovy)):(\d+)\)").unwrap()
});
static PYTHON_FRAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^File "([^"]+)", line (\d+)(?:, in (\S+))?"#).unwrap());
static GO_FUNC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([\w./\-]+(?:\.\(\*?\w+\))?\.[\w.]+)\(.*\)$").unwrap());
static GO_LOCATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\S+\.go):(\d+)(?: \+0x[0-9a-fA-F]+)?$").unwrap());
static PANIC_OLD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"panicked at '(.+)', (\S+?):(\d+)(?::\d+)?$").unwrap());
static PANIC_NEW: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"panicked at (\S+?):(\d+):\d+:\s*(.*)$").unwrap());
static ERROR_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:(?:error|Error|ERROR)(?:\[E\d+\])?|fatal|panic|(?:[a-z]\w*\.)*[A-Z]\w*(?:Error|Exception)): \S")
        .unwrap()
});

/// Extract references from an issue, deduplicated by kind and canonical
/// text, in order of first appearance (title before body).
pub fn extract(title: &str, body: &str) -> Vec<Reference> {
    let text = format!("{title}\n{body}");
    let mut found = Found::default();

    let mut lines = Vec::new();
    let mut start = 0;
    for raw in text.split('\n') {
        lines.push((start, raw.trim_end_matches('\r')));
        start += raw.len() + 1;
    }

    let mut i = 0;
    while i < lines.len() {
        let (line_start, raw) = lines[i];
        let stripped = raw.trim_start().trim_start_matches('>').trim_start();
        let at = line_start + raw.len() - stripped.len();
        let next = lines.get(i + 1).map(|(_, l)| l.trim());

        if let Some(consumed_next) = frame(stripped, next, at, &mut found) {
            i += if consumed_next { 2 } else { 1 };
            continue;
        }
        if panic(stripped, next, at, &mut found) {
            i += 1;
            continue;
        }
        if ERROR_LINE.is_match(stripped) {
            found.push(at, error_ref(stripped));
        }
        scan_prose(raw, line_start, &mut found);
        i += 1;
    }

    found.finish()
}

#[derive(Default)]
struct Found {
    refs: Vec<(usize, usize, Reference)>,
}

impl Found {
    fn push(&mut self, offset: usize, r: Reference) {
        let seq = self.refs.len();
        self.refs.push((offset, seq, r));
    }

    fn finish(mut self) -> Vec<Reference> {
        self.refs.sort_by_key(|(offset, seq, _)| (*offset, *seq));
        let mut seen = HashSet::new();
        self.refs
            .into_iter()
            .map(|(_, _, r)| r)
            .filter(|r| seen.insert((r.kind, r.text.clone())))
            .collect()
    }
}

/// Match a stack frame starting at `line`. Returns `Some(true)` when the
/// frame also used the next line (Rust and Go print location separately).
fn frame(line: &str, next: Option<&str>, at: usize, found: &mut Found) -> Option<bool> {
    if let Some(c) = PYTHON_FRAME.captures(line) {
        let symbol = c.get(3).map(|m| m.as_str());
        push_frame(found, at, symbol, Some(&c[1]), c[2].parse().ok());
        return Some(false);
    }
    if let Some(c) = JAVA_FRAME.captures(line) {
        // Java frames name only the file, so the class decides what is noise.
        if !is_noise_symbol(&c[1]) {
            push_frame(found, at, Some(&c[1]), Some(&c[2]), c[3].parse().ok());
        }
        return Some(false);
    }
    if let Some(c) = AT_LOCATION.captures(line) {
        let symbol = c.get(1).map(|m| m.as_str());
        push_frame(found, at, symbol, Some(&c[2]), c[3].parse().ok());
        return Some(false);
    }
    if let Some(c) = RUST_FRAME.captures(line) {
        let symbol = strip_rust_hash(&c[1]);
        if let Some(n) = next.and_then(|n| AT_LOCATION.captures(n)) {
            if n.get(1).is_none() {
                push_frame(found, at, Some(symbol), Some(&n[2]), n[3].parse().ok());
                return Some(true);
            }
        }
        if symbol.contains("::") {
            push_frame(found, at, Some(symbol), None, None);
            return Some(false);
        }
        return None;
    }
    if let Some(c) = GO_FUNC.captures(line) {
        if let Some(n) = next.and_then(|n| GO_LOCATION.captures(n)) {
            push_frame(found, at, Some(&c[1]), Some(&n[1]), n[2].parse().ok());
            return Some(true);
        }
    }
    None
}

fn push_frame(
    found: &mut Found,
    at: usize,
    symbol: Option<&str>,
    path: Option<&str>,
    line: Option<u32>,
) {
    let path = path.map(normalize_path);
    let path_noise = path.as_deref().is_some_and(is_noise_path);
    let symbol = symbol.filter(|s| !path_noise && !is_noise_symbol(s));
    let path = path.filter(|_| !path_noise);
    if symbol.is_none() && path.is_none() {
        return;
    }

    let location = path.as_deref().map(|p| path_text(p, line));
    let text = match (symbol, &location) {
        (Some(s), Some(l)) => format!("{s} at {l}"),
        (Some(s), None) => s.to_string(),
        (None, Some(l)) => l.clone(),
        (None, None) => unreachable!(),
    };
    found.push(
        at,
        Reference {
            kind: RefKind::Frame,
            text,
            path: path.clone(),
            line: path.as_ref().and(line),
        },
    );
    if let Some(s) = symbol {
        found.push(at, symbol_ref(frame_symbol(s)));
    }
    if let Some(p) = path {
        found.push(at, path_ref(p, line));
    }
}

/// Match a Rust panic line; emits the message as an Error and the location
/// as a Path.
fn panic(line: &str, next: Option<&str>, at: usize, found: &mut Found) -> bool {
    let (message, path, line_no) = if let Some(c) = PANIC_OLD.captures(line) {
        (c[1].to_string(), c[2].to_string(), c[3].parse().ok())
    } else if let Some(c) = PANIC_NEW.captures(line) {
        let message = match c[3].trim() {
            "" => next.unwrap_or("").to_string(),
            m => m.to_string(),
        };
        (message, c[1].to_string(), c[2].parse().ok())
    } else {
        return false;
    };
    if !message.is_empty() {
        found.push(at, error_ref(&message));
    }
    let path = normalize_path(&path);
    if !is_noise_path(&path) {
        found.push(at, path_ref(path, line_no));
    }
    true
}

/// Scan free text for blob URLs, paths, and backticked symbols. Other URLs
/// are blanked out so their parts are not mistaken for paths.
fn scan_prose(line: &str, line_start: usize, found: &mut Found) {
    let mut masked = line.to_string();
    for m in URL.find_iter(line) {
        if let Some(c) = BLOB_URL.captures(m.as_str()) {
            let path = normalize_path(c[1].trim_end_matches(TRAILING_PUNCT));
            found.push(
                line_start + m.start(),
                path_ref(path, c.get(2).and_then(|l| l.as_str().parse().ok())),
            );
        }
        masked.replace_range(m.range(), &" ".repeat(m.len()));
    }

    let mut span_ranges = Vec::new();
    for c in CODE_SPAN.captures_iter(&masked) {
        let m = c.get(1).unwrap();
        let s = m.as_str().trim();
        let r = match as_path(s) {
            Some((path, line_no)) => path_ref(path, line_no),
            None if is_symbol(s) => symbol_ref(s.trim_end_matches("()")),
            None => continue,
        };
        span_ranges.push(m.range());
        found.push(line_start + m.start(), r);
    }

    for m in TOKEN.find_iter(&masked) {
        if span_ranges.iter().any(|r| r.contains(&m.start())) {
            continue;
        }
        if let Some((path, line_no)) = as_path(m.as_str()) {
            found.push(line_start + m.start(), path_ref(path, line_no));
        }
    }
}

/// Classify a token as a repo-relative-looking file path with optional line.
fn as_path(token: &str) -> Option<(String, Option<u32>)> {
    let token = token.trim_end_matches(TRAILING_PUNCT);
    if token.contains("::") {
        return None;
    }
    let (path, line) = match LINE_SUFFIX.captures(token) {
        Some(c) => (c.get(1).unwrap().as_str(), c[2].parse().ok()),
        None => (token, None),
    };
    let path = normalize_path(path);
    let (dir, file) = match path.rsplit_once('/') {
        Some((d, f)) => (Some(d), f),
        None => (None, path.as_str()),
    };
    let (name, ext) = file.rsplit_once('.')?;
    let ext_ok = ext.len() <= 5
        && ext.chars().all(|c| c.is_ascii_alphanumeric())
        && ext.chars().any(|c| c.is_ascii_alphabetic());
    if !ext_ok || path.contains("//") || (path.contains(':') && !is_drive_path(&path)) {
        return None;
    }
    match dir {
        Some(d) => {
            let first = d.split('/').next().unwrap_or("");
            if first.contains('.') && !first.starts_with('.') {
                return None;
            }
        }
        None => {
            if !name.chars().any(|c| c.is_ascii_alphabetic()) || !CODE_EXTS.contains(&ext) {
                return None;
            }
        }
    }
    if is_noise_path(&path) {
        return None;
    }
    Some((path, line))
}

fn is_drive_path(path: &str) -> bool {
    let b = path.as_bytes();
    b.len() > 2
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && b[2] == b'/'
        && !path[2..].contains(':')
}

/// A backticked span looks like code: `a::b`, `a.b`, `f()`, `snake_case`,
/// or `CamelCase` with an inner capital. Plain words are not symbols.
fn is_symbol(s: &str) -> bool {
    IDENT_PATH.is_match(s)
        && (s.contains("::")
            || s.contains('.')
            || s.ends_with("()")
            || s.contains('_')
            || CAMEL.is_match(s))
}

fn is_noise_path(path: &str) -> bool {
    path.starts_with("/usr/") || NOISE_PATHS.iter().any(|n| path.contains(n))
}

/// `<module>`, `Object.<anonymous>`, and std/runtime frames are noise;
/// `<myapp::Pool as Drop>::drop` is not.
fn is_noise_symbol(symbol: &str) -> bool {
    let s = symbol.trim_start_matches('<');
    (symbol.starts_with('<') && !s.contains("::"))
        || symbol.contains("<anonymous>")
        || NOISE_SYMBOLS.iter().any(|n| s.starts_with(n))
}

/// Drop the `::h<16 hex>` hash Rust appends to backtrace symbols.
fn strip_rust_hash(symbol: &str) -> &str {
    match symbol.rsplit_once("::h") {
        Some((head, hash)) if hash.len() == 16 && hash.chars().all(|c| c.is_ascii_hexdigit()) => {
            head
        }
        _ => symbol,
    }
}

/// Go symbols carry the package import path (`github.com/o/r/pkg.Func`);
/// keep the part after the last `/`.
fn frame_symbol(symbol: &str) -> &str {
    symbol.rsplit_once('/').map_or(symbol, |(_, s)| s)
}

fn normalize_path(path: &str) -> String {
    let path = path
        .strip_prefix("file://")
        .unwrap_or(path)
        .replace('\\', "/");
    path.strip_prefix("./").map(str::to_string).unwrap_or(path)
}

fn path_text(path: &str, line: Option<u32>) -> String {
    match line {
        Some(l) => format!("{path}:{l}"),
        None => path.to_string(),
    }
}

fn path_ref(path: String, line: Option<u32>) -> Reference {
    Reference {
        kind: RefKind::Path,
        text: path_text(&path, line),
        path: Some(path),
        line,
    }
}

fn symbol_ref(symbol: &str) -> Reference {
    Reference {
        kind: RefKind::Symbol,
        text: symbol.to_string(),
        path: None,
        line: None,
    }
}

fn error_ref(message: &str) -> Reference {
    let message = message.trim();
    let end = message
        .char_indices()
        .nth(MAX_ERROR_LEN)
        .map_or(message.len(), |(i, _)| i);
    Reference {
        kind: RefKind::Error,
        text: message[..end].to_string(),
        path: None,
        line: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use RefKind::*;

    const RUST_PANIC: &str = "Running `stillvalid scan` crashes:

```
thread 'main' panicked at src/compaction.rs:88:14:
index out of bounds: the len is 0 but the index is 0
stack backtrace:
   0: rust_begin_unwind
             at /rustc/abc123/library/std/src/panicking.rs:652:5
   1: core::panicking::panic_bounds_check
             at /rustc/abc123/library/core/src/panicking.rs:208:5
   2: stillvalid::compaction::compact_table
             at ./src/compaction.rs:88:14
   3: stillvalid::main::h0123456789abcdef
             at ./src/main.rs:12:5
```";

    const RUST_OLD_PANIC: &str = "\
> thread 'main' panicked at 'called `Option::unwrap()` on a `None` value', src\\pool.rs:42:5
> note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace";

    const PYTHON: &str = "```
Traceback (most recent call last):
  File \"/home/u/proj/tools/gen.py\", line 10, in <module>
    main()
  File \"/home/u/proj/tools/gen.py\", line 6, in main
    load(path)
  File \"/usr/lib/python3.11/json/__init__.py\", line 293, in load
    return loads(fp.read())
ValueError: invalid literal for int() with base 10: 'x'
```";

    const JS: &str = "```
TypeError: Cannot read properties of undefined (reading 'id')
    at renderRow (/app/src/table.js:42:17)
    at Array.map (<anonymous>)
    at Object.<anonymous> (/app/src/index.js:7:3)
    at Module._compile (node:internal/modules/cjs/loader:1256:14)
```";

    const GO: &str = "```
panic: runtime error: index out of range [3] with length 3

goroutine 1 [running]:
main.compact(...)
\t/home/u/proj/compact.go:88 +0x1d
runtime.main()
\t/usr/local/go/src/runtime/proc.go:267 +0x2bb
```";

    const JAVA: &str = "```
java.lang.IllegalStateException: pool closed
\tat com.example.Pool.release(Pool.java:42)
\tat java.base/java.lang.Thread.run(Thread.java:833)
```";

    const BLOB_URL: &str = "The check is here: \
https://github.com/o/r/blob/0a1b2c3/src/pool.rs#L10-L20 but see \
https://github.com/o/r/issues/12 and https://docs.rs/foo/1.2.3/foo/index.html. \
Repro: https://github.com/o/repro/blob/main/src/main.rs.";

    const PROSE: &str = "Since v1.2.3 the loop in crates/core/src/lib.rs:88 never exits; \
see also `src/compaction.rs` and `Pool::release`, e.g. in README.md.\r\n\
Error: connection refused";

    const NOISE: &str = "Thanks! Is this `yes` or `no`? Using version 1.2.3 on Ubuntu 22.04, \
see https://example.com/page.html and https://github.com/o/r/pull/5. Works 50/50. \
Branch https://github.com/o/r/tree/ag/freeze-2 at library/std/src/sys/fs/mod.rs:68, regex [^/]*\\\\.py[cod]. \
Run `rg --files -g '*.rs'` with `--no-ignore`.";

    /// (name, title, body, expected kind + text)
    type Case = (
        &'static str,
        &'static str,
        &'static str,
        &'static [(RefKind, &'static str)],
    );

    #[test]
    fn extracts_references() {
        let cases: &[Case] = &[
            (
                "rust panic and backtrace",
                "Panic in `compact_table` when table is empty",
                RUST_PANIC,
                &[
                    (Symbol, "compact_table"),
                    (
                        Error,
                        "index out of bounds: the len is 0 but the index is 0",
                    ),
                    (Path, "src/compaction.rs:88"),
                    (
                        Frame,
                        "stillvalid::compaction::compact_table at src/compaction.rs:88",
                    ),
                    (Symbol, "stillvalid::compaction::compact_table"),
                    (Frame, "stillvalid::main at src/main.rs:12"),
                    (Symbol, "stillvalid::main"),
                    (Path, "src/main.rs:12"),
                ],
            ),
            (
                "old rust panic in a quote",
                "",
                RUST_OLD_PANIC,
                &[
                    (Error, "called `Option::unwrap()` on a `None` value"),
                    (Path, "src/pool.rs:42"),
                ],
            ),
            (
                "python traceback",
                "gen.py crashes",
                PYTHON,
                &[
                    (Path, "gen.py"),
                    (Frame, "/home/u/proj/tools/gen.py:10"),
                    (Path, "/home/u/proj/tools/gen.py:10"),
                    (Frame, "main at /home/u/proj/tools/gen.py:6"),
                    (Symbol, "main"),
                    (Path, "/home/u/proj/tools/gen.py:6"),
                    (
                        Error,
                        "ValueError: invalid literal for int() with base 10: 'x'",
                    ),
                ],
            ),
            (
                "js stack",
                "",
                JS,
                &[
                    (
                        Error,
                        "TypeError: Cannot read properties of undefined (reading 'id')",
                    ),
                    (Frame, "renderRow at /app/src/table.js:42"),
                    (Symbol, "renderRow"),
                    (Path, "/app/src/table.js:42"),
                    (Frame, "/app/src/index.js:7"),
                    (Path, "/app/src/index.js:7"),
                ],
            ),
            (
                "go panic",
                "",
                GO,
                &[
                    (
                        Error,
                        "panic: runtime error: index out of range [3] with length 3",
                    ),
                    (Frame, "main.compact at /home/u/proj/compact.go:88"),
                    (Symbol, "main.compact"),
                    (Path, "/home/u/proj/compact.go:88"),
                ],
            ),
            (
                "java stack",
                "",
                JAVA,
                &[
                    (Error, "java.lang.IllegalStateException: pool closed"),
                    (Frame, "com.example.Pool.release at Pool.java:42"),
                    (Symbol, "com.example.Pool.release"),
                    (Path, "Pool.java:42"),
                ],
            ),
            (
                "github blob url",
                "",
                BLOB_URL,
                &[(Path, "src/pool.rs:10"), (Path, "src/main.rs")],
            ),
            (
                "path and line in prose",
                "",
                PROSE,
                &[
                    (Path, "crates/core/src/lib.rs:88"),
                    (Path, "src/compaction.rs"),
                    (Symbol, "Pool::release"),
                    (Path, "README.md"),
                    (Error, "Error: connection refused"),
                ],
            ),
            ("noise only", "Question about 1.2.3", NOISE, &[]),
        ];
        for (name, title, body, want) in cases {
            let refs = extract(title, body);
            let got: Vec<(RefKind, &str)> =
                refs.iter().map(|r| (r.kind, r.text.as_str())).collect();
            assert_eq!(&got, want, "{name}");
        }
    }

    #[test]
    fn reference_fields() {
        let refs = extract("", "see src/pool.rs:42 and `Pool::release()`");
        assert_eq!(
            refs,
            vec![
                Reference {
                    kind: Path,
                    text: "src/pool.rs:42".into(),
                    path: Some("src/pool.rs".into()),
                    line: Some(42),
                },
                Reference {
                    kind: Symbol,
                    text: "Pool::release".into(),
                    path: None,
                    line: None,
                },
            ]
        );
    }

    #[test]
    fn deduplicates_in_order() {
        let refs = extract(
            "`Pool::release` leaks",
            "`Pool::release` in src/pool.rs, src/pool.rs",
        );
        let texts: Vec<_> = refs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, ["Pool::release", "src/pool.rs"]);
    }

    #[test]
    fn truncates_long_errors() {
        let refs = extract("", &format!("Error: {}", "é".repeat(300)));
        assert_eq!(refs[0].text.chars().count(), MAX_ERROR_LEN);
    }

    /// Throwaway precision check against real issues: set
    /// STILLVALID_EVAL_ISSUES to a `gh issue list --json number,title,body`
    /// file and run with `--ignored --nocapture`.
    #[test]
    #[ignore]
    fn eval_issue_dump() {
        let path = std::env::var("STILLVALID_EVAL_ISSUES").expect("STILLVALID_EVAL_ISSUES");
        let data = std::fs::read_to_string(path).unwrap();
        let issues: Vec<serde_json::Value> = serde_json::from_str(&data).unwrap();
        for issue in issues {
            let refs = extract(
                issue["title"].as_str().unwrap_or(""),
                issue["body"].as_str().unwrap_or(""),
            );
            println!("#{} ({} refs)", issue["number"], refs.len());
            for r in refs {
                println!("  {:?} {}", r.kind, r.text);
            }
        }
    }
}
