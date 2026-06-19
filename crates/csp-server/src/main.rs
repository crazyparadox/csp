//! `csp-server` — the reference Coverage Server Protocol server.
//!
//! Reads JSON-RPC over stdin and writes responses/notifications to stdout, exactly
//! like an LSP server. The workspace root defaults to the current directory and
//! can be overridden with `--root <path>`; an optional `--framework <name>` forces
//! a specific adapter (otherwise it is auto-detected).
//!
//! ```sh
//! csp-server --root /path/to/project [--framework cargo-llvm-cov|go|istanbul|lcov]
//! ```

use std::io::{self, BufReader, Write};
use std::path::PathBuf;

use csp_core::jsonrpc::{self, TransportError};
use csp_server::{Flow, Server};

fn main() {
    let mut root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut framework: Option<String> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => {
                if let Some(v) = args.next() {
                    root = PathBuf::from(v);
                }
            }
            "--framework" => framework = args.next(),
            "--help" | "-h" => {
                eprintln!("usage: csp-server [--root <path>] [--framework <name>]");
                return;
            }
            other => eprintln!("[csp-server] ignoring unknown argument: {other}"),
        }
    }

    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    // Lock stdout once and hand it to the server for the lifetime of the process.
    let stdout = io::stdout();
    let mut server = Server::new(stdout.lock(), root, framework);

    loop {
        match jsonrpc::read_message(&mut reader) {
            Ok(req) => {
                if server.handle(req) == Flow::Exit {
                    break;
                }
            }
            Err(TransportError::Closed) => break, // client hung up
            Err(e) => {
                eprintln!("[csp-server] transport error: {e}");
                break;
            }
        }
    }
    let _ = io::stdout().flush();
}
