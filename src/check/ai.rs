//! Issue check with a language model (Tier 2): find the code an issue is about, ask the model
//! whether the described behavior is still possible there, and keep its verdict only when every
//! line it cites is in the code it was shown.

use super::code;
use crate::fetch::{Issue, Snapshot};
use crate::incremental;
use crate::index::{self, RefKind};
use crate::llm::{Client, LlmError};
use crate::repo::{Repo, RepoError};
use crate::store::{self, Confidence, Evidence, EvidenceType, Item, Kind, Report, Tier, Verdict};
use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::LazyLock;

/// Files shown to the model per issue.
const MAX_FILES: usize = 4;
/// Lines shown around each match.
const CONTEXT_LINES: u32 = 30;
const MAX_FILE_LINES: usize = 120;
/// Code shown per issue, about 6k tokens.
const MAX_CODE_CHARS: usize = 24_000;
const MAX_ISSUE_CHARS: usize = 6_000;
const MAX_SYMBOLS: usize = 8;
const MAX_ERRORS: usize = 4;
const MAX_KEYWORDS: usize = 8;
const MIN_TITLE_WORD_LEN: usize = 5;
/// Matching lines read per file and search.
const GREP_PER_FILE: u32 = 20;
const MAX_REASON_CHARS: usize = 300;
/// Evidence items kept per verdict.
const MAX_CITATIONS: usize = 3;
/// Failed calls in a row before the run stops calling the model.
const MAX_ERRORS_IN_A_ROW: usize = 5;

/// Score of a file the issue names, of a named symbol or error string found in a file, of an
/// identifier or flag name from the issue text found in a file, and of a title word found in a
/// file (each counted once per file).
const NAMED_SCORE: u32 = 10;
const REF_SCORE: u32 = 5;
const IDENTIFIER_SCORE: u32 = 3;
const TITLE_WORD_SCORE: u32 = 1;

const SYSTEM_PROMPT: &str = "You check whether a GitHub issue is still valid against the current code. \
You get the issue and excerpts of the code at the scanned commit; every excerpt line starts with its line number. \
Decide whether the behavior the issue describes is still possible. \
Answer likely_fixed only if the excerpts show code that prevents it, still_valid only if they show code that still causes it, and cant_tell otherwise (including when the excerpts are not enough). \
Cite the lines that show it as {path, line}, using only paths and line numbers from the excerpts. \
Give a short reason (one sentence). Reply with JSON only.";

const STOPWORDS: &[&str] = &[
    "about", "above", "after", "again", "allow", "allows", "always", "another", "because",
    "before", "being", "below", "between", "broken", "cannot", "could", "doesn", "doesnt",
    "during", "error", "every", "expected", "feature", "first", "found", "github", "given",
    "instead", "issue", "might", "never", "other", "should", "shouldn", "since", "still",
    "support", "their", "there", "these", "thing", "things", "those", "through", "under", "until",
    "using", "wants", "where", "which", "while", "without", "would", "wrong",
];

static FLAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[\s`'(\[])--([a-z][a-z0-9]*(?:-[a-z0-9]+)+|[a-z]{4,})\b").unwrap()
});
static IDENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[A-Za-z_][A-Za-z0-9_]{3,}\b").unwrap());
static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[A-Za-z]+\b").unwrap());
static FENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)^\s*```(?:json)?\s*(.*?)\s*```\s*$").unwrap());

/// Code shown to the model from one file at the scanned commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Excerpt {
    pub path: String,
    /// Inclusive line ranges, in order and not overlapping.
    pub ranges: Vec<(u32, u32)>,
    /// The ranges' lines, each prefixed with its number.
    pub text: String,
}

/// What the issue asks the code about, found by [`index::extract`] and in its words.
#[derive(Debug, Default, PartialEq, Eq)]
struct Terms {
    /// Paths it names, with a line if it gives one.
    paths: Vec<(String, Option<u32>)>,
    /// Symbols (whole words) and error strings it names.
    symbols: Vec<String>,
    errors: Vec<String>,
    /// Identifiers and flag names in its text (whole words), then title words (any case).
    identifiers: Vec<String>,
    title_words: Vec<String>,
}

fn terms(issue: &Issue) -> Terms {
    let body = issue.body.as_deref().unwrap_or("");
    let mut t = Terms::default();
    let mut seen = HashSet::new();
    for r in index::extract(&issue.title, body) {
        if let (RefKind::Path | RefKind::Frame, Some(path)) = (r.kind, &r.path) {
            if !t.paths.iter().any(|(p, _)| p == path) {
                t.paths.push((path.clone(), r.line));
            }
        }
        let symbol = match r.kind {
            RefKind::Path => continue,
            RefKind::Error => {
                if t.errors.len() < MAX_ERRORS && r.text.chars().count() >= 20 {
                    t.errors.push(r.text);
                }
                continue;
            }
            RefKind::Symbol => r.text.as_str(),
            // A frame's text is `symbol at path:line`.
            RefKind::Frame => r.text.split(" at ").next().unwrap_or_default(),
        };
        let needle = code::symbol_needle(symbol);
        if t.symbols.len() < MAX_SYMBOLS
            && needle.chars().count() >= 4
            && seen.insert(needle.to_string())
        {
            t.symbols.push(needle.to_string());
        }
    }
    // Links carry names (owners, repos) that are not in the code.
    let text = index::URL
        .replace_all(&format!("{}\n{body}", issue.title), "")
        .into_owned();
    let flags = FLAG.captures_iter(&text).map(|c| c[1].to_string());
    // Identifiers only people who read the code write: snake_case or camelCase.
    let idents = IDENT
        .find_iter(&text)
        .map(|m| m.as_str())
        .filter(|w| w.trim_matches('_').contains('_') || index::CAMEL.is_match(w))
        .map(str::to_string);
    for word in flags.chain(idents) {
        if t.identifiers.len() < MAX_KEYWORDS && seen.insert(word.clone()) {
            t.identifiers.push(word);
        }
    }
    for word in WORD
        .find_iter(&issue.title)
        .map(|m| m.as_str().to_ascii_lowercase())
    {
        let room = t.identifiers.len() + t.title_words.len() < MAX_KEYWORDS;
        if room
            && word.len() >= MIN_TITLE_WORD_LEN
            && !STOPWORDS.contains(&word.as_str())
            && seen.insert(word.clone())
        {
            t.title_words.push(word);
        }
    }
    t
}

fn scored(needles: &[String], score: u32) -> impl Iterator<Item = (&str, u32)> {
    needles.iter().map(move |n| (n.as_str(), score))
}

/// One `git grep` over the scanned commit: needles with their scores, matched as whole words
/// with `word`, in any case with `ignore_case`.
struct Search<'a> {
    needles: Vec<(&'a str, u32)>,
    word: bool,
    ignore_case: bool,
}

#[derive(Debug, Default)]
struct Hits {
    score: u32,
    /// Lines the issue names, then matching lines.
    named: BTreeSet<u32>,
    lines: BTreeSet<u32>,
}

/// The files at `repo.head_sha` most likely to show whether `issue` still holds, with excerpts
/// around what matched: files the issue names, then files holding its symbols, error strings,
/// identifiers, and title words. Files other than source code count only when named.
pub fn retrieve(
    issue: &Issue,
    repo: &Repo,
    token: Option<&str>,
) -> Result<Vec<Excerpt>, RepoError> {
    let t = terms(issue);
    let head = repo.head_sha.as_str();
    let mut files: HashMap<String, Hits> = HashMap::new();
    let mut head_files = None;
    for (path, line) in &t.paths {
        let Some(path) = code::resolve_path(repo, head, path, &mut head_files)? else {
            continue;
        };
        let hits = files.entry(path).or_default();
        hits.score += NAMED_SCORE;
        hits.named.extend(*line);
    }
    let named: HashSet<String> = files.keys().cloned().collect();

    let searches = [
        Search {
            needles: scored(&t.symbols, REF_SCORE)
                .chain(scored(&t.identifiers, IDENTIFIER_SCORE))
                .collect(),
            word: true,
            ignore_case: false,
        },
        Search {
            needles: scored(&t.errors, REF_SCORE).collect(),
            word: false,
            ignore_case: false,
        },
        Search {
            needles: scored(&t.title_words, TITLE_WORD_SCORE).collect(),
            word: true,
            ignore_case: true,
        },
    ];
    for Search {
        needles,
        word,
        ignore_case,
    } in searches
    {
        let refs: Vec<&str> = needles.iter().map(|(n, _)| *n).collect();
        let mut found: HashSet<(String, usize)> = HashSet::new();
        for (path, line, text) in
            repo.grep_all(head, &refs, word, ignore_case, GREP_PER_FILE, token)?
        {
            if !named.contains(&path) && !is_source(&path) {
                continue;
            }
            let text = match ignore_case {
                true => text.to_ascii_lowercase(),
                false => text,
            };
            let hits = files.entry(path.clone()).or_default();
            hits.lines.insert(line);
            for (i, (needle, score)) in needles.iter().enumerate() {
                if text.contains(needle) && found.insert((path.clone(), i)) {
                    hits.score += *score;
                }
            }
        }
    }

    let mut ranked: Vec<(String, Hits)> = files.into_iter().collect();
    ranked.sort_by(|a, b| b.1.score.cmp(&a.1.score).then_with(|| a.0.cmp(&b.0)));
    let mut excerpts = Vec::new();
    let mut budget = MAX_CODE_CHARS;
    for (path, hits) in ranked.into_iter().take(MAX_FILES) {
        let content = repo.read_file(head, &path, token)?;
        if content.contains('\0') {
            continue;
        }
        if let Some(e) = excerpt(&path, &content, &hits.named, &hits.lines, budget) {
            budget -= e.text.len();
            excerpts.push(e);
        }
    }
    Ok(excerpts)
}

/// Whether `path` is source code: a known code extension ([`index::CODE_EXTS`]) that is not a
/// docs or config file.
fn is_source(path: &str) -> bool {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    ext.is_some_and(|e| index::CODE_EXTS.contains(&e.as_str())) && code::is_code_file(path)
}

/// Lines of `content` within [`CONTEXT_LINES`] of `named` (lines the issue names) and `hits`
/// (the first lines when there are none), at most [`MAX_FILE_LINES`] lines and `budget`
/// characters, windows around `named` lines first; `None` when nothing fits.
fn excerpt(
    path: &str,
    content: &str,
    named: &BTreeSet<u32>,
    hits: &BTreeSet<u32>,
    budget: usize,
) -> Option<Excerpt> {
    const GAP: &str = "...\n";
    let lines: Vec<&str> = content.lines().collect();
    let last = u32::try_from(lines.len()).ok()?;
    let centers: BTreeSet<u32> = match named.is_empty() && hits.is_empty() {
        true => BTreeSet::from([1]),
        false => named
            .union(hits)
            .copied()
            .filter(|&l| l >= 1 && l <= last)
            .collect(),
    };
    let mut windows: Vec<(u32, u32)> = Vec::new();
    for c in centers {
        let (start, end) = (
            c.saturating_sub(CONTEXT_LINES).max(1),
            (c + CONTEXT_LINES).min(last),
        );
        match windows.last_mut() {
            Some(prev) if start <= prev.1 + 1 => prev.1 = prev.1.max(end),
            _ => windows.push((start, end)),
        }
    }
    // Stable: named windows first, each group in file order.
    windows.sort_by_key(|&(start, end)| named.range(start..=end).next().is_none());
    let render = |n: u32| format!("{n}| {}\n", lines[n as usize - 1]);
    let (mut lines_left, mut chars_left) = (MAX_FILE_LINES, budget);
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for (start, end) in windows {
        let mut written = None;
        for n in start..=end {
            // Every range may need a gap line before it once they are back in file order.
            let cost = render(n).len() + if written.is_none() { GAP.len() } else { 0 };
            if lines_left == 0 || cost > chars_left {
                break;
            }
            lines_left -= 1;
            chars_left -= cost;
            written = Some(n);
        }
        if let Some(n) = written {
            ranges.push((start, n));
        }
        if written != Some(end) {
            break;
        }
    }
    ranges.sort();
    let text = ranges
        .iter()
        .map(|&(start, end)| (start..=end).map(render).collect::<String>())
        .collect::<Vec<_>>()
        .join(GAP);
    (!ranges.is_empty()).then(|| Excerpt {
        path: path.to_string(),
        ranges,
        text,
    })
}

/// The user message: the issue, then each excerpt under its path.
fn prompt(issue: &Issue, head_sha: &str, excerpts: &[Excerpt]) -> String {
    let body = issue.body.as_deref().unwrap_or("");
    let body = match body.char_indices().nth(MAX_ISSUE_CHARS) {
        Some((cut, _)) => format!("{}\n[issue truncated]", &body[..cut]),
        None => body.to_string(),
    };
    let mut out = format!(
        "Issue #{} (filed {}): {}\n\n{body}\n\nCode at commit {head_sha}:\n",
        issue.number,
        issue.created_at.format("%Y-%m-%d"),
        issue.title
    );
    for e in excerpts {
        out.push_str(&format!("\n--- {}\n{}", e.path, e.text));
    }
    out
}

/// JSON schema of the model's answer.
fn answer_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["verdict", "confidence", "reason", "citations"],
        "properties": {
            "verdict": { "type": "string", "enum": ["likely_fixed", "still_valid", "cant_tell"] },
            "confidence": { "type": "string", "enum": ["high", "medium", "low"] },
            "reason": { "type": "string" },
            "citations": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["path", "line"],
                    "properties": {
                        "path": { "type": "string" },
                        "line": { "type": "integer" },
                    },
                },
            },
        },
    })
}

#[derive(Debug, Deserialize)]
struct Answer {
    verdict: Verdict,
    confidence: Confidence,
    reason: String,
    citations: Vec<Citation>,
}

#[derive(Debug, Deserialize)]
struct Citation {
    path: String,
    line: u32,
}

/// The verdict, confidence, and evidence to record for the model's `content`. It stands only
/// when it is `likely_fixed` or `still_valid`, cites at least one line, and every cited line is
/// in `excerpts`; anything else is `cant_tell`. Model confidence is capped: `likely_fixed` is
/// always `low`, `still_valid` at most `medium`.
fn judge(content: &str, excerpts: &[Excerpt]) -> (Verdict, Confidence, Vec<Evidence>) {
    let cant_tell = (Verdict::CantTell, Confidence::Low, Vec::new());
    let json = FENCE.captures(content).map_or(content, |c| {
        c.get(1).expect("group 1 always matches").as_str()
    });
    let Ok(answer) = serde_json::from_str::<Answer>(json) else {
        return cant_tell;
    };
    let shown = |c: &Citation| {
        let path = c.path.trim_start_matches("./");
        excerpts
            .iter()
            .any(|e| e.path == path && e.ranges.iter().any(|&(s, end)| (s..=end).contains(&c.line)))
    };
    let confidence = match answer.verdict {
        Verdict::LikelyFixed => Confidence::Low,
        Verdict::StillValid => answer.confidence.max(Confidence::Medium),
        _ => return cant_tell,
    };
    if answer.citations.is_empty() || !answer.citations.iter().all(shown) {
        return cant_tell;
    }
    let reason: String = answer
        .reason
        .trim()
        .chars()
        .take(MAX_REASON_CHARS)
        .collect();
    let mut seen = HashSet::new();
    let evidence = answer
        .citations
        .iter()
        .map(|c| format!("{}:{}", c.path.trim_start_matches("./"), c.line))
        .filter(|r| seen.insert(r.clone()))
        .take(MAX_CITATIONS)
        .enumerate()
        .map(|(i, reference)| Evidence {
            kind: EvidenceType::Code,
            reference,
            note: match i {
                0 => reason.clone(),
                _ => "Also cited by the model".to_string(),
            },
        })
        .collect();
    (answer.verdict, confidence, evidence)
}

/// Result of [`prepare`].
#[derive(Debug, Default)]
pub struct Prepared {
    /// Excerpts per issue number, for issues with code to show.
    pub excerpts: BTreeMap<u64, Vec<Excerpt>>,
    /// Issues whose code could not be read.
    pub failed: Vec<(u64, RepoError)>,
}

/// Whether the model checks `item`: an issue no heuristic or earlier run settled.
fn is_candidate(item: &Item) -> bool {
    item.kind == Kind::Issue && item.verdict == Verdict::CantTell && item.tier == Tier::None
}

/// Retrieve code for every candidate issue and record the files as its `related_files` (blob
/// SHAs from `blobs`), so [`crate::incremental::reuse`] can then reuse an earlier model verdict
/// whose issue and code are unchanged.
pub fn prepare(
    report: &mut Report,
    snapshot: &Snapshot,
    repo: &Repo,
    blobs: &HashMap<String, String>,
    token: Option<&str>,
) -> Prepared {
    let issues: HashMap<u64, &Issue> = snapshot.issues.iter().map(|i| (i.number, i)).collect();
    let mut prepared = Prepared::default();
    for item in report.items.iter_mut().filter(|i| is_candidate(i)) {
        let Some(issue) = issues.get(&item.number) else {
            continue;
        };
        match retrieve(issue, repo, token) {
            Ok(excerpts) if excerpts.is_empty() => {}
            Ok(excerpts) => {
                let paths = excerpts.iter().map(|e| e.path.as_str());
                item.fingerprint.related_files = Some(incremental::related(paths, blobs));
                prepared.excerpts.insert(item.number, excerpts);
            }
            Err(e) => prepared.failed.push((item.number, e)),
        }
    }
    prepared
}

/// What [`run`] did.
#[derive(Debug, Default)]
pub struct Run {
    pub calls: usize,
    pub likely_fixed: usize,
    pub still_valid: usize,
    /// Answers that fell back to `cant_tell` (the model's own, or an invalid citation).
    pub cant_tell: usize,
    /// Candidates left unchecked for the next run.
    pub left: usize,
    /// Why calls stopped before the candidates ran out.
    pub stopped: Option<String>,
    /// Failed calls (the item stays unchecked).
    pub failed: Vec<(u64, LlmError)>,
}

/// Ask the model about each candidate with excerpts, most-reacted first, then most-commented,
/// then newest, making at most `max_calls` calls. Calls stop early on a rate limit the client
/// won't wait out, a refused key, the endpoint reporting no requests left, or
/// [`MAX_ERRORS_IN_A_ROW`] failures. Unchecked items stay `cant_tell` with tier `none`, so the
/// next run (with `--previous`) reuses this run's answers and continues with them.
pub async fn run(
    report: &mut Report,
    snapshot: &Snapshot,
    prepared: &Prepared,
    client: &mut Client,
    max_calls: usize,
) -> Run {
    let issues: HashMap<u64, &Issue> = snapshot.issues.iter().map(|i| (i.number, i)).collect();
    let mut order: Vec<(usize, &Issue)> = report
        .items
        .iter()
        .enumerate()
        .filter(|(_, item)| is_candidate(item) && prepared.excerpts.contains_key(&item.number))
        .filter_map(|(at, item)| Some((at, *issues.get(&item.number)?)))
        .collect();
    order.sort_by(|(_, a), (_, b)| {
        (b.reactions.total_count, b.comments, b.created_at, a.number).cmp(&(
            a.reactions.total_count,
            a.comments,
            a.created_at,
            b.number,
        ))
    });

    let schema = answer_schema();
    let mut run = Run::default();
    let mut errors_in_a_row = 0;
    for (at, issue) in order {
        if run.stopped.is_some() || run.calls == max_calls {
            run.left += 1;
            continue;
        }
        let excerpts = &prepared.excerpts[&issue.number];
        let user = prompt(issue, &report.head_sha, excerpts);
        run.calls += 1;
        match client
            .complete(SYSTEM_PROMPT, &user, "verdict", &schema)
            .await
        {
            Ok(completion) => {
                errors_in_a_row = 0;
                let (verdict, confidence, evidence) = judge(&completion.content, excerpts);
                match verdict {
                    Verdict::LikelyFixed => run.likely_fixed += 1,
                    Verdict::StillValid => run.still_valid += 1,
                    _ => run.cant_tell += 1,
                }
                let item = &mut report.items[at];
                item.verdict = verdict;
                item.confidence = confidence;
                item.tier = Tier::Llm;
                item.evidence = evidence;
                if completion.exhausted {
                    run.stopped = Some("the endpoint reports no requests left".into());
                }
            }
            Err(e) => {
                errors_in_a_row += 1;
                run.left += 1;
                run.stopped = match &e {
                    LlmError::RateLimited { .. } | LlmError::Auth { .. } => Some(e.to_string()),
                    _ if errors_in_a_row == MAX_ERRORS_IN_A_ROW => {
                        Some(format!("{MAX_ERRORS_IN_A_ROW} failed calls in a row"))
                    }
                    _ => None,
                };
                run.failed.push((issue.number, e));
            }
        }
    }
    if run.stopped.is_none() && run.left > 0 {
        run.stopped = Some(format!("reached --max-llm-calls {max_calls}"));
    }
    report.summary = store::summarize(&report.items);
    run
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::pulls::PullThresholds;
    use chrono::{DateTime, Utc};
    use std::path::Path;
    use std::process::Command;
    use tempfile::TempDir;
    use wiremock::matchers::{body_string_contains, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn sh(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// A repo with one commit of `files`.
    fn repo(files: &[(&str, &str)]) -> (TempDir, Repo) {
        let tmp = TempDir::new().unwrap();
        sh(tmp.path(), &["init", "--quiet", "-b", "main"]);
        for (path, text) in files {
            let full = tmp.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, text).unwrap();
        }
        sh(tmp.path(), &["add", "-A"]);
        sh(tmp.path(), &["commit", "--quiet", "-m", "init"]);
        let head_sha = sh(tmp.path(), &["rev-parse", "HEAD"]);
        let repo = Repo::open_existing(tmp.path(), &head_sha).unwrap();
        (tmp, repo)
    }

    fn ts(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn issue(number: u64, title: &str, body: &str) -> Issue {
        Issue {
            number,
            title: title.into(),
            html_url: format!("https://github.com/acme/rocketdb/issues/{number}"),
            created_at: ts("2026-01-01T00:00:00Z"),
            body: Some(body.into()),
            ..Default::default()
        }
    }

    /// `n` numbered lines of filler, with `line` at `at`.
    fn file(n: u32, at: u32, line: &str) -> String {
        (1..=n)
            .map(|i| match i == at {
                true => format!("{line}\n"),
                false => format!("// filler {i}\n"),
            })
            .collect()
    }

    #[test]
    fn terms_pick_refs_flags_identifiers_and_title_words() {
        let t = terms(&issue(
            1,
            "Panic when compacting with --max-depth",
            "In `src/compaction.rs:88` the call to `Compactor::run_once` fails with\n\
             error: table is empty and cannot be compacted\nuse sst_table or emptyTable (https://github.com/AcmeCorp/rocketDb)",
        ));
        assert_eq!(t.paths, [("src/compaction.rs".to_string(), Some(88))]);
        assert!(t.symbols.contains(&"run_once".to_string()), "{t:?}");
        assert_eq!(t.errors, ["error: table is empty and cannot be compacted"]);
        assert!(t.identifiers.contains(&"max-depth".to_string()), "{t:?}");
        assert!(t.identifiers.contains(&"sst_table".to_string()), "{t:?}");
        assert!(t.identifiers.contains(&"emptyTable".to_string()), "{t:?}");
        assert!(
            !t.identifiers.iter().any(|w| w.starts_with("Acme")),
            "{t:?}"
        );
        assert!(t.title_words.contains(&"panic".to_string()), "{t:?}");
        assert!(t.title_words.contains(&"compacting".to_string()), "{t:?}");
        // Stopwords and short words are skipped.
        assert!(!t.title_words.iter().any(|w| w == "when" || w == "with"));
    }

    #[test]
    fn retrieval_ranks_named_files_then_symbol_hits_and_skips_docs() {
        let (_tmp, repo) = repo(&[
            (
                "src/compaction.rs",
                &file(200, 150, "fn compact() { assert!(!table.is_empty()) }"),
            ),
            ("src/pool.rs", &file(50, 10, "fn run_once() {}")),
            ("src/other.rs", &file(50, 20, "// compacting later")),
            ("docs/guide.md", "run_once is documented here\n"),
            ("doc/rg.1", "run_once in the man page\n"),
        ]);
        let got = retrieve(
            &issue(
                1,
                "Panic when compacting",
                "at src/compaction.rs:150 in `run_once`",
            ),
            &repo,
            None,
        )
        .unwrap();
        let paths: Vec<&str> = got.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["src/compaction.rs", "src/pool.rs", "src/other.rs"]);
        assert_eq!(got[0].ranges, [(120, 180)]);
        assert!(got[0].text.starts_with("120| // filler 120\n"));
        assert!(got[0].text.contains("150| fn compact()"));
        assert_eq!(got[1].ranges, [(1, 40)]);
    }

    #[test]
    fn excerpts_merge_windows_and_respect_the_budgets() {
        let content = file(400, 0, "");
        let hits = BTreeSet::from([40, 60, 300]);
        let none = BTreeSet::new();
        let e = excerpt("a.rs", &content, &none, &hits, usize::MAX).unwrap();
        assert_eq!(e.ranges, [(10, 90), (270, 308)]);
        assert_eq!(
            e.text.lines().filter(|l| *l != "...").count(),
            MAX_FILE_LINES
        );
        assert!(e.text.contains("\n...\n270| "));

        let e = excerpt("a.rs", &content, &none, &hits, 44).unwrap();
        assert_eq!(e.ranges, [(10, 11)]);
        assert!(excerpt("a.rs", &content, &none, &hits, 5).is_none());
        assert_eq!(
            excerpt("a.rs", "x\n", &none, &none, usize::MAX)
                .unwrap()
                .ranges,
            [(1, 1)]
        );

        // The named line is shown even when earlier matches would use up the lines.
        let hits = BTreeSet::from([10, 80, 150]);
        let e = excerpt("a.rs", &content, &BTreeSet::from([350]), &hits, usize::MAX).unwrap();
        assert_eq!(e.ranges, [(1, 40), (50, 68), (320, 380)]);
        assert!(e.text.contains("\n...\n320| "));
    }

    fn shown() -> Vec<Excerpt> {
        vec![Excerpt {
            path: "src/pool.rs".into(),
            ranges: vec![(10, 40), (60, 70)],
            text: String::new(),
        }]
    }

    #[test]
    fn judge_keeps_answers_citing_shown_lines_and_caps_confidence() {
        let (v, c, e) = judge(
            r#"{"verdict":"still_valid","confidence":"high","reason":"Timeout path skips release.","citations":[{"path":"./src/pool.rs","line":12},{"path":"src/pool.rs","line":65},{"path":"src/pool.rs","line":12}]}"#,
            &shown(),
        );
        assert_eq!((v, c), (Verdict::StillValid, Confidence::Medium));
        assert_eq!(
            e,
            [
                Evidence {
                    kind: EvidenceType::Code,
                    reference: "src/pool.rs:12".into(),
                    note: "Timeout path skips release.".into()
                },
                Evidence {
                    kind: EvidenceType::Code,
                    reference: "src/pool.rs:65".into(),
                    note: "Also cited by the model".into()
                },
            ]
        );

        let fenced = "```json\n{\"verdict\":\"likely_fixed\",\"confidence\":\"high\",\"reason\":\"r\",\"citations\":[{\"path\":\"src/pool.rs\",\"line\":40}]}\n```";
        let (v, c, _) = judge(fenced, &shown());
        assert_eq!((v, c), (Verdict::LikelyFixed, Confidence::Low));
    }

    #[test]
    fn judge_turns_unsupported_answers_into_cant_tell() {
        let answer = |verdict: &str, citations: &str| {
            format!(
                r#"{{"verdict":"{verdict}","confidence":"low","reason":"r","citations":[{citations}]}}"#
            )
        };
        for content in [
            // A line outside the excerpts, a file not shown, no citation, the model's own
            // cant_tell, a verdict it may not give, and no JSON at all.
            answer("still_valid", r#"{"path":"src/pool.rs","line":50}"#),
            answer(
                "likely_fixed",
                r#"{"path":"src/pool.rs","line":12},{"path":"src/gone.rs","line":1}"#,
            ),
            answer("likely_fixed", ""),
            answer("cant_tell", r#"{"path":"src/pool.rs","line":12}"#),
            answer("superseded", r#"{"path":"src/pool.rs","line":12}"#),
            "The issue is fixed.".to_string(),
        ] {
            assert_eq!(
                judge(&content, &shown()),
                (Verdict::CantTell, Confidence::Low, Vec::new()),
                "{content}"
            );
        }
    }

    fn snapshot(repo: &Repo, issues: Vec<Issue>) -> Snapshot {
        Snapshot {
            repo: "acme/rocketdb".into(),
            branch: "main".into(),
            head_sha: repo.head_sha.clone(),
            issues,
            pulls: Vec::new(),
            references: Default::default(),
            reopened_at: Default::default(),
            commits_on_branch: Default::default(),
            pull_activity: Default::default(),
        }
    }

    fn report(snapshot: &Snapshot) -> Report {
        let none = BTreeMap::new();
        store::build_report(
            snapshot,
            "pro-ai",
            ts("2026-09-27T00:00:00Z"),
            &PullThresholds::default(),
            &BTreeMap::new(),
            &none,
            &none,
        )
    }

    fn reply(content: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": { "content": content } }]
        }))
    }

    #[tokio::test]
    async fn end_to_end_with_a_mocked_model() {
        let (_tmp, repo) = repo(&[
            (
                "src/pool.rs",
                &file(60, 30, "fn release_on_timeout() { /* skipped */ }"),
            ),
            ("src/cache.rs", &file(60, 20, "fn evict_entry() {}")),
            ("src/lock.rs", &file(60, 5, "fn acquire_lock() {}")),
        ]);
        let mut popular = issue(1, "Leak", "`release_on_timeout` never releases");
        popular.reactions.total_count = 9;
        let mut snapshot = snapshot(
            &repo,
            vec![
                popular,
                issue(2, "Stale cache", "`evict_entry` keeps old data"),
                issue(3, "Deadlock", "`acquire_lock` hangs"),
                issue(4, "Vague", "it is slow"),
            ],
        );
        // Newest first among equal reactions: #3 before #2.
        snapshot.issues[2].created_at = ts("2026-02-01T00:00:00Z");
        let mut report = report(&snapshot);
        let blobs = repo.blob_shas().unwrap();
        let prepared = prepare(&mut report, &snapshot, &repo, &blobs, None);
        assert_eq!(
            prepared.excerpts.keys().copied().collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(
            report.items[0].fingerprint.related_files,
            Some(BTreeMap::from([(
                "src/pool.rs".to_string(),
                format!("blob:{}", blobs["src/pool.rs"])
            )]))
        );

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string_contains("Issue #1 "))
            .respond_with(reply(
                r#"{"verdict":"still_valid","confidence":"medium","reason":"Still skipped.","citations":[{"path":"src/pool.rs","line":30}]}"#,
            ))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("Issue #3 "))
            .respond_with(reply(
                r#"{"verdict":"likely_fixed","confidence":"high","reason":"r","citations":[{"path":"src/lock.rs","line":59}]}"#,
            ))
            .expect(1)
            .mount(&server)
            .await;
        let mut client = Client::new(&server.uri(), None, "m").unwrap();
        let run = run(&mut report, &snapshot, &prepared, &mut client, 2).await;

        assert_eq!(
            (run.calls, run.still_valid, run.cant_tell, run.left),
            (2, 1, 1, 1)
        );
        assert_eq!(run.stopped.as_deref(), Some("reached --max-llm-calls 2"));
        let by_number = |n: u64| report.items.iter().find(|i| i.number == n).unwrap();
        let first = by_number(1);
        assert_eq!(
            (first.verdict, first.confidence, first.tier),
            (Verdict::StillValid, Confidence::Medium, Tier::Llm)
        );
        assert_eq!(first.evidence[0].reference, "src/pool.rs:30");
        // Line 59 was not shown (the window around line 5 ends at 35).
        let third = by_number(3);
        assert_eq!((third.verdict, third.tier), (Verdict::CantTell, Tier::Llm));
        assert!(third.evidence.is_empty());
        assert_eq!(by_number(2).tier, Tier::None);
        assert_eq!(by_number(4).tier, Tier::None);
        assert_eq!(report.summary.issues.still_valid, 1);

        // The next run reuses both answers and asks only about #2.
        let mut next = self::report(&snapshot);
        let prepared = prepare(&mut next, &snapshot, &repo, &blobs, None);
        let counts = crate::incremental::reuse(&mut next, &report);
        assert_eq!(counts.reused, 2);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string_contains("Issue #2 "))
            .respond_with(reply(
                r#"{"verdict":"still_valid","confidence":"low","reason":"r","citations":[{"path":"src/cache.rs","line":20}]}"#,
            ))
            .expect(1)
            .mount(&server)
            .await;
        let mut client = Client::new(&server.uri(), None, "m").unwrap();
        let again = self::run(&mut next, &snapshot, &prepared, &mut client, 2).await;
        assert_eq!((again.calls, again.left, again.stopped), (1, 0, None));
        assert_eq!(next.summary.issues.still_valid, 2);
    }

    #[tokio::test]
    async fn stops_on_a_rate_limit_and_leaves_the_rest_unchecked() {
        let (_tmp, repo) = repo(&[("src/pool.rs", &file(10, 3, "fn release_on_timeout() {}"))]);
        let snapshot = snapshot(
            &repo,
            vec![
                issue(1, "Leak", "`release_on_timeout`"),
                issue(2, "Leak again", "`release_on_timeout`"),
            ],
        );
        let mut report = report(&snapshot);
        let prepared = prepare(
            &mut report,
            &snapshot,
            &repo,
            &repo.blob_shas().unwrap(),
            None,
        );
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "86400"))
            .expect(1)
            .mount(&server)
            .await;
        let mut client = Client::new(&server.uri(), None, "m").unwrap();
        let run = run(&mut report, &snapshot, &prepared, &mut client, 200).await;
        assert_eq!((run.calls, run.left, run.failed.len()), (1, 2, 1));
        assert_eq!(
            run.stopped.as_deref(),
            Some("rate limited (retry after 86400s)")
        );
        assert!(report.items.iter().all(|i| i.tier == Tier::None));
    }
}
