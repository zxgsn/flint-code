use std::path::Path;

#[test]
fn test_claude_code_import() {
    let test_file = std::env::temp_dir().join("test_session.jsonl");

    // Check if file exists
    if !test_file.exists() {
        println!("Test file not found, skipping");
        return;
    }

    // Test format detection
    let format = flint_cli::session_import::detect_format(test_file);
    assert_eq!(format, flint_cli::session_import::AgentFormat::ClaudeCode);

    // Test import
    let result = flint_cli::session_import::import_session(test_file);
    assert!(result.is_ok(), "Import failed: {:?}", result.err());

    let (session, meta) = result.unwrap();

    // Verify metadata
    assert_eq!(meta.session_id, "test-session-123");
    assert_eq!(meta.provider, "claude-code");
    assert_eq!(meta.model, "gpt-4o");
    assert_eq!(meta.message_count, 4);

    // Verify messages
    assert_eq!(session.messages.len(), 4);
    assert_eq!(session.messages[0].role, flint_types::Role::User);
    assert_eq!(session.messages[1].role, flint_types::Role::Assistant);
    assert_eq!(session.messages[2].role, flint_types::Role::User);
    assert_eq!(session.messages[3].role, flint_types::Role::Assistant);

    // Verify content
    let text0 = session.messages[0].text();
    assert_eq!(text0, "Hello, this is a test message");

    let text1 = session.messages[1].text();
    assert_eq!(text1, "Hello! I'm here to help.");

    println!("✓ Claude Code import test passed");
}

#[test]
fn test_flint_session_save_load() {
    use flint_agent::Session;
    use flint_types::Message;

    let test_dir = std::env::temp_dir().join("flint_test_sessions");
    if !test_dir.exists() {
        std::fs::create_dir_all(test_dir).unwrap();
    }

    let test_file = test_dir.join("test_session.json");

    // Create a test session
    let mut session = Session::new();
    session.add_user("Test message 1");
    session.add_assistant("Response 1");
    session.add_user("Test message 2");
    session.add_assistant("Response 2");

    // Save
    let result = session.save(&test_file, "openai", "gpt-4o");
    assert!(result.is_ok(), "Save failed: {:?}", result.err());

    // Load
    let result = Session::load(&test_file);
    assert!(result.is_ok(), "Load failed: {:?}", result.err());

    let (loaded_session, meta) = result.unwrap();

    // Verify
    assert_eq!(loaded_session.messages.len(), 4);
    assert_eq!(meta.provider, "openai");
    assert_eq!(meta.model, "gpt-4o");
    assert_eq!(meta.message_count, 4);

    // Cleanup
    let _ = std::fs::remove_dir_all(test_dir);

    println!("✓ Flint session save/load test passed");
}
