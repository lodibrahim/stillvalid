//! Badge: shields.io endpoint JSON (https://shields.io/badges/endpoint-badge).

use crate::store::Report;
use serde::Serialize;

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Badge {
    pub schema_version: u32,
    pub label: String,
    pub message: String,
    pub color: String,
}

/// "likely fixed: 12 of 125 issues", in informational blue.
pub fn badge(report: &Report) -> Badge {
    let issues = &report.summary.issues;
    let (message, color) = match issues.open {
        0 => ("0 open issues".to_string(), "lightgrey"),
        open => (format!("{} of {open} issues", issues.likely_fixed), "blue"),
    };
    Badge {
        schema_version: 1,
        label: "likely fixed".into(),
        message,
        color: color.into(),
    }
}

pub fn render(report: &Report) -> String {
    let mut json = serde_json::to_string_pretty(&badge(report)).expect("plain struct");
    json.push('\n');
    json
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn example() -> Report {
        serde_json::from_str(include_str!("../../schema/report.example.json")).unwrap()
    }

    #[test]
    fn endpoint_json_shape() {
        let v: Value = serde_json::from_str(&render(&example())).unwrap();
        assert_eq!(
            v,
            json!({
                "schemaVersion": 1,
                "label": "likely fixed",
                "message": "612 of 3412 issues",
                "color": "blue"
            })
        );
    }

    #[test]
    fn no_open_issues_is_grey() {
        let mut report = example();
        report.summary.issues = Default::default();
        let b = badge(&report);
        assert_eq!(b.message, "0 open issues");
        assert_eq!(b.color, "lightgrey");
    }
}
