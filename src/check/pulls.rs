//! PR activity rules: `abandoned` (author gone quiet, checks failing or conflicting) and
//! `ready_unreviewed` (green, not draft, nobody has reviewed it).

use crate::fetch::{Association, CheckState, Mergeable, Pull, PullActivity};
use crate::store::{Confidence, Evidence, EvidenceType, Verdict};
use chrono::{DateTime, Duration, Utc};

/// Inactivity at or past this many days makes `abandoned` high confidence.
const HIGH_CONFIDENCE_DAYS: i64 = 365;

/// Configurable cutoffs for the PR rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PullThresholds {
    /// Days without author activity before a failing or conflicting PR is `abandoned`.
    pub abandoned_after_days: u32,
    /// Days open without a review before a green PR is `ready_unreviewed`.
    pub unreviewed_after_days: u32,
}

impl Default for PullThresholds {
    fn default() -> Self {
        Self {
            abandoned_after_days: 180,
            unreviewed_after_days: 21,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub verdict: Verdict,
    pub confidence: Confidence,
    pub evidence: Vec<Evidence>,
}

/// Judge one open PR at time `now`. `None` means no rule matched (`cant_tell`).
pub fn check(
    pull: &Pull,
    activity: &PullActivity,
    now: DateTime<Utc>,
    thresholds: &PullThresholds,
) -> Option<Finding> {
    abandoned(pull, activity, now, thresholds)
        .or_else(|| unreviewed(pull, activity, now, thresholds))
}

fn abandoned(
    pull: &Pull,
    activity: &PullActivity,
    now: DateTime<Utc>,
    thresholds: &PullThresholds,
) -> Option<Finding> {
    let last = last_author_activity(pull, activity);
    let idle = now - last;
    if idle < Duration::days(thresholds.abandoned_after_days.into()) {
        return None;
    }
    let mut reasons = Vec::new();
    if matches!(
        activity.checks,
        Some(CheckState::Failure | CheckState::Error)
    ) {
        reasons.push("checks failing");
    }
    if activity.mergeable == Mergeable::Conflicting {
        reasons.push("merge conflicts");
    }
    if reasons.is_empty() {
        return None;
    }
    let confidence = if idle >= Duration::days(HIGH_CONFIDENCE_DAYS) {
        Confidence::High
    } else {
        Confidence::Medium
    };
    let note = format!(
        "Last author activity {} ({} ago); {}",
        last.format("%Y-%m-%d"),
        ago(idle),
        reasons.join(", ")
    );
    Some(finding(Verdict::Abandoned, confidence, pull, note))
}

fn unreviewed(
    pull: &Pull,
    activity: &PullActivity,
    now: DateTime<Utc>,
    thresholds: &PullThresholds,
) -> Option<Finding> {
    let open_for = now - pull.created_at;
    let ready = !activity.draft
        && activity.checks == Some(CheckState::Success)
        && activity.mergeable != Mergeable::Conflicting
        && open_for >= Duration::days(thresholds.unreviewed_after_days.into());
    if !ready || reviewed(activity) {
        return None;
    }
    let note = format!(
        "Checks passing; no review since opened {} ({} ago)",
        pull.created_at.format("%Y-%m-%d"),
        ago(open_for)
    );
    Some(finding(
        Verdict::ReadyUnreviewed,
        Confidence::Medium,
        pull,
        note,
    ))
}

fn finding(verdict: Verdict, confidence: Confidence, pull: &Pull, note: String) -> Finding {
    let sha = &pull.head.sha;
    Finding {
        verdict,
        confidence,
        evidence: vec![Evidence {
            kind: EvidenceType::Commit,
            reference: sha.get(..7).unwrap_or(sha).to_string(),
            note,
        }],
    }
}

/// Latest of: PR opened, head commit pushed, author's own comments and reviews.
fn last_author_activity(pull: &Pull, activity: &PullActivity) -> DateTime<Utc> {
    let own_posts = activity
        .comments
        .iter()
        .chain(&activity.reviews)
        .filter(|p| is_author(activity, &p.author))
        .map(|p| p.at);
    own_posts
        .chain(activity.head_committed_at)
        .fold(pull.created_at, DateTime::max)
}

/// Someone other than the author submitted a review, or a maintainer commented.
fn reviewed(activity: &PullActivity) -> bool {
    activity
        .reviews
        .iter()
        .any(|r| !is_author(activity, &r.author))
        || activity.comments.iter().any(|c| {
            !is_author(activity, &c.author)
                && matches!(
                    c.association,
                    Association::Owner | Association::Member | Association::Collaborator
                )
        })
}

/// A post is the PR author's only when both logins are known and match.
fn is_author(activity: &PullActivity, poster: &Option<String>) -> bool {
    activity.author.is_some() && *poster == activity.author
}

/// "9 months" from 60 days up, "3 weeks" from 7 days up, otherwise days.
fn ago(d: Duration) -> String {
    let days = d.num_days();
    let (n, unit) = match days {
        60.. => (days / 30, "month"),
        7.. => (days / 7, "week"),
        _ => (days, "day"),
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::{Post, PullHead};

    fn ts(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    const NOW: &str = "2026-09-27T00:00:00Z";

    fn days_before_now(days: i64) -> DateTime<Utc> {
        ts(NOW) - Duration::days(days)
    }

    fn pull(opened_days_ago: i64) -> Pull {
        Pull {
            number: 7,
            title: "PR".into(),
            html_url: "https://github.com/o/r/pull/7".into(),
            created_at: days_before_now(opened_days_ago),
            head: PullHead {
                sha: "c0ffee1234567".into(),
            },
        }
    }

    /// Green, mergeable, not draft, no reviews; last pushed when opened.
    fn activity(pushed_days_ago: i64) -> PullActivity {
        PullActivity {
            draft: false,
            author: Some("alice".into()),
            mergeable: Mergeable::Mergeable,
            head_committed_at: Some(days_before_now(pushed_days_ago)),
            checks: Some(CheckState::Success),
            reviews: vec![],
            comments: vec![],
        }
    }

    fn post(author: &str, association: Association, days_ago: i64) -> Post {
        Post {
            author: Some(author.into()),
            association,
            at: days_before_now(days_ago),
        }
    }

    fn run(pull: &Pull, activity: &PullActivity) -> Option<Finding> {
        check(pull, activity, ts(NOW), &PullThresholds::default())
    }

    fn verdict(pull: &Pull, activity: &PullActivity) -> Option<Verdict> {
        run(pull, activity).map(|f| f.verdict)
    }

    /// Idle for `days`, checks failing.
    fn failing(days: i64) -> (Pull, PullActivity) {
        let mut a = activity(days);
        a.checks = Some(CheckState::Failure);
        (pull(days), a)
    }

    #[test]
    fn abandoned_starts_at_threshold() {
        let (p, a) = failing(179);
        assert_eq!(verdict(&p, &a), None);
        let (p, a) = failing(180);
        let f = run(&p, &a).unwrap();
        assert_eq!(f.verdict, Verdict::Abandoned);
        assert_eq!(f.confidence, Confidence::Medium);
        assert_eq!(
            f.evidence,
            [Evidence {
                kind: EvidenceType::Commit,
                reference: "c0ffee1".into(),
                note: "Last author activity 2026-03-31 (6 months ago); checks failing".into(),
            }]
        );
    }

    #[test]
    fn abandoned_is_high_confidence_after_a_year() {
        let (p, a) = failing(364);
        assert_eq!(run(&p, &a).unwrap().confidence, Confidence::Medium);
        let (p, a) = failing(365);
        assert_eq!(run(&p, &a).unwrap().confidence, Confidence::High);
    }

    #[test]
    fn abandoned_needs_failing_checks_or_conflicts() {
        let (p, mut a) = failing(400);
        for checks in [
            Some(CheckState::Success),
            Some(CheckState::Pending),
            Some(CheckState::Expected),
            None,
        ] {
            a.checks = checks;
            // Old and green is ready_unreviewed, never abandoned.
            assert_ne!(verdict(&p, &a), Some(Verdict::Abandoned), "{checks:?}");
        }
        a.checks = Some(CheckState::Error);
        assert_eq!(verdict(&p, &a), Some(Verdict::Abandoned));

        a.checks = Some(CheckState::Pending);
        a.mergeable = Mergeable::Conflicting;
        let f = run(&p, &a).unwrap();
        assert_eq!(f.verdict, Verdict::Abandoned);
        assert!(f.evidence[0]
            .note
            .ends_with("(13 months ago); merge conflicts"));

        a.checks = Some(CheckState::Failure);
        assert!(run(&p, &a).unwrap().evidence[0]
            .note
            .ends_with("; checks failing, merge conflicts"));
    }

    #[test]
    fn abandoned_applies_to_drafts() {
        let (p, mut a) = failing(200);
        a.draft = true;
        assert_eq!(verdict(&p, &a), Some(Verdict::Abandoned));
    }

    #[test]
    fn author_comments_reviews_and_pushes_count_as_activity() {
        let (p, a) = failing(300);

        let mut pushed = a.clone();
        pushed.head_committed_at = Some(days_before_now(179));
        assert_eq!(verdict(&p, &pushed), None);

        let mut commented = a.clone();
        commented
            .comments
            .push(post("alice", Association::Other, 179));
        assert_eq!(verdict(&p, &commented), None);

        let mut replied = a.clone();
        replied.reviews.push(post("alice", Association::Other, 179));
        assert_eq!(verdict(&p, &replied), None);

        // Other people's recent posts don't keep the PR alive.
        let mut others = a.clone();
        others.comments.push(post("bob", Association::Member, 1));
        let f = run(&p, &others).unwrap();
        assert_eq!(f.verdict, Verdict::Abandoned);
        assert!(f.evidence[0].note.contains("2025-12-01 (10 months ago)"));
    }

    #[test]
    fn deleted_author_uses_push_and_open_dates() {
        let (p, mut a) = failing(200);
        a.author = None;
        a.comments.push(Post {
            author: None,
            association: Association::Other,
            at: days_before_now(1),
        });
        assert_eq!(verdict(&p, &a), Some(Verdict::Abandoned));
    }

    #[test]
    fn ready_unreviewed_starts_at_threshold() {
        assert_eq!(verdict(&pull(20), &activity(20)), None);
        let f = run(&pull(21), &activity(21)).unwrap();
        assert_eq!(f.verdict, Verdict::ReadyUnreviewed);
        assert_eq!(f.confidence, Confidence::Medium);
        assert_eq!(
            f.evidence[0].note,
            "Checks passing; no review since opened 2026-09-06 (3 weeks ago)"
        );
        // Measured from when the PR was opened, not the last push.
        assert_eq!(
            verdict(&pull(21), &activity(1)),
            Some(Verdict::ReadyUnreviewed)
        );
    }

    #[test]
    fn ready_unreviewed_needs_green_ready_mergeable() {
        let p = pull(30);

        let mut draft = activity(30);
        draft.draft = true;
        assert_eq!(verdict(&p, &draft), None);

        for checks in [
            Some(CheckState::Pending),
            Some(CheckState::Expected),
            Some(CheckState::Failure),
            None,
        ] {
            let mut a = activity(30);
            a.checks = checks;
            assert_eq!(verdict(&p, &a), None, "{checks:?}");
        }

        let mut conflicting = activity(30);
        conflicting.mergeable = Mergeable::Conflicting;
        assert_eq!(verdict(&p, &conflicting), None);

        let mut unknown = activity(30);
        unknown.mergeable = Mergeable::Unknown;
        assert_eq!(verdict(&p, &unknown), Some(Verdict::ReadyUnreviewed));
    }

    #[test]
    fn reviews_and_maintainer_comments_count_as_review() {
        let p = pull(30);

        let mut review = activity(30);
        review.reviews.push(post("bob", Association::Other, 10));
        assert_eq!(verdict(&p, &review), None);

        for association in [
            Association::Owner,
            Association::Member,
            Association::Collaborator,
        ] {
            let mut a = activity(30);
            a.comments.push(post("bob", association, 10));
            assert_eq!(verdict(&p, &a), None, "{association:?}");
        }

        // The author's own replies and drive-by comments are not reviews.
        let mut a = activity(30);
        a.reviews.push(post("alice", Association::Owner, 10));
        a.comments.push(post("alice", Association::Owner, 10));
        a.comments.push(post("carol", Association::Other, 10));
        assert_eq!(verdict(&p, &a), Some(Verdict::ReadyUnreviewed));
    }

    #[test]
    fn thresholds_are_configurable() {
        let t = PullThresholds {
            abandoned_after_days: 30,
            unreviewed_after_days: 7,
        };
        let (p, a) = failing(30);
        assert_eq!(
            check(&p, &a, ts(NOW), &t).map(|f| f.verdict),
            Some(Verdict::Abandoned)
        );
        let f = check(&pull(7), &activity(7), ts(NOW), &t).unwrap();
        assert_eq!(f.verdict, Verdict::ReadyUnreviewed);
        assert!(f.evidence[0].note.ends_with("(1 week ago)"));
    }

    #[test]
    fn ago_picks_a_readable_unit() {
        let d = Duration::days;
        assert_eq!(ago(d(1)), "1 day");
        assert_eq!(ago(d(6)), "6 days");
        assert_eq!(ago(d(14)), "2 weeks");
        assert_eq!(ago(d(59)), "8 weeks");
        assert_eq!(ago(d(60)), "2 months");
        assert_eq!(ago(d(275)), "9 months");
    }
}
