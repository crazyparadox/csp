//! `csp-cli` — a minimal CSP client for debugging servers without an editor.
//!
//! It spawns a `csp-server`, runs the full lifecycle (initialize → initialized →
//! collect the pushed coverage → shutdown/exit), and prints a per-file coverage
//! summary. This is the easiest way to confirm a server/adapter works against a
//! real project before wiring it into an IDE.
//!
//! ```sh
//! csp-cli --root examples/rust-sample [--framework cargo-llvm-cov] [--server <path>]
//! ```

use std::io::{BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result};
use csp_core::jsonrpc::{self, Id, JsonRpcVersion, Request, TransportError};
use csp_core::messages::method;
use serde_json::{json, Value};

fn main() -> Result<()> {
    let mut root = std::env::current_dir()?;
    let mut framework: Option<String> = None;
    let mut server_path: Option<PathBuf> = None;
    let mut rerun: Option<String> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => root = PathBuf::from(args.next().context("--root needs a value")?),
            "--framework" => framework = args.next(),
            "--server" => server_path = args.next().map(PathBuf::from),
            "--rerun" => rerun = args.next(),
            "--help" | "-h" => {
                eprintln!("usage: csp-cli [--root <path>] [--framework <name>] [--server <path>]");
                return Ok(());
            }
            other => eprintln!("[csp-cli] ignoring unknown argument: {other}"),
        }
    }

    let root = root.canonicalize().unwrap_or(root);
    let server_path = server_path.unwrap_or_else(default_server_path);

    let mut child = spawn_server(&server_path, &root, framework.as_deref())
        .with_context(|| format!("spawning server {}", server_path.display()))?;

    let mut stdin = child.stdin.take().context("server stdin unavailable")?;
    let mut stdout = BufReader::new(child.stdout.take().context("server stdout unavailable")?);

    // 1. initialize
    let root_uri = format!("file://{}", root.to_string_lossy());
    send(
        &mut stdin,
        request(
            1,
            method::INITIALIZE,
            json!({
                "rootUri": root_uri,
                "capabilities": { "coverageGutter": true, "testResults": true, "staleness": true },
                "framework": framework,
            }),
        ),
    )?;
    let init = read_until_id(&mut stdout, 1)?;
    let server_info = init["result"]["serverInfo"].as_str().unwrap_or("unknown");
    println!("connected: {server_info}");
    println!(
        "capabilities: {}",
        serde_json::to_string(&init["result"]["capabilities"]).unwrap_or_default()
    );

    // 2. initialized — the server now pushes coverage.
    send(&mut stdin, notification(method::INITIALIZED, Value::Null))?;

    // 3. drain pushes until the run finishes.
    let mut files: Vec<Value> = Vec::new();
    loop {
        let msg = match jsonrpc::read_value(&mut stdout) {
            Ok(m) => m,
            Err(TransportError::Closed) => break,
            Err(e) => return Err(anyhow::anyhow!("transport error: {e}")),
        };
        match msg["method"].as_str() {
            Some(m) if m == method::PUBLISH_COVERAGE => files.push(msg["params"].clone()),
            Some(m) if m == method::PUBLISH_TEST_RESULTS => {
                let results = msg["params"]["results"].as_array().cloned().unwrap_or_default();
                let count = |s: &str| results.iter().filter(|r| r["status"] == s).count();
                println!(
                    "tests: {} passed, {} failed, {} skipped",
                    count("pass"),
                    count("fail"),
                    count("skip"),
                );
            }
            Some(m) if m == method::RUN_STATE_CHANGED => {
                let state = msg["params"]["state"].as_str().unwrap_or("");
                if state == "finished" || state == "errored" {
                    print_report(&files, &msg["params"]["summary"]);
                    if state == "errored" {
                        eprintln!("[csp-cli] server reported run errored");
                    }
                    break;
                }
            }
            _ => {}
        }
    }

    // 3b. optional selective re-run: csp/run with a filter; expect partial results.
    if let Some(name) = rerun.as_deref() {
        println!("\nselective re-run: {name}");
        send(
            &mut stdin,
            notification(method::RUN, json!({ "filter": [name] })),
        )?;
        loop {
            let msg = match jsonrpc::read_value(&mut stdout) {
                Ok(m) => m,
                Err(_) => break,
            };
            if msg["method"] == method::PUBLISH_TEST_RESULTS {
                let results = msg["params"]["results"].as_array().cloned().unwrap_or_default();
                let partial = msg["params"]["partial"].as_bool().unwrap_or(false);
                let names: Vec<&str> = results.iter().filter_map(|r| r["name"].as_str()).collect();
                println!("  partial={partial} results: {names:?}");
                break;
            }
        }
    }

    // 4. shutdown / exit
    send(&mut stdin, request(2, method::SHUTDOWN, Value::Null))?;
    let _ = read_until_id(&mut stdout, 2);
    send(&mut stdin, notification(method::EXIT, Value::Null))?;
    let _ = child.wait();
    Ok(())
}

/// Default to a `csp-server` sibling of this executable (both land in target/<profile>/).
fn default_server_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("csp-server")))
        .unwrap_or_else(|| PathBuf::from("csp-server"))
}

fn spawn_server(path: &PathBuf, root: &PathBuf, framework: Option<&str>) -> Result<Child> {
    let mut cmd = Command::new(path);
    cmd.arg("--root").arg(root);
    if let Some(f) = framework {
        cmd.arg("--framework").arg(f);
    }
    Ok(cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?)
}

fn request(id: i64, method: &str, params: Value) -> Request {
    Request {
        jsonrpc: JsonRpcVersion,
        id: Some(Id::Number(id)),
        method: method.to_string(),
        params: Some(params),
    }
}

fn notification(method: &str, params: Value) -> Request {
    Request {
        jsonrpc: JsonRpcVersion,
        id: None,
        method: method.to_string(),
        params: Some(params),
    }
}

fn send<W: Write>(w: &mut W, msg: Request) -> Result<()> {
    jsonrpc::write_message(w, &msg).map_err(|e| anyhow::anyhow!("write: {e}"))
}

/// Read messages until a response with the given id arrives, returning it.
fn read_until_id<R: std::io::BufRead>(r: &mut R, id: i64) -> Result<Value> {
    loop {
        let msg = jsonrpc::read_value(r).map_err(|e| anyhow::anyhow!("read: {e}"))?;
        if msg["id"] == json!(id) {
            return Ok(msg);
        }
    }
}

fn print_report(files: &[Value], workspace_summary: &Value) {
    println!("\n  coverage by file");
    println!("  {:-<64}", "");
    let mut sorted: Vec<&Value> = files.iter().collect();
    sorted.sort_by_key(|f| f["uri"].as_str().unwrap_or("").to_string());
    for f in sorted {
        let uri = f["uri"].as_str().unwrap_or("?");
        let covered = f["summary"]["linesCovered"].as_u64().unwrap_or(0);
        let total = f["summary"]["linesTotal"].as_u64().unwrap_or(0);
        let pct = if total > 0 {
            covered as f64 / total as f64 * 100.0
        } else {
            100.0
        };
        let short = uri.rsplit('/').next().unwrap_or(uri);
        println!("  {pct:5.1}%  {covered:>4}/{total:<4}  {short}");
    }
    let wc = workspace_summary["linesCovered"].as_u64().unwrap_or(0);
    let wt = workspace_summary["linesTotal"].as_u64().unwrap_or(0);
    let wpct = if wt > 0 { wc as f64 / wt as f64 * 100.0 } else { 100.0 };
    println!("  {:-<64}", "");
    println!("  {wpct:5.1}%  {wc:>4}/{wt:<4}  TOTAL  ({} files)", files.len());
}
