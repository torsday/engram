//! Integration test: spawn the real `engram serve --mcp-stdio` binary,
//! drive it with raw JSON-RPC over stdin/stdout, and assert the
//! `initialize` → `tools/list` round-trip.
//!
//! This exercises the full CLI wiring (flag parsing, config loading,
//! `serve_stdio` call) that unit tests cannot reach.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Path to the compiled `engram` binary under test.
fn engram_bin() -> PathBuf {
    // `cargo test` sets CARGO_BIN_EXE_engram when the binary is declared.
    // Fall back to the typical debug-build location.
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_engram") {
        return PathBuf::from(p);
    }
    let mut p = std::env::current_exe().expect("current_exe");
    p.pop(); // remove test binary name
    p.pop(); // remove "deps"
    p.push("engram");
    p
}

fn send_line(stdin: &mut impl Write, msg: &str) {
    stdin
        .write_all(msg.as_bytes())
        .expect("write to engram stdin");
    stdin.write_all(b"\n").expect("write newline");
    stdin.flush().expect("flush");
}

/// Read lines from the server until we get one that is non-empty and
/// looks like a JSON-RPC *response* (has an `"id"` field). Skips blank
/// lines and server-sent notifications (which have `"method"` but no `"id"`).
fn recv_response(reader: &mut impl BufRead) -> serde_json::Value {
    loop {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .expect("read from engram stdout");
        if n == 0 {
            panic!("engram stdout closed before receiving a response");
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let v: serde_json::Value =
            serde_json::from_str(trimmed).expect("engram stdout must be JSON");
        // Skip notifications (they have "method" but no "id").
        if v.get("id").is_some() {
            return v;
        }
    }
}

#[test]
fn mcp_stdio_initialize_and_list_tools() {
    let bin = engram_bin();
    if !bin.exists() {
        // The binary hasn't been built yet (e.g., in doc-only CI runs).
        // Skip gracefully rather than failing with a confusing error.
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
        .stderr(Stdio::null()) // tracing goes to stderr; swallow it here
        .spawn()
        .expect("spawn engram serve --mcp-stdio");

    let mut stdin = child.stdin.take().expect("child stdin");
    let stdout = child.stdout.take().expect("child stdout");
    let mut reader = BufReader::new(stdout);

    // ── initialize ────────────────────────────────────────────────────────
    send_line(
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

    // Notify initialized (no response expected from server).
    send_line(
        &mut stdin,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    );

    // ── tools/list ────────────────────────────────────────────────────────
    send_line(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    );

    let list = recv_response(&mut reader);
    assert_eq!(list["id"], 2, "id mismatch on tools/list response");
    let tools = list["result"]["tools"].as_array().expect("tools array");
    assert!(
        !tools.is_empty(),
        "expected at least one tool, got none: {list}"
    );

    // All tools in the default registry should be present.
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
            "tool {expected} missing from list: {names:?}"
        );
    }

    // Clean up: close stdin to signal EOF → server exits.
    drop(stdin);
    let _ = child.wait_timeout(Duration::from_secs(5));
}

// Minimal wait_timeout for process cleanup.
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
