//! Minimal ACP echo agent for debugging a client: answers `initialize` and echoes other params.
//! Reads Content-Length framed or single-line JSON-RPC on stdin.

use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

const ACP_PROTOCOL_VERSION: i64 = 1;

fn handle(request: &Value) -> Option<Value> {
    let method = request.as_object()?.get("method")?.as_str()?;
    let result = if method == "initialize" {
        json!({
            "protocolVersion": ACP_PROTOCOL_VERSION,
            "serverInfo": { "name": "EchoAgent", "version": "0.0.1" },
        })
    } else {
        json!({ "echo": request.get("params") })
    };
    Some(json!({ "jsonrpc": "2.0", "id": request.get("id"), "result": result }))
}

/// Next message body, or `None` at end of input.
fn read_framed(input: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let Some(length) = line.trim_end().strip_prefix("Content-Length") else {
        return Ok(Some(line.trim_end().to_string()));
    };
    let length = length.trim_start_matches(':').trim().parse().unwrap_or(0);
    input.read_line(&mut String::new())?;
    let mut body = vec![0; length];
    input.read_exact(&mut body)?;
    Ok(Some(String::from_utf8_lossy(&body).into_owned()))
}

fn serve(mut input: impl BufRead, mut output: impl Write) -> io::Result<()> {
    while let Some(message) = read_framed(&mut input)? {
        let response = serde_json::from_str(&message).ok().and_then(|request| handle(&request));
        if let Some(response) = response {
            let payload = response.to_string();
            write!(output, "Content-Length: {}\n\n{payload}\n", payload.len())?;
            output.flush()?;
        }
    }
    Ok(())
}

fn main() -> io::Result<()> {
    serve(io::stdin().lock(), io::stdout().lock())
}

#[cfg(test)]
mod tests {
    #[test]
    fn framed_initialize_and_line_requests_get_framed_replies_and_garbage_is_skipped() {
        let body = r#"{"jsonrpc":"2.0","id":7,"method":"initialize"}"#;
        let input = format!(
            "Content-Length: {}\n\n{body}not json\n\n{}\n",
            body.len(),
            r#"{"id":"a","method":"say","params":{"x":1}}"#
        );
        let mut output = Vec::new();
        super::serve(input.as_bytes(), &mut output).unwrap();
        let first = r#"{"id":7,"jsonrpc":"2.0","result":{"protocolVersion":1,"serverInfo":{"name":"EchoAgent","version":"0.0.1"}}}"#;
        let second = r#"{"id":"a","jsonrpc":"2.0","result":{"echo":{"x":1}}}"#;
        assert_eq!(
            String::from_utf8(output).unwrap(),
            format!(
                "Content-Length: {}\n\n{first}\nContent-Length: {}\n\n{second}\n",
                first.len(),
                second.len()
            )
        );
    }
}
