//! Reporter: static outputs rendered from a report (see docs/DESIGN.md §6).

pub mod badge;
pub mod html;

use crate::store::Report;
use std::path::Path;

/// Write the dashboard (`index.html`) and the badge (`badge.json`) into `dir`, creating it if needed.
pub fn write_site(report: &Report, dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("index.html"), html::render(report))?;
    std::fs::write(dir.join("badge.json"), badge::render(report))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_site_creates_the_dir() {
        let report: Report =
            serde_json::from_str(include_str!("../../schema/report.example.json")).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("site/stillvalid");
        write_site(&report, &dir).unwrap();
        assert!(std::fs::read_to_string(dir.join("index.html"))
            .unwrap()
            .starts_with("<!doctype html>"));
        assert!(dir.join("badge.json").is_file());
    }
}
