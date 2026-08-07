//! nuphus-mcp standalone demo — self-contained stdio MCP client.
//!
//! Demonstrates: initialize handshake → tools/list → tools/call (desktop_screen_size →
//! browser_navigate → browser_close). No external dependencies; std-only.
//!
//! Run:
//! ```sh
//! cargo build -p nuphus-mcp
//! cargo run -p nuphus-mcp --example demo
//! ```
//!
//! Note: this demo shows how **any MCP client** can connect to nuphus-mcp (stdio protocol).
//! For how the Nuphus main app connects itself, see `src/mcp/dual.rs` in the main repo (dual-channel dogfooding).

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};

/// Self-contained minimal stdio JSON-RPC client (mirrors the McpClient protocol of the Nuphus main crate).
struct DemoClient {
    child: Child,
    reader: BufReader<std::process::ChildStdout>,
}

impl DemoClient {
    fn start(binary: &str, args: &[&str]) -> Result<Self, String> {
        let mut cmd = Command::new(binary);
        cmd.args(args);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::null()); // logs go to stderr; suppress to keep output clean
        let mut child = cmd.spawn().map_err(|e| format!("spawn failed: {}", e))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "stdout not available".to_string())?;
        Ok(Self {
            child,
            reader: BufReader::new(stdout),
        })
    }

    /// Send a single-line JSON request and read the single-line response.
    fn call(
        &mut self,
        id: u64,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let line = serde_json::to_string(&req).map_err(|e| e.to_string())?;

        let stdin = self
            .child
            .stdin
            .as_mut()
            .ok_or_else(|| "stdin not available".to_string())?;
        writeln!(stdin, "{}", line).map_err(|e| e.to_string())?;
        stdin.flush().map_err(|e| e.to_string())?;

        let mut resp_line = String::new();
        self.reader
            .read_line(&mut resp_line)
            .map_err(|e| e.to_string())?;
        let resp: serde_json::Value =
            serde_json::from_str(resp_line.trim()).map_err(|e| format!("bad JSON: {}", e))?;
        if let Some(err) = resp.get("error") {
            return Err(format!("JSON-RPC error: {}", err));
        }
        Ok(resp)
    }

    /// Send a notification (no id; server does not respond, so no response line can be read).
    fn notify(&mut self, method: &str) -> Result<(), String> {
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
        });
        let line = serde_json::to_string(&req).map_err(|e| e.to_string())?;
        let stdin = self
            .child
            .stdin
            .as_mut()
            .ok_or_else(|| "stdin not available".to_string())?;
        writeln!(stdin, "{}", line).map_err(|e| e.to_string())?;
        stdin.flush().map_err(|e| e.to_string())?;
        Ok(())
    }

    fn shutdown(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for DemoClient {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Extract the text content of a tools/call result.
fn text_of(resp: &serde_json::Value) -> String {
    resp["result"]["content"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn main() -> Result<(), String> {
    // Binary path: defaults to the same target dir (current exe dir or parent); overridable via DEMO_MCP_BIN
    let binary = std::env::var("DEMO_MCP_BIN").unwrap_or_else(|_| {
        let exe = std::env::current_exe().expect("current exe");
        let exe_dir = exe.parent().expect("exe parent");
        let name = if cfg!(windows) {
            "nuphus-mcp.exe"
        } else {
            "nuphus-mcp"
        };
        // Candidates: exe dir → exe parent dir (examples → debug)
        let mut candidates: Vec<std::path::PathBuf> = vec![exe_dir.join(name)];
        if let Some(parent) = exe_dir.parent() {
            candidates.push(parent.join(name));
        }
        candidates
            .into_iter()
            .find(|p| p.is_file())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| exe_dir.join(name).to_string_lossy().to_string())
    });
    if !std::path::Path::new(&binary).exists() {
        return Err(format!(
            "nuphus-mcp binary not found at '{}'. Run: cargo build -p nuphus-mcp",
            binary
        ));
    }

    let mut client = DemoClient::start(&binary, &[])?;
    println!("== nuphus-mcp demo (binary: {}) ==\n", binary);

    // 1. initialize handshake
    let resp = client.call(
        0,
        "initialize",
        serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "nuphus-mcp-demo", "version": "0.1.0" }
        }),
    )?;
    let info = &resp["result"]["serverInfo"];
    println!(
        "[1] initialize OK → server={} v{}, protocol={}",
        info["name"].as_str().unwrap_or("?"),
        info["version"].as_str().unwrap_or("?"),
        resp["result"]["protocolVersion"].as_str().unwrap_or("?")
    );

    // initialized notification (no response)
    client.notify("notifications/initialized")?;

    // 2. tools/list
    let resp = client.call(1, "tools/list", serde_json::json!({}))?;
    let tools = resp["result"]["tools"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let desktop_count = tools
        .iter()
        .filter(|t| t["name"].as_str().unwrap_or("").starts_with("desktop_"))
        .count();
    let browser_count = tools
        .iter()
        .filter(|t| t["name"].as_str().unwrap_or("").starts_with("browser_"))
        .count();
    let destructive = tools
        .iter()
        .filter(|t| t["annotations"]["destructiveHint"] == serde_json::json!(true))
        .count();
    println!(
        "[2] tools/list OK → {} tools (desktop {} + browser {}), {} marked destructive",
        tools.len(),
        desktop_count,
        browser_count,
        destructive
    );

    // 3. tools/call: read desktop screen resolution (real execution, via desktop-api)
    let resp = client.call(
        2,
        "tools/call",
        serde_json::json!({
            "name": "desktop_screen_size",
            "arguments": {}
        }),
    )?;
    println!("[3] desktop_screen_size → {}", text_of(&resp));

    // 4. tools/call: harmless browser operation.
    // URL scheme whitelist only allows http/https (data: was rejected by the
    // security boundary once the whitelist shipped) — use the standard example domain.
    let resp = client.call(
        3,
        "tools/call",
        serde_json::json!({
            "name": "browser_navigate",
            "arguments": { "url": "https://example.com" }
        }),
    )?;
    let nav = text_of(&resp);
    let first_line = nav
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(80)
        .collect::<String>();
    println!("[4] browser_navigate → {}", first_line);

    let resp = client.call(
        4,
        "tools/call",
        serde_json::json!({
            "name": "browser_evaluate",
            "arguments": { "script": "document.querySelector('h1').textContent" }
        }),
    )?;
    println!("[5] browser_evaluate → {}", text_of(&resp));

    let resp = client.call(
        5,
        "tools/call",
        serde_json::json!({
            "name": "browser_close",
            "arguments": {}
        }),
    )?;
    println!("[6] browser_close → {}", text_of(&resp));

    println!("\n== demo done ==");
    Ok(())
}
