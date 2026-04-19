/// Resume a session and re-enter the autonomous loop with restored context.
async fn resume_session_run(session_id: &str) -> Result<()> {
    
    use crow_workspace::PlanHydrator;

    let store = session::SessionStore::open()?;
    let mut loaded_session = store.load(&session::SessionId(session_id.to_string()))?;

    println!(
        "🦅 crow session resume — continuing session {}",
        &session_id[..8.min(session_id.len())]
    );
    println!("  Workspace: {}", loaded_session.workspace_root.display());
    println!("  Task: {}", loaded_session.task);

    let restored_messages = loaded_session.restore_messages();
    println!(
        "  Restored {} messages from history",
        restored_messages.len()
    );

    if !loaded_session.workspace_root.exists() {
        anyhow::bail!(
            "Workspace no longer exists: {}",
            loaded_session.workspace_root.display()
        );
    }

    let cfg = CrowConfig::load_for(&loaded_session.workspace_root)?;
    let snapshot_id = snapshot::resolve_snapshot_id(&loaded_session.workspace_root);
    println!("  Snapshot ID: {}", snapshot_id.0);

    // Compare snapshot timeline to detect workspace drift
    if let Some(last_snap) = loaded_session.snapshot_timeline.last() {
        if *last_snap != snapshot_id {
            println!("  ⚠️  Workspace has changed since last session snapshot");
            println!("     Last: {} → Current: {}", last_snap.0, snapshot_id.0);
        } else {
            println!("  ✅ Workspace matches last session snapshot");
        }
    }

    // Probe workspace
    let profile = scan_workspace(&loaded_session.workspace_root).map_err(|e| anyhow::anyhow!(e))?;
    let candidate = match profile.verification_candidates.first() {
        Some(c) => c.clone(),
        None => {
            anyhow::bail!(
                "No verification candidates found. Cannot safely resume execution without a test suite.\n\
                 Please configure a custom test script in `.crow/config.json`."
            );
        }
    };

    // Materialize sandbox
    println!("\n  Materializing sandbox...");
    let mat_config = MaterializeConfig {
        source: loaded_session.workspace_root.clone(),
        artifact_dirs: profile.ignore_spec.artifact_dirs.clone(),
        skip_patterns: profile.ignore_spec.ignore_patterns.clone(),
        allow_hardlinks: false,
    };
    let sandbox = tokio::task::spawn_blocking(move || materialize(&mat_config))
        .await
        .context("Materialization task panicked")?
        .context("Failed to materialize sandbox")?;
    let frozen_root = sandbox.path().to_path_buf();

    // Build repo map from frozen sandbox
    let repo_map = cfg
        .build_repo_map_for(&frozen_root)
        .map_err(|e| anyhow::anyhow!(e))?;

    let mcp_manager = crate::mcp::McpManager::boot(&cfg.mcp_servers).await?;
    let sys_msgs = crate::prompt::PromptBuilder::new()
        .with_repo_map(&repo_map, &snapshot_id)
        .with_mcp(Some(&mcp_manager))
        .with_contract(&snapshot_id)
        .build();

    // Rebuild conversation manager with system context + restored history
    let mut messages = context::ConversationManager::new(sys_msgs);

    // Restore non-system messages from session history
    for msg in &restored_messages {
        match msg.role {
            crow_brain::ChatRole::User => messages.push_user(&msg.content),
            crow_brain::ChatRole::Assistant => messages.push_assistant(&msg.content),
            crow_brain::ChatRole::System => {} // System messages rebuilt above
        }
    }

    // Add a continuation prompt
    messages.push_user(format!(
        "[SESSION RESUMED]\nContinuing work on the original task: {}\n\nPlease pick up where you left off. If the previous attempt failed, try a different approach.",
        loaded_session.task
    ));

    println!("  Entering crucible loop...\n");

    let client = cfg.build_llm_client().map_err(|e| anyhow::anyhow!(e))?;
    let compiler = crow_brain::IntentCompiler::new(client);

    let mut obs = crate::event::CliEventHandler::new();
    let compiled_plan = epistemic::run_epistemic_loop(
        &compiler,
        &mut messages,
        &frozen_root,
        Some(&mcp_manager),
        &mut obs,
    )
    .await?;


    // Hydrate + apply + verify
    let attempt_mat_config = MaterializeConfig {
        source: frozen_root.clone(),
        artifact_dirs: profile.ignore_spec.artifact_dirs.clone(),
        skip_patterns: profile.ignore_spec.ignore_patterns.clone(),
        allow_hardlinks: false,
    };
    let attempt_sandbox = tokio::task::spawn_blocking(move || materialize(&attempt_mat_config))
        .await
        .context("Materialization task panicked")?
        .context("Failed to materialize attempt sandbox")?;

    let attempt_sandbox_path = attempt_sandbox.path().to_path_buf();
    let plan_clone = compiled_plan.clone();
    let snap_clone = snapshot_id.clone();
    let hydrated_plan = tokio::task::spawn_blocking(move || {
        PlanHydrator::hydrate(&plan_clone, &snap_clone, &attempt_sandbox_path)
    })
    .await
    .context("Hydration task panicked")?
    .context("Hydration failed")?;

    let plan_for_apply = hydrated_plan.clone();
    let sandbox_view = attempt_sandbox.non_owning_view();
    tokio::task::spawn_blocking(move || apply_plan_to_sandbox(&plan_for_apply, &sandbox_view))
        .await
        .context("Apply task panicked")?
        .context("Failed to apply plan to sandbox")?;

    // Preflight check
    let preflight_result = crow_verifier::preflight::run_preflight(
        attempt_sandbox.path(),
        Some(&frozen_root),
        std::time::Duration::from_secs(60),
        &profile.primary_lang,
    )
    .await;

    match &preflight_result {
        crow_verifier::preflight::PreflightResult::Clean => {
            println!("  ✅ Preflight: code compiles cleanly");
        }
        crow_verifier::preflight::PreflightResult::Errors(diags) => {
            let summary = crow_verifier::preflight::format_diagnostics(diags);
            println!("  ❌ Preflight: {} compile error(s)", diags.len());
            println!("{}", summary);
        }
        crow_verifier::preflight::PreflightResult::Skipped(reason) => {
            println!("  ⚠️  Preflight skipped: {}", reason);
        }
    }

    // Full verification
    let exec_config = ExecutionConfig {
        timeout: std::time::Duration::from_secs(60),
        max_output_bytes: 5 * 1024 * 1024,
    };
    let result = crow_verifier::executor::execute(
        attempt_sandbox.path(),
        &candidate.command,
        &exec_config,
        &crow_verifier::types::AciConfig::compact(),
        Some(&frozen_root),
    )
    .await
    .context("Verification execution failed")?;

    let outcome = &result.test_run.outcome;
    println!("\n╔══════════════════════════════════════╗");
    println!("║  Resume Verdict: {:?}", outcome);
    println!("╚══════════════════════════════════════╝");
    println!("Evidence:\n{}", result.test_run.truncated_log);

    // Diff
    println!("\n--- Changes ---");
    diff::render_plan_diff(&frozen_root, attempt_sandbox.path(), &hydrated_plan);

    if outcome != &crow_evidence::TestOutcome::Passed {
        anyhow::bail!("Resumed session: verification failed.");
    }

    // Write-back phase
    if matches!(
        cfg.write_mode,
        config::WriteMode::WorkspaceWrite | config::WriteMode::DangerFullAccess
    ) {
        println!("\n[4] Writing verified changes to workspace...");
        if let Err(e) = apply_sandbox_to_workspace(&cfg.workspace, &hydrated_plan) {
            eprintln!("  ❌ Failed to apply to workspace: {:?}", e);
            anyhow::bail!("Workspace mutation failed during resume.");
        }

        println!("  ✅ Workspace updated successfully.");
        if let Err(e) = crate::snapshot::commit_applied_plan(&cfg.workspace, &hydrated_plan) {
            println!("  ⚠️  Could not automatically commit changes: {}", e);
        } else {
            println!("  ✅ Changes committed to git timeline.");
        }
    } else {
        println!("\n  📦 Write mode is sandbox-only. Changes not applied to workspace.");
    }

    // Update session AFTER mutations to properly anchor snapshot
    let post_mutation_snapshot = snapshot::resolve_snapshot_id(&cfg.workspace);
    loaded_session.save_messages(&messages.as_messages());
    loaded_session.push_snapshot(post_mutation_snapshot);
    store.save(&loaded_session)?;
    println!("\n  💾 Session updated: {}", loaded_session.id.0);

    println!("\n[🎉] Resumed session completed successfully!");
    Ok(())
}
