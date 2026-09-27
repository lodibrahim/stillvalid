//! Dashboard: one static page per report, from the embedded `dashboard.html` template.
//!
//! Everything is rendered here, with all report text HTML-escaped (titles are untrusted input).
//! The template's inline script only switches tabs and filters rows, so the page works from
//! `file://`, on GitHub Pages, and with JavaScript off (both lists shown, unfiltered).

use crate::check::pulls::{ago, short_sha};
use crate::store::{Confidence, Evidence, EvidenceType, Item, Kind, Report, Tier, Verdict};
use std::fmt::Write;

const TEMPLATE: &str = include_str!("dashboard.html");
pub(crate) const GITHUB: &str = "https://github.com/";

/// Display and sort order per tab: most actionable first, `cant_tell` last.
pub(crate) const ISSUE_VERDICTS: &[Verdict] = &[
    Verdict::LikelyFixed,
    Verdict::StillValid,
    Verdict::NeedsInfo,
    Verdict::Duplicate,
    Verdict::CantTell,
];
pub(crate) const PULL_VERDICTS: &[Verdict] = &[
    Verdict::Superseded,
    Verdict::Abandoned,
    Verdict::Conflicts,
    Verdict::ReadyUnreviewed,
    Verdict::StillApplies,
    Verdict::CantTell,
];

pub fn render(report: &Report) -> String {
    let base = format!("{GITHUB}{}", encode_path(&report.repo));
    let repo = escape(&report.repo);
    let head = match is_hex(&report.head_sha) {
        true => format!(
            r#"<a href="{base}/commit/{sha}"><code>{short}</code></a>"#,
            base = escape(&base),
            sha = report.head_sha,
            short = short_sha(&report.head_sha),
        ),
        false => format!("<code>{}</code>", escape(&report.head_sha)),
    };
    let header = format!(
        r#"<h1>stillvalid <a href="{href}">{repo}</a></h1>
<p class="meta"><code>{branch}</code> at {head} · {mode} mode · scanned <time datetime="{at}">{at_text}</time> · stillvalid {version}</p>"#,
        href = escape(&base),
        branch = escape(&report.branch),
        mode = escape(&report.mode),
        at = report.scanned_at.to_rfc3339(),
        at_text = report.scanned_at.format("%Y-%m-%d %H:%M UTC"),
        version = escape(&report.tool.version),
    );

    let issues = &report.summary.issues;
    let pulls = &report.summary.pulls;
    let tiles = [
        ("Open issues", issues.open, ""),
        ("Likely fixed", issues.likely_fixed, "likely_fixed"),
        ("Open PRs", pulls.open, ""),
        (
            "Superseded or abandoned",
            pulls.superseded + pulls.abandoned,
            "superseded",
        ),
    ]
    .iter()
    .map(|(label, n, verdict)| {
        let accent = match *verdict {
            "" => String::new(),
            v => format!(" v-{v}"),
        };
        format!(r#"<div class="tile{accent}"><span class="n">{n}</span> {label}</div>"#)
    })
    .collect::<Vec<_>>()
    .join("\n");

    let tabs = [(Kind::Issue, issues.open), (Kind::Pull, pulls.open)]
        .iter()
        .map(|(kind, n)| {
            let id = panel_id(*kind);
            format!(
                r#"<button type="button" role="tab" id="tab-{id}" aria-controls="{id}" aria-selected="false">{name} <span class="n">{n}</span></button>"#,
                name = plural(*kind),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let panels = format!(
        "{}\n{}",
        panel(report, &base, Kind::Issue),
        panel(report, &base, Kind::Pull)
    );

    fill(
        TEMPLATE,
        &[
            ("title", &format!("stillvalid: {repo}")),
            ("header", &header),
            ("tiles", &tiles),
            ("tabs", &tabs),
            ("panels", &panels),
        ],
    )
}

/// One tab: verdict bar, filter buttons (which double as the bar's legend), and the table.
fn panel(report: &Report, base: &str, kind: Kind) -> String {
    let order = match kind {
        Kind::Issue => ISSUE_VERDICTS,
        Kind::Pull => PULL_VERDICTS,
    };
    let rank = |v: Verdict| order.iter().position(|o| *o == v).unwrap_or(order.len());
    let mut items: Vec<&Item> = report.items.iter().filter(|i| i.kind == kind).collect();
    items.sort_by_key(|i| (rank(i.verdict), std::cmp::Reverse(i.created_at)));

    let id = panel_id(kind);
    let name = plural(kind);
    let lower = name.to_lowercase();
    let mut out = format!(
        r#"<section class="panel" id="{id}" role="tabpanel" aria-labelledby="tab-{id}">
<h2>{name}</h2>
"#
    );
    if items.is_empty() {
        let _ = writeln!(out, r#"<p class="empty">No open {lower}.</p>"#);
        out.push_str("</section>");
        return out;
    }

    // Verdicts with at least one item, in display order; any verdict outside the list goes last.
    let mut counts: Vec<(Verdict, usize)> = Vec::new();
    for item in &items {
        match counts.iter_mut().find(|(v, _)| *v == item.verdict) {
            Some((_, n)) => *n += 1,
            None => counts.push((item.verdict, 1)),
        }
    }
    let total = items.len();
    let summary = counts
        .iter()
        .map(|(v, n)| format!("{n} {}", label(*v).to_lowercase()))
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(
        out,
        r#"<div class="bar" role="img" aria-label="{name} by verdict: {summary}">"#
    );
    for (v, n) in &counts {
        let _ = writeln!(
            out,
            r#"<span class="v-{key}" style="flex-grow:{n}" title="{label}: {n}"></span>"#,
            key = key(*v),
            label = label(*v),
        );
    }
    out.push_str("</div>\n");

    let _ = writeln!(
        out,
        r#"<div class="filters" role="group" aria-label="Show {lower} by verdict">"#
    );
    for (v, n) in &counts {
        let _ = writeln!(
            out,
            r#"<button type="button" data-verdict="{key}" aria-pressed="true"><span class="dot v-{key}"></span>{label} <span class="n">{n}</span></button>"#,
            key = key(*v),
            label = label(*v),
        );
    }
    let _ = write!(
        out,
        r#"<button type="button" class="reset">Show all</button>
</div>
<p class="shown" aria-live="polite">{total} of {total} shown</p>
<table>
<caption class="sr-only">Open {lower}</caption>
<thead><tr><th scope="col">{singular}</th><th scope="col">Verdict</th><th scope="col">Evidence</th><th scope="col">Opened</th></tr></thead>
<tbody>
"#,
        singular = match kind {
            Kind::Issue => "Issue",
            Kind::Pull => "Pull request",
        },
    );
    for item in items {
        out.push_str(&row(report, base, item));
    }
    out.push_str("</tbody>\n</table>\n</section>");
    out
}

fn row(report: &Report, base: &str, item: &Item) -> String {
    let title = escape(&item.title);
    let title = match item.url.starts_with(GITHUB) {
        true => format!(r#"<a href="{}">{title}</a>"#, escape(&item.url)),
        false => title,
    };
    let confidence = confidence_name(item.confidence);
    // No check gave an unchecked item its confidence, so it has no line under the verdict.
    let sub = match item.tier {
        Tier::None => String::new(),
        Tier::Heuristic => {
            format!(r#"<span class="sub">{confidence} confidence · heuristic</span>"#)
        }
        Tier::Llm => format!(r#"<span class="sub">{confidence} confidence · AI</span>"#),
    };
    let evidence = match item.evidence.is_empty() {
        true => r#"<span class="muted">No evidence</span>"#.to_string(),
        false => {
            let list: String = item
                .evidence
                .iter()
                .map(|e| evidence(report, base, e))
                .collect();
            format!(r#"<ul class="evidence">{list}</ul>"#)
        }
    };
    let age = report.scanned_at - item.created_at;
    let age = match age.num_days() {
        ..=0 => "today".to_string(),
        730.. => format!("{} years ago", age.num_days() / 365),
        365.. => "1 year ago".to_string(),
        _ => format!("{} ago", ago(age)),
    };
    format!(
        r#"<tr data-verdict="{key}">
<td class="item">{title} <span class="num">#{number}</span></td>
<td><span class="verdict"><span class="dot v-{key}"></span>{label}</span>{sub}</td>
<td>{evidence}</td>
<td><time datetime="{at}" title="{date}">{age}</time></td>
</tr>
"#,
        key = key(item.verdict),
        label = label(item.verdict),
        number = item.number,
        at = item.created_at.to_rfc3339(),
        date = item.created_at.format("%Y-%m-%d"),
    )
}

fn evidence(report: &Report, base: &str, e: &Evidence) -> String {
    let kind = match e.kind {
        EvidenceType::Pull => "PR",
        EvidenceType::Commit => "commit",
        EvidenceType::Code => "code",
    };
    let reference = escape(&e.reference);
    let reference = match evidence_href(report, base, e) {
        Some(href) => format!(
            r#"<a href="{}"><code>{reference}</code></a>"#,
            escape(&href)
        ),
        None => format!("<code>{reference}</code>"),
    };
    format!(
        r#"<li><span class="etype">{kind}</span> {reference} <span class="note">{}</span></li>"#,
        escape(&e.note)
    )
}

/// GitHub link for an evidence ref: `#N` → PR, hex → commit, `path[:line]` → file at `head_sha`.
pub(crate) fn evidence_href(report: &Report, base: &str, e: &Evidence) -> Option<String> {
    let r = e.reference.as_str();
    match e.kind {
        EvidenceType::Pull => r
            .strip_prefix('#')
            .filter(|n| is_digits(n))
            .map(|n| format!("{base}/pull/{n}")),
        EvidenceType::Commit => is_hex(r).then(|| format!("{base}/commit/{r}")),
        EvidenceType::Code => {
            if !is_hex(&report.head_sha) || r.is_empty() {
                return None;
            }
            let (path, anchor) = match r.rsplit_once(':') {
                Some((path, line)) if is_digits(line) && !path.is_empty() => {
                    (path, format!("#L{line}"))
                }
                _ => (r, String::new()),
            };
            Some(format!(
                "{base}/blob/{sha}/{path}{anchor}",
                sha = report.head_sha,
                path = encode_path(path),
            ))
        }
    }
}

fn panel_id(kind: Kind) -> &'static str {
    match kind {
        Kind::Issue => "issues",
        Kind::Pull => "pulls",
    }
}

fn plural(kind: Kind) -> &'static str {
    match kind {
        Kind::Issue => "Issues",
        Kind::Pull => "Pull requests",
    }
}

/// The verdict as it appears in report.json; also the CSS class and filter key.
pub(crate) fn key(v: Verdict) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s,
        _ => unreachable!("Verdict serializes as a string"),
    }
}

pub(crate) fn confidence_name(c: Confidence) -> &'static str {
    match c {
        Confidence::High => "high",
        Confidence::Medium => "medium",
        Confidence::Low => "low",
    }
}

pub(crate) fn label(v: Verdict) -> &'static str {
    match v {
        Verdict::LikelyFixed => "Likely fixed",
        Verdict::StillValid => "Still valid",
        Verdict::Duplicate => "Duplicate",
        Verdict::NeedsInfo => "Needs info",
        Verdict::StillApplies => "Still applies",
        Verdict::Superseded => "Superseded",
        Verdict::Conflicts => "Conflicts",
        Verdict::Abandoned => "Abandoned",
        Verdict::ReadyUnreviewed => "Ready, unreviewed",
        Verdict::CantTell => "Can't tell",
    }
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Percent-encode a URL path, keeping `/` and unreserved characters.
pub(crate) fn encode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char)
            }
            b => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Replace each `{{name}}` in one pass, so filled-in text is never scanned for placeholders.
fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let found = after.find("}}").and_then(|end| {
            let value = values.iter().find(|(k, _)| *k == &after[..end])?.1;
            Some((value, end))
        });
        match found {
            Some((value, end)) => {
                out.push_str(value);
                rest = &after[end + 2..];
            }
            None => {
                out.push_str("{{");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example() -> Report {
        serde_json::from_str(include_str!("../../schema/report.example.json")).unwrap()
    }

    #[test]
    fn renders_the_schema_example() {
        let html = render(&example());
        assert!(!html.contains("{{"), "unfilled placeholder");
        assert!(html.contains(r#"<a href="https://github.com/acme/rocketdb">acme/rocketdb</a>"#));
        assert!(html.contains(
            r#"<a href="https://github.com/acme/rocketdb/issues/1204">Panic when compacting an empty SSTable</a>"#
        ));
        assert!(html.contains(r#"<a href="https://github.com/acme/rocketdb/pull/2890">"#));
        assert!(html.contains(
            r#"<a href="https://github.com/acme/rocketdb/pull/2977"><code>#2977</code></a>"#
        ));
        assert!(html.contains(
            r#"<a href="https://github.com/acme/rocketdb/blob/9f1c2ab7e0d4c3b8a1f2e3d4c5b6a7f8e9d0c1b2/src/compaction.rs#L88"><code>src/compaction.rs:88</code></a>"#
        ));
        assert!(html.contains(
            r#"<a href="https://github.com/acme/rocketdb/commit/91b0f3e"><code>91b0f3e</code></a>"#
        ));
        // Tiles come from the summary; tabs, bar and filters from the items.
        assert!(html.contains(r#"<span class="n">3412</span> Open issues"#));
        assert!(html.contains(r#"<span class="n">93</span> Superseded or abandoned"#));
        assert!(html.contains(r#"<tr data-verdict="likely_fixed">"#));
        assert!(html.contains(r#"<tr data-verdict="superseded">"#));
        assert!(html.contains("high confidence · heuristic"));
        assert!(html.contains("2 years ago"));
    }

    #[test]
    fn untrusted_text_is_escaped() {
        let mut report = example();
        let evil = r#"<script>alert(1)</script><img src=x onerror="alert(2)">'&"#;
        report.repo = evil.into();
        report.branch = evil.into();
        let item = &mut report.items[0];
        item.title = evil.into();
        item.url = r#"https://github.com/a/b/issues/1"><script>alert(3)</script>"#.into();
        item.evidence[0].note = evil.into();
        item.evidence[1].reference = r#"x" onmouseover="alert(4):1"#.into();
        report.items[1].url = "javascript:alert(5)".into();

        let html = render(&report);
        assert!(!html.contains("<script>alert"));
        assert!(!html.contains("<img"));
        assert!(!html.contains(r#"" onmouseover"#));
        assert!(!html.contains("javascript:"));
        assert!(html.contains(
            "&lt;script&gt;alert(1)&lt;/script&gt;&lt;img src=x onerror=&quot;alert(2)&quot;&gt;&#39;&amp;"
        ));
        // The only <script> is the template's own.
        assert_eq!(html.matches("<script").count(), 1);
    }

    #[test]
    fn evidence_links() {
        let report = example();
        let base = "https://github.com/acme/rocketdb";
        let href = |kind, reference: &str| {
            let e = Evidence {
                kind,
                reference: reference.into(),
                note: String::new(),
            };
            evidence_href(&report, base, &e)
        };
        let sha = &report.head_sha;
        assert_eq!(
            href(EvidenceType::Pull, "#12").unwrap(),
            format!("{base}/pull/12")
        );
        assert_eq!(href(EvidenceType::Pull, "other/repo#12"), None);
        assert_eq!(
            href(EvidenceType::Commit, "0123abc").unwrap(),
            format!("{base}/commit/0123abc")
        );
        assert_eq!(href(EvidenceType::Commit, "HEAD~1"), None);
        assert_eq!(
            href(EvidenceType::Code, "crates/ignore/src/walk.rs").unwrap(),
            format!("{base}/blob/{sha}/crates/ignore/src/walk.rs")
        );
        assert_eq!(
            href(EvidenceType::Code, "docs/a b#c.md:7").unwrap(),
            format!("{base}/blob/{sha}/docs/a%20b%23c.md#L7")
        );
    }

    #[test]
    fn empty_tab_says_so() {
        let mut report = example();
        report.items.retain(|i| i.kind == Kind::Issue);
        let html = render(&report);
        assert!(html.contains(r#"<p class="empty">No open pull requests.</p>"#));
    }

    #[test]
    fn rows_sort_by_verdict_then_newest() {
        let mut report = example();
        let mut unchecked = report.items[0].clone();
        unchecked.number = 7;
        unchecked.verdict = Verdict::CantTell;
        unchecked.created_at = "2026-09-01T00:00:00Z".parse().unwrap();
        report.items.insert(0, unchecked);
        let html = render(&report);
        assert!(html.find("#1204</span>").unwrap() < html.find("#7</span>").unwrap());
    }

    #[test]
    fn fill_is_single_pass() {
        let out = fill("{{a}} {{b}} {{c", &[("a", "{{b}}"), ("b", "x")]);
        assert_eq!(out, "{{b}} x {{c");
    }
}
