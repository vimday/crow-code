//! Centralized IntentPlan schema definition for LLM prompts.
//!
//! This module is the **single source of truth** for the JSON schema
//! that the LLM sees. When `IntentPlan` or `EditOp` evolve, this is
//! the one place that needs updating — not scattered raw strings.

/// Returns the IntentPlan JSON schema guide for LLM prompts.
///
/// This must be kept in sync with `crow_patch::IntentPlan` and its
/// constituent types. The schema is intentionally simplified for the
/// model — the hydrator will fill in system-owned fields afterwards.
pub fn intent_plan_schema() -> &'static str {
    r#"Expected JSON Schema for IntentPlan:
{
  "base_snapshot_id": "string (any identifier)",
  "rationale": "string (explain why you are making these changes)",
  "is_partial": boolean,
  "confidence": "High" | "Medium" | "Low" | "None",
  "operations": [
    {
      "Create": {
        "path": "relative/path.ext",
        "content": "full file content as string",
        "precondition": "MustNotExist"
      }
    },
    {
      "Modify": {
        "path": "relative/path.ext",
        "preconditions": {
          "content_hash": "any-placeholder (system will replace)",
          "expected_line_count": null
        },
        "hunks": [
          {
            "original_start": 1,
            "remove_lines": ["exact line to remove"],
            "insert_lines": ["replacement line"]
          }
        ]
      }
    },
    {
      "Delete": {
        "path": "relative/path.ext",
        "precondition": "MustExist"
      }
    },
    {
      "Rename": {
        "from": "old/path.ext",
        "to": "new/path.ext",
        "on_conflict": "Fail",
        "source_precondition": "MustExist",
        "dest_precondition": "MustNotExist"
      }
    }
  ]
}

Rules:
- Paths must be relative to workspace root, no leading slash, no ".." traversal.
- For Modify, preconditions.content_hash can be any string — the system will replace it.
- For Create, precondition must be "MustNotExist".
- Each hunk's original_start is 1-based.
- Output ONLY a valid JSON object. No markdown, no explanation."#
}
