//! Security tests for Phase 1b.
//!
//! Covers: approval policy, file sandbox, untrusted-data framing,
//! secret redaction, and project context file trust.

use rho_core::{
    AgentConfig, ChatMessage, NopObserver, RhoConfig, Session, ToolCallId, ToolName, ToolRegistry,
    ToolRisk,
    agent::{LoopParams, run_loop},
    approval::{ApprovalPolicy, DefaultApprovalPolicy},
    context_files::{ContextScanner, compose_system_prompt},
    tool::CancellationToken,
};
use rho_test_helpers::{
    AutoApproveGate, AutoDenyGate, FixedResponseTool, MockChatClient, empty_trust_store,
    tempdir_with_sandbox, text_response, tool_call_response,
};
use std::io::Cursor;

// ── Approval policy ───────────────────────────────────────────────────────────

#[test]
fn read_tools_auto_approved() {
    let policy = DefaultApprovalPolicy;
    assert!(!policy.requires_approval(&ToolName::from("read_file"), ToolRisk::Read));
}

#[test]
fn write_tools_require_approval() {
    let policy = DefaultApprovalPolicy;
    assert!(policy.requires_approval(&ToolName::from("write_file"), ToolRisk::Write));
}

#[test]
fn destructive_tools_require_approval() {
    let policy = DefaultApprovalPolicy;
    assert!(policy.requires_approval(&ToolName::from("run_command"), ToolRisk::Destructive));
}

#[tokio::test]
async fn denied_tool_gets_denial_message_fed_back() {
    // Model requests a destructive tool; user denies it; model then says "ok".
    let client = MockChatClient::new(vec![
        tool_call_response("call_1", "bang", "{}"),
        text_response("understood, skipping"),
    ]);

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(FixedResponseTool {
        name: "bang",
        response: "executed".into(),
        risk: ToolRisk::Destructive,
    }));

    let config = AgentConfig::default(); // DefaultApprovalPolicy → requires approval
    let mut session = Session::in_memory("mock", None, registry.tool_definitions(), "/tmp");

    // AutoDenyGate always says no.
    let params = LoopParams {
        client: &client,
        registry: &registry,
        config: &config,
        cancel: CancellationToken::new(),
        gate: &AutoDenyGate,
        observer: &NopObserver,
    };
    let result = run_loop(&mut session, "do the destructive thing", &params)
        .await
        .unwrap();

    assert_eq!(result, "understood, skipping");

    // The second request must include a Tool message with a denial text.
    let requests = client.requests();
    let second = &requests[1];
    let denial_msg = second
        .messages
        .iter()
        .find(|m| matches!(m, ChatMessage::Tool { .. }));
    assert!(
        denial_msg.is_some(),
        "expected Tool denial message in history"
    );
    if let Some(ChatMessage::Tool { content, .. }) = denial_msg {
        let text = match &content[0] {
            rho_core::ContentBlock::Text { text } => text.clone(),
        };
        assert!(
            text.to_lowercase().contains("denied"),
            "denial message should mention denial: {text}"
        );
    }
}

#[tokio::test]
async fn approved_tool_executes() {
    let client = MockChatClient::new(vec![
        tool_call_response("call_1", "bang", "{}"),
        text_response("done"),
    ]);

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(FixedResponseTool {
        name: "bang",
        response: "executed".into(),
        risk: ToolRisk::Destructive,
    }));

    let config = AgentConfig::default();
    let mut session = Session::in_memory("mock", None, registry.tool_definitions(), "/tmp");

    // AutoApproveGate always says yes.
    let params = LoopParams {
        client: &client,
        registry: &registry,
        config: &config,
        cancel: CancellationToken::new(),
        gate: &AutoApproveGate,
        observer: &NopObserver,
    };
    let result = run_loop(&mut session, "do it", &params).await.unwrap();

    assert_eq!(result, "done");

    // Verify the tool result (not a denial) is in history.
    let requests = client.requests();
    assert_eq!(requests.len(), 2);
    let tool_msg = requests[1]
        .messages
        .iter()
        .find(|m| matches!(m, ChatMessage::Tool { .. }));
    assert!(tool_msg.is_some());
    if let Some(ChatMessage::Tool { content, .. }) = tool_msg {
        let text = match &content[0] {
            rho_core::ContentBlock::Text { text } => text.clone(),
        };
        assert_eq!(text, "executed");
    }
}

// ── File sandbox ──────────────────────────────────────────────────────────────

#[test]
fn validates_existing_file_inside_root() {
    let (dir, root) = tempdir_with_sandbox();
    let file = dir.path().join("hello.txt");
    std::fs::write(&file, "hello").unwrap();
    assert!(root.validate(&file).is_ok());
}

#[test]
fn rejects_existing_file_outside_root() {
    let (inside, root) = tempdir_with_sandbox();
    let outside = tempfile::tempdir().unwrap();
    let file = outside.path().join("evil.txt");
    std::fs::write(&file, "evil").unwrap();
    let _ = inside; // keep alive
    assert!(root.validate(&file).is_err());
}

#[test]
fn rejects_dotdot_traversal_outside_root() {
    let (dir, root) = tempdir_with_sandbox();
    // Construct a path that traverses out: dir/../../../tmp/evil
    let traversal = dir.path().join("..").join("evil.txt");
    // validate uses canonicalize which resolves .., so the canonical path
    // will be outside the root.
    let result = root.validate(&traversal);
    // Either it errors (path doesn't exist) or it resolves outside and is rejected.
    match result {
        Err(_) => {} // expected
        Ok(fp) => assert!(
            root.contains(&fp),
            "path {fp} resolved inside root — traversal was a no-op"
        ),
    }
}

#[test]
fn validates_new_file_for_write_inside_root() {
    let (dir, root) = tempdir_with_sandbox();
    let new_file = dir.path().join("subdir").join("new.txt");
    assert!(root.validate_for_write(&new_file).is_ok());
}

#[cfg(unix)]
#[test]
fn rejects_symlink_pointing_outside_root() {
    use std::os::unix::fs::symlink;

    let (inside, root) = tempdir_with_sandbox();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("secret.txt");
    std::fs::write(&target, "secret").unwrap();

    // Create a symlink inside the sandbox that points outside.
    let link = inside.path().join("link.txt");
    symlink(&target, &link).unwrap();

    // validate() must resolve the symlink and reject it.
    let result = root.validate(&link);
    assert!(
        result.is_err(),
        "symlink pointing outside sandbox must be rejected"
    );
}

#[test]
fn rejects_new_file_for_write_outside_root() {
    let (_dir, root) = tempdir_with_sandbox();
    let outside = tempfile::tempdir().unwrap();
    let new_file = outside.path().join("evil.txt");
    assert!(root.validate_for_write(&new_file).is_err());
}

#[test]
fn rejects_dotdot_in_non_existent_suffix() {
    let (dir, root) = tempdir_with_sandbox();
    // nonexistent/../../../evil — .. after non-existent segment must fail.
    let sneaky = dir.path().join("nonexistent").join("..").join("evil.txt");
    assert!(root.validate_for_write(&sneaky).is_err());
}

// ── Untrusted-data framing ────────────────────────────────────────────────────

#[test]
fn user_context_text_wraps_in_context_tags() {
    let msg = ChatMessage::user_context_text("file contents here");
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("<context>"));
    assert!(json.contains("file contents here"));
    assert!(json.contains("<context:end>"));
}

#[test]
fn context_framing_survives_serde_round_trip() {
    let msg = ChatMessage::user_context_text("sensitive data");
    let json = serde_json::to_string(&msg).unwrap();
    let back: ChatMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(msg, back);
}

// ── Secret redaction ──────────────────────────────────────────────────────────

#[test]
fn redaction_applied_before_tool_result_enters_history() {
    use rho_core::tool::ToolResult;

    let secret_key = "sk-".to_owned() + &"z".repeat(32);
    let output = format!("found key: {secret_key}");

    let mut session = Session::in_memory("mock", None, vec![], "/tmp");
    session.add_tool_result(ToolCallId::from("call_1"), &ToolResult::success(&output));

    // The message stored in history must not contain the raw secret.
    let messages = session.path_messages();
    let tool_msg = messages
        .iter()
        .find(|m| matches!(m, ChatMessage::Tool { .. }))
        .expect("tool message must be in history");

    let json = serde_json::to_string(tool_msg).unwrap();
    assert!(
        !json.contains(&secret_key),
        "raw secret must not appear in history: {json}"
    );
    assert!(json.contains("[REDACTED]"), "REDACTED marker must appear");
}

#[test]
fn redaction_applied_to_aws_key_in_tool_result() {
    use rho_core::tool::ToolResult;

    let mut session = Session::in_memory("mock", None, vec![], "/tmp");
    session.add_tool_result(
        ToolCallId::from("call_2"),
        &ToolResult::success("AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE"),
    );

    let messages = session.path_messages();
    let json = serde_json::to_string(messages.last().unwrap()).unwrap();
    assert!(!json.contains("AKIAIOSFODNN7EXAMPLE"));
    assert!(json.contains("[REDACTED]"));
}

// ── Project context file trust ────────────────────────────────────────────────

#[test]
fn new_context_file_prompts_for_confirmation() {
    let (dir, root) = tempdir_with_sandbox();
    std::fs::write(dir.path().join("AGENTS.md"), "# instructions").unwrap();
    let (mut store, _store_dir) = empty_trust_store();
    let scanner = ContextScanner::new(&root);

    let mut input = Cursor::new(b"y\n");
    let mut output = Vec::new();
    let trusted = scanner.run(&mut store, &mut input, &mut output);

    assert_eq!(trusted.len(), 1, "expected 1 trusted file");
    assert_eq!(trusted[0].name, "AGENTS.md");
}

#[test]
fn unchanged_context_file_loads_silently() {
    let (dir, root) = tempdir_with_sandbox();
    std::fs::write(dir.path().join("AGENTS.md"), "# instructions").unwrap();
    let (mut store, _store_dir) = empty_trust_store();
    let scanner = ContextScanner::new(&root);

    // First load: trust it.
    let mut input = Cursor::new(b"y\n");
    let mut out = Vec::new();
    scanner.run(&mut store, &mut input, &mut out);

    // Second load: no prompt — should load silently.
    let mut input2 = Cursor::new(b""); // would error if prompted
    let mut out2 = Vec::new();
    let trusted = scanner.run(&mut store, &mut input2, &mut out2);

    assert_eq!(trusted.len(), 1);
    let output_text = String::from_utf8_lossy(&out2);
    assert!(
        !output_text.contains("Trust"),
        "should not prompt for unchanged file"
    );
}

#[test]
fn changed_context_file_prompts_for_reconfirmation() {
    let (dir, root) = tempdir_with_sandbox();
    std::fs::write(dir.path().join("AGENTS.md"), "# original").unwrap();
    let (mut store, _store_dir) = empty_trust_store();
    let scanner = ContextScanner::new(&root);

    // Trust the original.
    let mut input = Cursor::new(b"y\n");
    let mut out = Vec::new();
    scanner.run(&mut store, &mut input, &mut out);

    // Modify the file.
    std::fs::write(dir.path().join("AGENTS.md"), "# modified — different").unwrap();

    // Re-run: must prompt again.
    let mut input2 = Cursor::new(b"y\n");
    let mut out2 = Vec::new();
    let trusted = scanner.run(&mut store, &mut input2, &mut out2);

    let output_text = String::from_utf8_lossy(&out2);
    assert!(
        output_text.contains("changed"),
        "should mention 'changed': {output_text}"
    );
    assert_eq!(trusted.len(), 1);
}

#[test]
fn rejected_context_file_not_included_in_prompt() {
    let (dir, root) = tempdir_with_sandbox();
    std::fs::write(dir.path().join("AGENTS.md"), "# instructions").unwrap();
    let (mut store, _store_dir) = empty_trust_store();
    let scanner = ContextScanner::new(&root);

    let mut input = Cursor::new(b"n\n");
    let mut out = Vec::new();
    let trusted = scanner.run(&mut store, &mut input, &mut out);

    assert!(trusted.is_empty(), "rejected file must not be trusted");
}

// ── Config-driven approval policy ────────────────────────────────────────────

#[test]
fn config_approval_auto_overrides_default() {
    use rho_core::{ApprovalAction, ApprovalConfig, RhoConfig, approval::ConfigApprovalPolicy};

    let config = RhoConfig {
        approval: ApprovalConfig {
            per_tool: vec![("run_command".to_owned(), ApprovalAction::Auto)]
                .into_iter()
                .collect(),
        },
        ..Default::default()
    };

    let policy = ConfigApprovalPolicy::new(&config);
    // run_command is Destructive, but config says Auto.
    assert!(!policy.requires_approval(&ToolName::from("run_command"), ToolRisk::Destructive));
}

#[test]
fn config_approval_ask_overrides_default() {
    use rho_core::{ApprovalAction, ApprovalConfig, RhoConfig, approval::ConfigApprovalPolicy};

    let config = RhoConfig {
        approval: ApprovalConfig {
            per_tool: vec![("read_file".to_owned(), ApprovalAction::Ask)]
                .into_iter()
                .collect(),
        },
        ..Default::default()
    };

    let policy = ConfigApprovalPolicy::new(&config);
    // read_file is Read (auto-approved by default), but config says Ask.
    assert!(policy.requires_approval(&ToolName::from("read_file"), ToolRisk::Read));
}

#[test]
fn config_approval_deny_requires_approval() {
    use rho_core::{ApprovalAction, ApprovalConfig, RhoConfig, approval::ConfigApprovalPolicy};

    let config = RhoConfig {
        approval: ApprovalConfig {
            per_tool: vec![("write_file".to_owned(), ApprovalAction::Deny)]
                .into_iter()
                .collect(),
        },
        ..Default::default()
    };

    let policy = ConfigApprovalPolicy::new(&config);
    // Deny means the tool must go through the approval gate so the gate can
    // issue a denial. It requires_approval() returns true.
    assert!(policy.requires_approval(&ToolName::from("write_file"), ToolRisk::Write));
    assert!(policy.is_denied(&ToolName::from("write_file")));
}

#[test]
fn config_approval_falls_back_to_default() {
    use rho_core::{ApprovalConfig, RhoConfig, approval::ConfigApprovalPolicy};

    let config = RhoConfig {
        approval: ApprovalConfig::default(),
        ..Default::default()
    };

    let policy = ConfigApprovalPolicy::new(&config);
    // No per-tool overrides — default policy applies.
    assert!(!policy.requires_approval(&ToolName::from("read_file"), ToolRisk::Read));
    assert!(policy.requires_approval(&ToolName::from("write_file"), ToolRisk::Write));
    assert!(policy.requires_approval(&ToolName::from("run_command"), ToolRisk::Destructive));
}

// ── Config sandbox opt-out ────────────────────────────────────────────────────

#[test]
fn config_sandbox_enabled_by_default() {
    let config = RhoConfig::default();
    assert!(config.sandbox.enabled);
}

#[test]
fn config_sandbox_can_be_disabled() {
    use rho_core::SandboxConfig;

    let config = RhoConfig {
        sandbox: SandboxConfig { enabled: false },
        ..Default::default()
    };
    assert!(!config.sandbox.enabled);
}

// ── Config redaction toggle ───────────────────────────────────────────────────

#[test]
fn config_redaction_enabled_by_default() {
    let config = RhoConfig::default();
    assert!(config.redaction.enabled);
}

#[test]
fn config_redaction_can_be_disabled() {
    use rho_core::RedactionConfig;

    let config = RhoConfig {
        redaction: RedactionConfig {
            enabled: false,
            custom_patterns: vec![],
        },
        ..Default::default()
    };
    assert!(!config.redaction.enabled);
}

#[test]
fn disabled_redactor_skips_builtin_patterns() {
    use rho_core::tool::ToolResult;

    // A disabled redactor should pass secrets through unchanged.
    let redactor = rho_core::Redactor::from_config(false, &[]);
    let mut session = Session::in_memory("mock", None, vec![], "/tmp").with_redactor(redactor);

    let secret_key = "sk-".to_owned() + &"x".repeat(32);
    session.add_tool_result(
        ToolCallId::from("call_1"),
        &ToolResult::success(&secret_key),
    );

    // The raw secret should be present in history (not redacted).
    let messages = session.path_messages();
    let tool_msg = messages
        .iter()
        .find(|m| matches!(m, ChatMessage::Tool { .. }))
        .unwrap();
    let json = serde_json::to_string(tool_msg).unwrap();
    assert!(
        json.contains(&secret_key),
        "disabled redactor should not redact: {json}"
    );
    assert!(
        !json.contains("[REDACTED]"),
        "disabled redactor should not add REDACTED: {json}"
    );
}

#[test]
fn enabled_redactor_with_custom_pattern_redacts_in_session() {
    use rho_core::tool::ToolResult;

    // A config-driven redactor with a custom pattern should redact
    // matches of that pattern when tool results enter the session.
    let redactor = rho_core::Redactor::from_config(true, &[r"COMPANY_KEY_\S+".to_owned()]);
    let mut session = Session::in_memory("mock", None, vec![], "/tmp").with_redactor(redactor);

    session.add_tool_result(
        ToolCallId::from("call_1"),
        &ToolResult::success("found COMPANY_KEY_abc123 here"),
    );

    let messages = session.path_messages();
    let tool_msg = messages
        .iter()
        .find(|m| matches!(m, ChatMessage::Tool { .. }))
        .unwrap();
    let json = serde_json::to_string(tool_msg).unwrap();
    assert!(
        json.contains("[REDACTED]"),
        "custom pattern should redact: {json}"
    );
    assert!(
        !json.contains("COMPANY_KEY_abc123"),
        "custom pattern match should be redacted: {json}"
    );
}

// ── Config API key handling ───────────────────────────────────────────────────

#[test]
fn config_api_key_not_in_plaintext() {
    // API keys are stored as env var references, never in the config struct.
    let config = RhoConfig::default();
    assert!(config.provider.is_empty());
    assert!(config.resolve_api_key().is_none());
}

#[test]
fn compose_system_prompt_inserts_context_files() {
    let base = "You are rho.";
    let files = vec![
        rho_core::ContextFile {
            name: "AGENTS.md".to_owned(),
            contents: "Use Rust edition 2024.".to_owned(),
        },
        rho_core::ContextFile {
            name: ".cursorrules".to_owned(),
            contents: "Prefer small functions.".to_owned(),
        },
    ];
    let composed = compose_system_prompt(base, &files);

    assert!(composed.starts_with(base));
    assert!(composed.contains("--- AGENTS.md ---"));
    assert!(composed.contains("Use Rust edition 2024."));
    assert!(composed.contains("--- .cursorrules ---"));
    assert!(composed.contains("Prefer small functions."));
}
