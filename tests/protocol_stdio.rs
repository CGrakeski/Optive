#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

fn optive() -> Command {
    Command::new(env!("CARGO_BIN_EXE_Optive"))
}

fn frame(value: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(value).expect("serialize protocol request");
    let mut framed = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    framed.extend(body);
    framed
}

fn temporary_dir(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("optive-{name}-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&path).expect("create temporary directory");
    path
}

#[test]
fn dap_stdout_remains_framed_when_debuggee_prints() {
    let dir = temporary_dir("dap-stdio");
    let script = dir.join("main.tive");
    std::fs::write(&script, "print(\"debuggee output\")\n").expect("write script");

    let requests = [
        json!({"seq": 1, "type": "request", "command": "initialize", "arguments": {}}),
        json!({
            "seq": 2,
            "type": "request",
            "command": "launch",
            "arguments": {"program": script, "cwd": dir}
        }),
        json!({"seq": 3, "type": "request", "command": "continue", "arguments": {}}),
        json!({"seq": 4, "type": "request", "command": "disconnect", "arguments": {}}),
    ];
    let mut child = optive()
        .arg("dap")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn DAP");
    {
        let stdin = child.stdin.as_mut().expect("DAP stdin");
        for request in &requests {
            stdin.write_all(&frame(request)).expect("write DAP frame");
        }
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("wait for DAP");
    assert!(
        output.status.success(),
        "DAP failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut reader = std::io::Cursor::new(&output.stdout);
    let mut messages = Vec::new();
    while let Some(message) = optive::rpc::read_json(&mut reader).expect("valid DAP frame") {
        messages.push(message);
    }
    assert!(
        messages.iter().any(|message| {
            message["type"] == "event"
                && message["event"] == "output"
                && message["body"]["category"] == "stdout"
                && message["body"]["output"]
                    .as_str()
                    .is_some_and(|text| text.contains("debuggee output"))
        }),
        "{messages:#?}"
    );
    std::fs::remove_dir_all(&dir).expect("remove temporary directory");
}

#[test]
fn dap_breakpoint_and_step_over_stdio() {
    let dir = temporary_dir("dap-step");
    let script = dir.join("main.tive");
    std::fs::write(&script, "let x = 1\nlet y = x + 1\nprint(y)\n").expect("write script");
    let program = script.to_string_lossy().into_owned();

    let requests = [
        json!({"seq": 1, "type": "request", "command": "initialize", "arguments": {}}),
        json!({
            "seq": 2,
            "type": "request",
            "command": "launch",
            "arguments": {"program": program, "cwd": dir}
        }),
        json!({
            "seq": 3,
            "type": "request",
            "command": "setBreakpoints",
            "arguments": {
                "source": { "path": script },
                "breakpoints": [{ "line": 2 }]
            }
        }),
        json!({"seq": 4, "type": "request", "command": "continue", "arguments": {"threadId": 1}}),
        json!({"seq": 5, "type": "request", "command": "next", "arguments": {"threadId": 1}}),
        json!({"seq": 6, "type": "request", "command": "continue", "arguments": {"threadId": 1}}),
        json!({"seq": 7, "type": "request", "command": "disconnect", "arguments": {}}),
    ];
    let mut child = optive()
        .arg("dap")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn DAP");
    {
        let stdin = child.stdin.as_mut().expect("DAP stdin");
        for request in &requests {
            stdin.write_all(&frame(request)).expect("write DAP frame");
        }
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("wait for DAP");
    assert!(
        output.status.success(),
        "DAP failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut reader = std::io::Cursor::new(&output.stdout);
    let mut messages = Vec::new();
    while let Some(message) = optive::rpc::read_json(&mut reader).expect("valid DAP frame") {
        messages.push(message);
    }
    let reasons: Vec<&str> = messages
        .iter()
        .filter(|m| m["type"] == "event" && m["event"] == "stopped")
        .filter_map(|m| m["body"]["reason"].as_str())
        .collect();
    assert!(
        reasons.iter().any(|r| *r == "entry"),
        "expected entry stop: {messages:#?}"
    );
    assert!(
        reasons.iter().any(|r| *r == "breakpoint") && reasons.iter().any(|r| *r == "step"),
        "expected breakpoint and step stops: {messages:#?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m["type"] == "event" && m["event"] == "terminated"),
        "expected terminated: {messages:#?}"
    );
    std::fs::remove_dir_all(&dir).expect("remove temporary directory");
}

#[test]
fn lsp_rejects_malformed_content_length_frame() {
    let mut child = optive()
        .arg("lsp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn LSP");
    child
        .stdin
        .as_mut()
        .expect("LSP stdin")
        .write_all(b"Content-Length: nope\r\n\r\n{}")
        .expect("write malformed frame");
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("wait for LSP");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("invalid Content-Length"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
