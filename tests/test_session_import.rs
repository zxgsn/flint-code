use std::path::Path;

#[test]
fn test_claude_code_format_detection() {
    let test_file = Path::new("/tmp/test_session.jsonl");

    // Create test file
    let content = r#"{"type":"last-prompt","leafUuid":"test-uuid","sessionId":"test-session-123"}
{"type":"user","message":{"role":"user","content":"Hello"},"uuid":"msg-1","timestamp":"2026-06-05T10:00:00Z","sessionId":"test-session-123"}
{"type":"assistant","message":{"id":"msg-2","type":"message","role":"assistant","model":"gpt-4o","content":[{"type":"text","text":"Hi!"}]},"uuid":"msg-3","timestamp":"2026-06-05T10:00:05Z","sessionId":"test-session-123"}"#;

    std::fs::write(test_file, content).unwrap();

    // Test format detection
    let format = flint_cli::session_import::detect_format(test_file);
    assert_eq!(format, flint_cli::session_import::AgentFormat::ClaudeCode);

    // Cleanup
    let _ = std::fs::remove_file(test_file);

    println!("✓ Claude Code format detection test passed");
}

#[test]
fn test_flint_format_detection() {
    let test_file = Path::new("/tmp/test_flint_session.json");

    // Create test file
    let content = r#"{
        "meta": {
            "id": "test-123",
            "created_at": "2026-06-06T10:00:00Z",
            "updated_at": "2026-06-06T10:05:00Z",
            "provider": "openai",
            "model": "gpt-4o",
            "title": "Test session",
            "message_count": 2
        },
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "Hello"}]},
            {"role": "assistant", "content": [{"type": "text", "text": "Hi there!"}]}
        ]
    }"#;

    std::fs::write(test_file, content).unwrap();

    // Test format detection
    let format = flint_cli::session_import::detect_format(test_file);
    assert_eq!(format, flint_cli::session_import::AgentFormat::Flint);

    // Cleanup
    let _ = std::fs::remove_file(test_file);

    println!("✓ Flint format detection test passed");
}
