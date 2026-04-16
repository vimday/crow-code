# RFC-001: Crow-Code Production Architecture Baseline

**Status:** 🟢 Ready for Implementation
**Objective:** To build a verified, timeline-rewindable, zero-leak refactoring engine for cross-file physical consistency.
**Core Principle:** Evidence over rhetoric, Patches are first-class citizens, Never blindly write to disk.

## 1. Abstract
Crow-Code is an intelligent coding agent architecture designed with extreme defensive engineering. It shifts the paradigm from simple "LLM text manipulation" to "Evidence-based materialization." The system operates on a dual-track proposal and MCTS search mechanism, generating atomic patches that must pass a rigorous crucible of local environment verification before finalizing a write to the physical workspace. 

## 2. Goals
- **O(1) Snapshot State Machine:** Ensure flawless workspace isolation during agent reasoning.
- **Evidence-Driven Resolution:** Replace opaque 0-100 confidence scores with multidimensional, verifiable matrices (lints, test passes, intelligence confidence).
- **Graceful Degradation:** A tri-state final resolution (Auto-apply, Review-required, Escalate-with-evidence).
- **Time-Travel Safety:** Every action must be logged in an event-sourcing model that supports rewind and deterministic replay.

## 3. Non-Goals
- We will **not** build custom cross-language compilers; we rely on OS and local toolchains.
- We do **not** promise to auto-discover all correct build/test hooks (user overrides or heuristics apply).
- We do **not** guarantee 100% auto-apply rates; safety outweighs automation.
- We do **not** guarantee optimal filesystem isolation setups (e.g., `clonefile`) on day 1 for all OSs, falling back to safe copies if necessary.
- We do **not** implement full OS-level process sandboxing (e.g., `bwrap`, `nsjail`, macOS Seatbelt) for Sprint 0. Current execution isolation is constrained strictly to bounding the physical workspace directory, environment variables, time, and bytes. Deep network and system read protections are deferred.

## 4. System Invariants
- **No Direct Disk Writes:** The LLM cannot directly modify the user's workspace. All changes are buffered as Intent Plans.
- **Precondition Verification:** Final disk flushes must match the hash, baseline snapshot ID, and file precondition rules before applying.
- **Zero Pollution:** A failed MCTS sequence or verification step must leave the physical workspace completely untouched.
- **Evidence Traceability:** Every risk flag or test pass must trace back to a specific command log, snapshot, or parser output.

## 5. Architectural Topology (Physical Crates)

All 10 crates are pre-split from day 1 as separate Cargo workspace members.
This is an intentional deviation from the original "start monolithic, split
later" strategy. Rationale: Rust's compilation model naturally enforces
dependency boundaries at the crate level, so pre-splitting gives us free
circular-dependency detection from `cargo check` on every commit. The cost
(interface design overhead) is acceptable because the Currency crates have
no inter-dependencies and higher-layer crates only depend downward.

**Dependency rule (convention):** crates may only depend on crates listed
_above_ them in this document. `cargo check` catches cyclic violations but
does _not_ prevent an acyclic upward dependency (e.g., a Currency crate
pulling in a Control crate). Until we add an explicit enforcement mechanism
(planned: `cargo-deny` policy or `xtask lint-deps`), this rule is upheld by
code review discipline.

### The Currencies (Data Primitives)
- `crow-patch`: Fuzzy-matching, struct-based diff application, rename, and structural AST manipulations.
- `crow-evidence`: Verifiable data structures defining the safety of a patch.
- `crow-probe`: Local workspace radar for framework detection, verification targets, and ignore extraction.

### The Crucible & Runtime
- `crow-workspace`: Event-sourcing log and VFS snapshot state machine.
- `crow-materialize`: Workspace-isolation physical copy engine (CoW / safe-copy fallback) to clone environments safely.
- `crow-verifier`: Workspace-isolated command executor that truncates standard outputs (ACI Log Pruning) cleanly.

### Intelligence & Control
- `crow-intel`: Tree-sitter powered codebase intelligence ensuring language-aware validations.
- `crow-brain`: Intent compiler, budget governor, and dual-track MCTS solver.

### Observability
- `crow-cli`: Evidence-first Ratatui terminal UI.
- `crow-replay`: State regression and debug testing harness.

## 6. Core Data Contracts

### 6.1 Unified Patch Intent
```rust
pub enum EditOp {
    Modify { 
        path: WorkspacePath, 
        preconditions: PreconditionState, // e.g., base_hash, lines
        hunks: Vec<DiffHunk> 
    },
    Create { path: WorkspacePath, content: String },
    Rename { from: WorkspacePath, to: WorkspacePath, on_conflict: ConflictStrategy },
    Delete { path: WorkspacePath },
}

pub struct IntentPlan {
    pub base_snapshot_id: SnapshotId,  // Crucial for consistency
    pub rationale: String,
    pub is_partial: bool,              // Supports exploratory branching
    pub confidence: Confidence,
    pub operations: Vec<EditOp>,
}
```

### 6.2 Evidence Matrix
```rust
pub struct EvidenceMatrix {
    pub compile_runs: Vec<TestRun>,            // Structured test history, not just f32
    pub lints_clean: bool,
    pub intelligence_confidence: Confidence,   // e.g. LSP resolution success
    pub risk_flags: Vec<RiskFlag>,             // e.g. "Deleted critical authentication routing"
}
```

### 6.3 Workspace Probe
```rust
pub struct ProjectProfile {
    pub primary_lang: DetectedLanguage,
    pub workspace_root: PathBuf,               // Absolute OS path, not WorkspacePath
    pub verification_candidates: Vec<VerificationCandidate>,
    pub ignore_spec: FilterSpec,
}
```

## 7. Failure Modes & Tri-State Resolution

1. 🟢 **Auto-Apply:** 
   - Criteria: `EvidenceMatrix` fully green, no severe `RiskFlags`, and execution logic semantically safe.
2. 🟡 **Review-Required:** 
   - Criteria: Safe base, but multiple paths found or high-risk areas modified. Presented to humane via TUI split-diffs.
3. 🔴 **Escalate-with-Evidence:** 
   - Criteria: All verification targets failed or budget depleted. Halts immediately, providing the user the isolated ACI truncated logs and stack traces without altering the workspace.

## 8. Rollout Plan (Sprint 0)

All 10 crates exist as physical workspace members from Step 1. Each step
adds real types and tests to exactly one layer, verified by `cargo test`
before moving on.

- **Step 1: Workspace Genesis** — Skeleton crates, `cargo check` green. ✅
- **Step 2: Core Data Contracts** — Implement types in the three Currency crates (`crow-patch`, `crow-evidence`, `crow-probe`). ✅
- **Step 3: Workspace-Isolation Materialization** — Implement physical working tree cloning in `crow-materialize`. APFS clonefile, SafeCopy, HardlinkTree (opt-in). ✅
- **Step 4: ACI Log Truncation** — Implement isolated execution constraint pipelines in `crow-verifier`. Verified by feeding 100K-line logs and asserting output ≤ 200 lines. ✅
- **Step 5: Probe + Applier + God Pipeline** — Implement detection heuristics in `crow-probe`, physical patch applier in `crow-workspace`, end-to-end integration pipeline in `crow-cli`. ✅
- **Step 6: MCTS Parallel Crucible & Cache Isolation** — Implement autonomous epistemic loops, prompt cache economic gates, and isolated branch-specific build cache clones to bypass file-lock serialization and maximize concurrent evaluation. ✅
