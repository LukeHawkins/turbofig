//! Context-firewall budgets: warn the caller when a read or a screenshot is
//! large enough that it should have been shaped (fields/depth) or written to
//! a file instead of inlined.

use serde_json::Value;

/// Serialized-byte limit for execute and selection results.
/// Results larger than this budget should use fields/depth shaping or file mode.
pub(crate) const READ_BUDGET_BYTES: usize = 20_000;

/// Base64-byte limit for inline screenshots.
/// Screenshots larger than this budget should use file mode and a subagent reader.
pub(crate) const INLINE_SCREENSHOT_BUDGET_BYTES: usize = 100_000;

/// Return a warning string when `len` exceeds the read budget.
/// Returns None when the result is within budget.
pub(crate) fn read_budget_warning(len: usize) -> Option<String> {
    if len > READ_BUDGET_BYTES {
        Some(format!(
            "result is {len} bytes, over the {READ_BUDGET_BYTES} byte budget; \
             shape the read with fields/depth or write to file"
        ))
    } else {
        None
    }
}

/// Return a warning string when `len` exceeds the inline screenshot budget.
/// Returns None when the screenshot is within budget.
pub(crate) fn inline_screenshot_warning(len: usize) -> Option<String> {
    if len > INLINE_SCREENSHOT_BUDGET_BYTES {
        Some(format!(
            "inline screenshot is {len} base64 bytes, over the {INLINE_SCREENSHOT_BUDGET_BYTES} \
             byte budget; use return:'file' and read it in a subagent"
        ))
    } else {
        None
    }
}

/// Insert a `"warning"` key into `value` when `warning` is Some and `value` is a JSON object.
/// Returns `value` unchanged when `warning` is None or `value` is not an object.
pub(crate) fn with_warning(mut value: Value, warning: Option<String>) -> Value {
    if let (Some(w), Some(obj)) = (warning, value.as_object_mut()) {
        obj.insert("warning".to_owned(), Value::String(w));
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn read_budget_warning_returns_none_just_under_budget() {
        assert!(read_budget_warning(READ_BUDGET_BYTES - 1).is_none());
    }

    #[test]
    fn read_budget_warning_returns_none_at_budget() {
        assert!(read_budget_warning(READ_BUDGET_BYTES).is_none());
    }

    #[test]
    fn read_budget_warning_returns_some_just_over_budget() {
        let w = read_budget_warning(READ_BUDGET_BYTES + 1).expect("must warn past budget");
        let count = (READ_BUDGET_BYTES + 1).to_string();
        assert!(
            w.contains(&count),
            "message must contain the byte count: {w}"
        );
        assert!(
            w.contains("fields"),
            "message must name the fields remedy: {w}"
        );
        assert!(w.contains("file"), "message must name the file remedy: {w}");
    }

    #[test]
    fn inline_screenshot_warning_returns_none_under_budget() {
        assert!(inline_screenshot_warning(INLINE_SCREENSHOT_BUDGET_BYTES - 1).is_none());
    }

    #[test]
    fn inline_screenshot_warning_returns_none_at_budget() {
        assert!(inline_screenshot_warning(INLINE_SCREENSHOT_BUDGET_BYTES).is_none());
    }

    #[test]
    fn inline_screenshot_warning_returns_some_over_budget() {
        let w = inline_screenshot_warning(INLINE_SCREENSHOT_BUDGET_BYTES + 1)
            .expect("must warn past budget");
        let count = (INLINE_SCREENSHOT_BUDGET_BYTES + 1).to_string();
        assert!(
            w.contains(&count),
            "message must contain the byte count: {w}"
        );
        assert!(w.contains("file"), "message must name file mode: {w}");
        assert!(w.contains("subagent"), "message must name subagent: {w}");
    }

    #[test]
    fn with_warning_some_inserts_warning_key() {
        let val = json!({"ok": true});
        let result = with_warning(val, Some("too big".to_owned()));
        assert_eq!(result["warning"], json!("too big"));
        assert_eq!(result["ok"], json!(true), "original keys must be preserved");
    }

    #[test]
    fn with_warning_none_leaves_object_unchanged() {
        let val = json!({"ok": true, "result": 42});
        let result = with_warning(val.clone(), None);
        assert_eq!(result, val, "None warning must not modify the object");
        assert!(
            result.get("warning").is_none(),
            "no warning key must be present"
        );
    }

    #[test]
    fn with_warning_non_object_is_returned_unchanged() {
        let val = json!([1, 2, 3]);
        let result = with_warning(val.clone(), Some("ignored".to_owned()));
        assert_eq!(result, val, "non-object value must be returned unchanged");
    }
}
