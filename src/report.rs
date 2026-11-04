//! What a run says it did.
//!
//! Two audiences, one record. The human report goes to stderr — stdout carries
//! the written path and nothing else — while `--json` writes one object per
//! item to stdout with a stable schema, so a script never has to parse prose.
//! The schema version is part of every object and is documented in
//! `docs/cli.md` §6; changing a field's meaning means changing that number.

use std::path::PathBuf;

use serde_json::json;

use crate::progress::human_bytes;

pub const RESULT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Downloaded,
    /// `--dry-run`: the plan was printed and nothing was fetched.
    Planned,
    /// `--list-formats`: the streams were printed and nothing was fetched.
    Listed,
    Failed,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Downloaded => "downloaded",
            Status::Planned => "planned",
            Status::Listed => "listed",
            Status::Failed => "failed",
        }
    }
}

/// One URL's outcome.
#[derive(Debug, Clone)]
pub struct Item {
    pub url: String,
    pub site: String,
    pub id: String,
    pub title: String,
    /// The stream choice, as `Plan::describe` phrased it.
    pub selection: String,
    pub path: Option<PathBuf>,
    pub bytes: u64,
    pub duration_ms: Option<u64>,
    pub elapsed_ms: u64,
    pub status: Status,
    /// Stable machine-readable reason for a failure (`http`, `refused`, …).
    pub reason: Option<String>,
    pub message: String,
}

impl Item {
    pub fn new(url: &str) -> Item {
        Item {
            url: url.to_string(),
            site: String::new(),
            id: String::new(),
            title: String::new(),
            selection: String::new(),
            path: None,
            bytes: 0,
            duration_ms: None,
            elapsed_ms: 0,
            status: Status::Failed,
            reason: None,
            message: String::new(),
        }
    }

    pub fn succeeded(&self) -> bool {
        matches!(
            self.status,
            Status::Downloaded | Status::Planned | Status::Listed
        )
    }

    /// The JSON object for this item, already serialised. Written with `json!`
    /// rather than a derive so that the field order is explicit and stable.
    pub fn to_json(&self) -> String {
        let value = json!({
            "schema": RESULT_SCHEMA_VERSION,
            "status": self.status.as_str(),
            "url": self.url,
            "site": self.site,
            "id": self.id,
            "title": self.title,
            "selection": self.selection,
            "path": self.path.as_ref().map(|p| p.display().to_string()),
            "bytes": self.bytes,
            "duration_ms": self.duration_ms,
            "elapsed_ms": self.elapsed_ms,
            "reason": self.reason,
            "message": self.message,
        });
        value.to_string()
    }

    /// One line for a terminal, or for a log file. The path is not repeated
    /// here: it is already on stdout.
    pub fn to_line(&self) -> String {
        match &self.path {
            Some(path) => format!(
                "{} {} -> {} ({})",
                self.site,
                self.id,
                path.display(),
                human_bytes(self.bytes)
            ),
            None => format!("{} {}", self.site, self.id),
        }
    }
}

/// Counts for the closing summary.
#[derive(Debug, Default, Clone)]
pub struct Totals {
    pub attempted: usize,
    pub downloaded: usize,
    pub failed: usize,
    pub bytes: u64,
}

impl Totals {
    pub fn add(&mut self, item: &Item) {
        self.attempted += 1;
        match item.status {
            Status::Downloaded => {
                self.downloaded += 1;
                self.bytes += item.bytes;
            }
            Status::Failed => self.failed += 1,
            Status::Planned | Status::Listed => {}
        }
    }

    /// The summary line, as printed to stderr at the end of a run.
    ///
    /// A single successful download prints nothing extra: the path is on
    /// stdout, and repeating it is noise. The summary appears when there is
    /// something to add — more than one item, or a failure.
    pub fn summary(&self, elapsed: std::time::Duration, json: bool) -> Option<String> {
        if json {
            return None;
        }
        if self.attempted <= 1 && self.failed == 0 {
            return None;
        }
        let mut parts = Vec::new();
        parts.push(format!("{} downloaded", self.downloaded));
        if self.failed > 0 {
            parts.push(format!("{} failed", self.failed));
        }
        parts.push(human_bytes(self.bytes));
        parts.push(crate::progress::human_duration(elapsed));
        Some(format!("Summary: {}", parts.join(", ")))
    }
}

/// The exit code a run earned: `4` beats `1`, because a verification failure is
/// a bug and a network error is weather.
pub fn exit_code(items: &[Item]) -> u8 {
    let mut code = 0u8;
    for item in items {
        if item.status != Status::Failed {
            continue;
        }
        let item_code = match item.reason.as_deref() {
            Some("usage") => 2,
            Some("verify") => 4,
            Some("internal") => 3,
            _ => 1,
        };
        if item_code == 4 {
            return 4;
        }
        code = code.max(item_code);
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn item(status: Status, reason: Option<&str>) -> Item {
        let mut item = Item::new("https://example/v");
        item.site = "youtube".into();
        item.id = "abc".into();
        item.status = status;
        item.reason = reason.map(str::to_string);
        item
    }

    #[test]
    fn json_is_one_object_with_a_schema_version() {
        let mut item = item(Status::Downloaded, None);
        item.bytes = 1024;
        item.path = Some(PathBuf::from("/tmp/a.mp4"));
        let value: serde_json::Value = serde_json::from_str(&item.to_json()).unwrap();
        assert_eq!(value["schema"], RESULT_SCHEMA_VERSION);
        assert_eq!(value["status"], "downloaded");
        assert_eq!(value["path"], "/tmp/a.mp4");
        assert_eq!(value["bytes"], 1024);
        assert!(value["reason"].is_null());
    }

    #[test]
    fn exit_codes_prefer_verification_failure() {
        assert_eq!(exit_code(&[item(Status::Downloaded, None)]), 0);
        assert_eq!(exit_code(&[item(Status::Failed, Some("http"))]), 1);
        assert_eq!(exit_code(&[item(Status::Failed, Some("usage"))]), 2);
        assert_eq!(
            exit_code(&[
                item(Status::Failed, Some("http")),
                item(Status::Failed, Some("verify")),
            ]),
            4
        );
    }

    #[test]
    fn the_summary_is_quiet_for_one_success() {
        let mut totals = Totals::default();
        let mut one = item(Status::Downloaded, None);
        one.bytes = 100;
        totals.add(&one);
        assert!(totals.summary(Duration::from_secs(1), false).is_none());

        totals.add(&item(Status::Failed, Some("http")));
        let summary = totals.summary(Duration::from_secs(2), false).unwrap();
        assert!(summary.contains("1 downloaded"));
        assert!(summary.contains("1 failed"));
        assert!(totals.summary(Duration::from_secs(2), true).is_none());
    }
}
