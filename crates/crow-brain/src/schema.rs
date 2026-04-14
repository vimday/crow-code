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
    r#"Expected JSON Schema for AgentAction:

You may choose ONE of the following actions.
If you need to see the exact code of a file before modifying it, use "read_files".
If you need to run a read-only command (e.g. grep, ls, cargo check) to understand the codebase, use "run_command".
If you are ready to apply changes, use "submit_plan".

ACTION TYPE 1: Read Files
{
  "action": "read_files",
  "paths": ["src/main.rs", "src/lib.rs"],
  "rationale": "I need to see the function body to correctly replace it."
}

ACTION TYPE 2: Run Command (reconnaissance only, allowlisted programs)
{
  "action": "run_command",
  "program": "grep",
  "args": ["-rn", "fn main", "src/"],
  "rationale": "I need to find all main functions in the project."
}

ACTION TYPE 3: Submit Plan
{
  "action": "submit_plan",
  "plan": {
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
              "remove_block": "exact lines to remove\nas a single string",
              "insert_block": "replacement lines\nas a single string"
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
}

Rules:
- Output ONLY a valid JSON object matching ONE of the action types above.
- Paths must be relative to workspace root, no leading slash, no ".." traversal.
- For Modify, preconditions.content_hash can be any string — the system will replace it.
- For Create, precondition must be "MustNotExist".
- Each hunk's original_start is 1-based.
- `remove_block` and `insert_block` must be single strings using `\n` for line breaks. Do NOT use arrays for lines.
- IMPORTANT: `remove_block` must NEVER be empty. For insertions, include at least one existing line as anchor context in `remove_block` and repeat that line alongside your new lines in `insert_block`. Example: to insert "new_line" after "line 2", set remove_block="line 2\n" and insert_block="line 2\nnew_line\n".
- For run_command, `program` must be from the allowlist (ls, cat, head, tail, find, wc, rg, grep, tree, file, stat, du). `args` is a list of argument strings.
- Output ONLY valid JSON. No markdown, no explanation."#
}
