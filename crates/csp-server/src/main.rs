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
use std::sync::mpsc;
use std::thread;

use csp_core::coverage::RunId;
use csp_core::jsonrpc::{self, Request, TransportError};
use csp_server::{Flow, RunOutcome, Server};

/// Everything the single-threaded event loop reacts to: an inbound client
/// message (from the stdin reader thread) or a finished run (from a worker).
enum Event {
    Client(Request),
    /// The stdin stream ended / errored — no more client messages will arrive.
    ClientClosed,
    RunDone(RunId, RunOutcome),
}

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

    // One channel funnels both inbound messages and worker results to the main
    // thread, which is the sole owner of the output writer and server state — so
    // a long run never blocks message handling, yet there are no locks.
    let (tx, rx) = mpsc::channel::<Event>();

    // Reader thread: blocking stdin reads, forwarded as events.
    {
        let tx = tx.clone();
        thread::spawn(move || {
            let stdin = io::stdin();
            let mut reader = BufReader::new(stdin.lock());
            loop {
                match jsonrpc::read_message(&mut reader) {
                    Ok(req) => {
                        if tx.send(Event::Client(req)).is_err() {
                            return;
                        }
                    }
                    Err(TransportError::Closed) => {
                        let _ = tx.send(Event::ClientClosed);
                        return;
                    }
                    Err(e) => {
                        eprintln!("[csp-server] transport error: {e}");
                        let _ = tx.send(Event::ClientClosed);
                        return;
                    }
                }
            }
        });
    }

    let stdout = io::stdout();
    let mut server = Server::new(stdout.lock(), root, framework);

    for event in rx {
        match event {
            Event::Client(req) => {
                if server.handle(req) == Flow::Exit {
                    break;
                }
                spawn_pending(&mut server, &tx);
            }
            Event::RunDone(run_id, result) => {
                server.apply_run_result(run_id, result);
                spawn_pending(&mut server, &tx);
            }
            Event::ClientClosed => break,
        }
    }

    let _ = io::stdout().flush();
}

/// Spawn a worker thread for each queued run; each reports back via `tx`.
fn spawn_pending<W: Write>(server: &mut Server<W>, tx: &mpsc::Sender<Event>) {
    for req in server.take_pending_runs() {
        let tx = tx.clone();
        thread::spawn(move || {
            let result = req.run();
            let _ = tx.send(Event::RunDone(req.run_id, result));
        });
    }
}
