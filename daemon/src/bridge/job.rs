//! Typed bridge job parsing.
//!
//! The bridge used to read each job's fields by hand with its own defaults,
//! duplicating the MCP parameter structs. This reuses those same structs
//! behind one `#[serde(tag = "op")]` enum, so a job with a bad field (a
//! typo'd `return` mode, a non-numeric `depth`) fails to parse the same way
//! an MCP tool call would, instead of silently taking a wrong default.

use crate::mcp::{ExecuteParams, FileTargetParams, ScreenshotParams, SelectionParams};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(crate) enum Job {
    Status(FileTargetParams),
    Execute(ExecuteParams),
    GetSelection(SelectionParams),
    Screenshot(ScreenshotParams),
}

impl Job {
    /// Parse a job from the raw JSON value read off disk.
    pub(crate) fn parse(raw: &serde_json::Value) -> Result<Job, String> {
        serde_json::from_value(raw.clone()).map_err(|e| format!("invalid job: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_execute_job() {
        let job = Job::parse(&json!({"op": "execute", "code": "return 1;"})).expect("parse");
        assert!(matches!(job, Job::Execute(_)));
    }

    #[test]
    fn parses_status_job_with_no_file_key() {
        let job = Job::parse(&json!({"op": "status"})).expect("parse");
        assert!(matches!(job, Job::Status(_)));
    }

    #[test]
    fn parses_get_selection_job() {
        let job = Job::parse(&json!({"op": "get_selection", "depth": 2})).expect("parse");
        assert!(matches!(job, Job::GetSelection(_)));
    }

    #[test]
    fn parses_screenshot_job_with_defaults() {
        let job = Job::parse(&json!({"op": "screenshot"})).expect("parse");
        match job {
            Job::Screenshot(p) => {
                assert_eq!(p.scale, 1.0);
                assert_eq!(p.max_dim, 1200);
                assert!(!p.full_res);
            }
            _ => panic!("expected Screenshot"),
        }
    }

    #[test]
    fn rejects_an_unknown_op() {
        assert!(Job::parse(&json!({"op": "delete_everything"})).is_err());
    }

    #[test]
    fn rejects_a_typo_in_return_mode() {
        // A typo'd return mode must fail to parse, not silently fall back
        // to file mode.
        let err = Job::parse(&json!({"op": "screenshot", "return": "inlin"}))
            .expect_err("typo must fail to parse");
        assert!(err.contains("invalid job"));
    }

    #[test]
    fn rejects_execute_without_code() {
        assert!(Job::parse(&json!({"op": "execute"})).is_err());
    }
}
