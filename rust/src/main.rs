use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use soul2d_core::{mcp_http, Session};
use std::net::TcpListener;

const USAGE: &str = "usage: soul2d-core [--mcp]
       soul2d-core --mcp-port <port> [--mcp-host <host>] [--no-auth] [--project <path> | <path>]
       soul2d-core --export-command-manifest <path> [--check]

  --mcp-port <port>   serve MCP over HTTP (POST /mcp) on this port; 0 picks a free port
  --mcp-host <host>   bind address, default 127.0.0.1; a non-loopback host requires SOUL2D_MCP_BEARER
  --no-auth           serve a loopback host without a bearer token
  --project <path>    open an existing project instead of starting blank
  --export-command-manifest <path>
                      write the MCP tool manifest as JSON; with --check, exit 1 if the file differs
SOUL2D_MCP_BEARER, when set, is the bearer token every HTTP request must present. Without it (and without
--no-auth) a random token is generated and written with the listening address, readable by the owner only, to
mcp-<port>.token in the config directory, where njc finds it.
";

fn main() {
    soul2d_core::crashdump::install("soul2d-crashdump");
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [flag, path] if flag == "--export-command-manifest" => run_manifest(path, false),
        [flag, path, check] if flag == "--export-command-manifest" && check == "--check" => run_manifest(path, true),
        [check, flag, path] if check == "--check" && flag == "--export-command-manifest" => run_manifest(path, true),
        [] => run_jsonl(),
        [flag] if flag == "--mcp" => run_mcp(),
        [flag] if flag == "--help" || flag == "-h" => print!("{USAGE}"),
        _ => run_http(&args),
    }
}

fn run_manifest(path: &str, check: bool) {
    let path = std::path::Path::new(path);
    let outcome = if check { soul2d_core::manifest::check(path) } else { soul2d_core::manifest::write(path) };
    if let Err(message) = outcome {
        fail(&message, 1);
    }
}

fn fail(message: &str, code: i32) -> ! {
    eprintln!("soul2d-core: {message}");
    if code == 2 {
        eprint!("{USAGE}");
    }
    std::process::exit(code);
}

/// A session that reads the user automesh presets where the editor keeps its settings. The directory is
/// created by the first preset that is saved, not here.
fn new_session() -> Session {
    let mut session = Session::new();
    if let Ok(dir) = soul2d_core::settings::config_dir() {
        session.set_config_dir(dir);
    }
    session
}

/// Headless MCP host: serve one shared session over HTTP until the process is killed.
fn run_http(args: &[String]) {
    let mut host = "127.0.0.1".to_string();
    let mut port = None;
    let mut project: Option<String> = None;
    let mut open = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut value = |name: &str| iter.next().cloned().unwrap_or_else(|| fail(&format!("missing value for {name}"), 2));
        match arg.as_str() {
            "--help" | "-h" => return print!("{USAGE}"),
            "--mcp-port" => {
                port = Some(value(arg).parse::<u16>().unwrap_or_else(|_| fail("--mcp-port must be 0..65535", 2)));
            }
            "--mcp-host" => host = value(arg),
            "--no-auth" => open = true,
            "--project" => project = Some(value(arg)),
            flag if flag.starts_with('-') => fail(&format!("unknown option: {flag}"), 2),
            path => {
                project.get_or_insert_with(|| path.to_string());
            }
        }
    }
    let bearer = std::env::var("SOUL2D_MCP_BEARER").ok().filter(|token| !token.is_empty());
    if bearer.is_none() && !mcp_http::is_loopback_host(&host) {
        fail("refusing to serve a non-loopback address without SOUL2D_MCP_BEARER", 2);
    }
    let mut session = new_session();
    if let Some(path) = project.as_deref().map(str::trim).filter(|path| !path.is_empty()) {
        let opened = session.handle_request(&json!({ "op": "open", "path": path }));
        if opened["succeeded"] != true {
            fail(&format!("failed to open project: {path}"), 1);
        }
    }
    // D defaults to 8088 when no port is given.
    let listener = TcpListener::bind((host.as_str(), port.unwrap_or(8088)))
        .unwrap_or_else(|error| fail(&format!("cannot listen on {host}: {error}"), 1));
    let addr = listener.local_addr().unwrap_or_else(|error| fail(&format!("cannot read listen address: {error}"), 1));
    let mut token_path = None;
    let bearer = bearer.or_else(|| {
        if open {
            return None;
        }
        let token = mcp_http::random_token().unwrap_or_else(|error| fail(&error, 1));
        token_path = Some(mcp_http::write_token_file(addr, &token).unwrap_or_else(|error| fail(&error, 1)));
        Some(token)
    });
    eprintln!("headless MCP host listening on http://{addr}/mcp");
    if bearer.is_none() {
        eprintln!("warning: --no-auth: any local process or local web page can drive this session");
    }
    if let Some(path) = token_path {
        eprintln!("bearer token written to {}", path.display());
    }
    mcp_http::serve(&listener, &mut session, bearer.as_deref());
}

fn run_jsonl() {
    let stdin = io::stdin();
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    let mut session = new_session();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(request) => session.handle_request(&request),
            Err(error) => json!({
                "succeeded": false,
                "status": "error",
                "message": format!("invalid JSON: {error}"),
                "result": null,
            }),
        };
        if writeln!(stdout, "{response}")
            .and_then(|()| stdout.flush())
            .is_err()
        {
            break;
        }
    }
}

fn run_mcp() {
    let stdin = io::stdin();
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    let mut session = new_session();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(request) => session.handle_mcp(&request),
            Err(error) => Some(json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": { "code": -32700, "message": format!("parse error: {error}") },
            })),
        };
        if let Some(response) = response {
            if writeln!(stdout, "{response}")
                .and_then(|()| stdout.flush())
                .is_err()
            {
                break;
            }
        }
    }
}
