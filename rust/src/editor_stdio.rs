use eframe::egui;
use serde_json::{json, Value};
use soul2d_core::mcp_http::RpcReply;
use std::io::{self, BufRead, Write};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::thread;

pub enum HostMessage {
    Rpc {
        request: Value,
        reply: SyncSender<RpcReply>,
    },
    StdinClosed,
    Failed,
}

// One JSON-RPC line at a time is in flight: the UI thread drains queued
// requests, answers them against the single owner Session, and replies over a
// bounded channel, so the stdin writer blocks until each answer is emitted.
pub fn spawn_stdin(ctx: &egui::Context) -> Receiver<HostMessage> {
    let (tx, rx) = sync_channel::<HostMessage>(1);
    let ctx = ctx.clone();
    thread::spawn(move || {
        let stdin = io::stdin();
        let stdout = io::BufWriter::new(io::stdout().lock());
        serve_lines(stdin.lock(), stdout, &tx, &ctx);
    });
    rx
}

fn serve_lines(
    input: impl BufRead,
    mut stdout: impl Write,
    tx: &SyncSender<HostMessage>,
    ctx: &egui::Context,
) {
    let mut failed = false;
    for line in input.lines() {
        let Ok(line) = line else {
            failed = true;
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(&line) {
            Err(error) => {
                let response = json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32700, "message": format!("parse error: {error}") },
                });
                if writeln!(stdout, "{response}")
                    .and_then(|()| stdout.flush())
                    .is_err()
                {
                    failed = true;
                    break;
                }
            }
            Ok(request) => {
                let (reply_tx, reply_rx) = sync_channel::<RpcReply>(1);
                if tx
                    .send(HostMessage::Rpc {
                        request,
                        reply: reply_tx,
                    })
                    .is_err()
                {
                    failed = true;
                    break;
                }
                ctx.request_repaint();
                let Ok(response) = reply_rx.recv() else {
                    failed = true;
                    break;
                };
                if response.write_line(&mut stdout).is_err() {
                    failed = true;
                    ctx.request_repaint();
                    break;
                }
                ctx.request_repaint();
            }
        }
    }
    let _ = tx.send(if failed {
        HostMessage::Failed
    } else {
        HostMessage::StdinClosed
    });
    ctx.request_repaint();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[test]
    fn remote_quit_stdio_read_failure_is_not_clean_eof() {
        struct Broken;
        impl std::io::Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { Err(io::ErrorKind::Other.into()) }
        }
        let (host, receive) = sync_channel(1);
        serve_lines(io::BufReader::new(Broken), Vec::new(), &host, &egui::Context::default());
        assert!(matches!(receive.recv().unwrap(), HostMessage::Failed));
    }

    struct Output {
        bytes: Arc<Mutex<Vec<u8>>>,
        fail_flush: bool,
    }
    impl Write for Output {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.bytes.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            if self.fail_flush {
                Err(io::ErrorKind::BrokenPipe.into())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn remote_quit_stdio_worker_distinguishes_flushed_reply_eof_and_output_failure() {
        for fail_flush in [false, true] {
            let request = json!({"jsonrpc":"2.0","id":"stdio-quit","method":"tools/call"});
            let response = json!({"jsonrpc":"2.0","id":"stdio-quit","result":{"isError":false}});
            let input = io::Cursor::new(format!("{request}\n"));
            let bytes = Arc::new(Mutex::new(Vec::new()));
            let output = Output {
                bytes: bytes.clone(),
                fail_flush,
            };
            let (host, receive) = sync_channel(1);
            let worker = std::thread::spawn(move || {
                serve_lines(input, output, &host, &egui::Context::default())
            });
            let HostMessage::Rpc {
                request: actual,
                reply,
            } = receive.recv_timeout(Duration::from_secs(5)).unwrap()
            else {
                panic!("RPC expected")
            };
            assert_eq!(actual, request);
            let (sent, delivery) = sync_channel(1);
            reply
                .send(RpcReply::tracked(&actual, Some(response.clone()), sent))
                .ok()
                .unwrap();
            assert_eq!(
                delivery
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .is_ok(),
                !fail_flush
            );
            let terminal = receive.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(matches!(terminal, HostMessage::Failed), fail_flush);
            assert_eq!(matches!(terminal, HostMessage::StdinClosed), !fail_flush);
            worker.join().unwrap();
            assert_eq!(*bytes.lock().unwrap(), format!("{response}\n").as_bytes());
        }
    }
}
