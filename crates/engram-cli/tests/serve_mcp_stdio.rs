//! Integration test: spawn the real `engram serve --mcp-stdio` binary,
//! drive it with raw JSON-RPC over stdin/stdout, and assert the
//! `initialize` → `tools/list` round-trip.
//!
//! This exercises the full CLI wiring (flag parsing, config loading,
//! `serve_stdio` call) that unit tests cannot reach.
//!
//! Wire protocol: rmcp uses newline-delimited JSON over stdio.
//! Each message is a single JSON object followed by `\n`.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Path to the compiled `engram` binary under test.
fn engram_bin() -> PathBuf {
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_engram") {
        return PathBuf::from(p);
    }
    let mut p = std::env::current_exe().expect("current_exe");
    p.pop(); // test binary name
    p.pop(); // "deps"
    p.push("engram");
    p
}

/// Send a JSON-RPC message as a newline-terminated line.
fn send_msg(stdin: &mut impl Write, msg: &str) {
    writeln!(stdin, "{}", msg).expect("write to engram stdin");
    stdin.flush().expect("flush");
}

/// Read one newline-delimited JSON message from the server.
/// Returns the parsed Value, skipping server-sent notifications
/// (messages that have `"method"` but no `"id"`).
fn recv_response(reader: &mut BufReader<impl std::io::Read>) -> serde_json::Value {
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).expect("read response line");
        if n == 0 {
            panic!("engram stdout closed unexpectedly");
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let v: serde_json::Value =
            serde_json::from_str(trimmed).expect("server response must be JSON");
        // Skip notifications (have "method", no "id").
        if v.get("id").is_some() {
            return v;
        }
    }
}

#[test]
fn mcp_stdio_initialize_and_list_tools() {
    let bin = engram_bin();
    if !bin.exists() {
        eprintln!("skipping: engram binary not found at {}", bin.display());
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let mut child = Command::new(&bin)
        .args([
            "serve",
            "--mcp-stdio",
            "--vault",
            tmp.path().to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn engram serve --mcp-stdio");

    let mut stdin = child.stdin.take().expect("child stdin");
    let stdout = child.stdout.take().expect("child stdout");
    let mut reader = BufReader::new(stdout);

    // ── initialize ────────────────────────────────────────────────────────
    send_msg(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"ci-test","version":"0"}}}"#,
    );

    let init = recv_response(&mut reader);
    assert_eq!(init["id"], 1, "id mismatch on initialize response");
    let caps = &init["result"]["capabilities"];
    assert!(
        !caps["tools"].is_null(),
        "tools capability not advertised: {init}"
    );

    // Notify initialized (one-way; no response expected).
    send_msg(
        &mut stdin,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    );

    // ── tools/list ────────────────────────────────────────────────────────
    send_msg(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    );

    let list = recv_response(&mut reader);
    assert_eq!(list["id"], 2, "id mismatch on tools/list response");
    let tools = list["result"]["tools"].as_array().expect("tools array");
    assert!(!tools.is_empty(), "expected at least one tool: {list}");

    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    for expected in &[
        "grep_notes",
        "read_note",
        "follow_backlinks",
        "follow_links",
        "vault_health",
    ] {
        assert!(
            names.contains(expected),
            "tool {expected} missing: {names:?}"
        );
    }

    drop(stdin);
    let _ = child.wait_timeout(Duration::from_secs(5));
}

trait WaitTimeout {
    fn wait_timeout(&mut self, dur: Duration) -> std::io::Result<Option<std::process::ExitStatus>>;
}

#[cfg(unix)]
impl WaitTimeout for std::process::Child {
    fn wait_timeout(&mut self, dur: Duration) -> std::io::Result<Option<std::process::ExitStatus>> {
        use std::time::Instant;
        let deadline = Instant::now() + dur;
        loop {
            match self.try_wait()? {
                Some(s) => return Ok(Some(s)),
                None if Instant::now() >= deadline => return Ok(None),
                None => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }
}

#[cfg(not(unix))]
impl WaitTimeout for std::process::Child {
    fn wait_timeout(
        &mut self,
        _dur: Duration,
    ) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.wait().map(Some)
    }
}
