//! JSON-RPC messages framed with `Content-Length` headers, as LSP sends them over stdio.

use std::io::{self, BufRead, Write};

use serde_json::{Value, json};

/// Reads one message; `None` at the end of the stream.
pub fn read(input: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut length: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        if input.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            if length.is_some() {
                break;
            }
            // Tolerate blank lines between messages.
            continue;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = Some(value.trim().parse().map_err(|_| invalid(format!("invalid Content-Length `{}`", value.trim())))?);
        }
    }
    let mut body = vec![0; length.unwrap_or_default()];
    input.read_exact(&mut body)?;
    serde_json::from_slice(&body).map(Some).map_err(|e| invalid(format!("invalid JSON-RPC message: {e}")))
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Writes one message.
pub fn write(output: &mut impl Write, message: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(message)?;
    write!(output, "Content-Length: {}\r\n\r\n", body.len())?;
    output.write_all(&body)?;
    output.flush()
}

/// What a message is, by the fields JSON-RPC defines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Request,
    Notification,
    Response,
}

pub fn kind(message: &Value) -> Kind {
    match (message.get("method").is_some(), message.get("id").is_some()) {
        (true, true) => Kind::Request,
        (true, false) => Kind::Notification,
        (false, _) => Kind::Response,
    }
}

pub fn method(message: &Value) -> &str {
    message.get("method").and_then(Value::as_str).unwrap_or_default()
}

pub fn response(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub fn error(id: &Value, code: i64, message: impl Into<String>) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.into() } })
}

pub fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

pub fn request(id: &str, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

/// The JSON-RPC error code of an unknown method.
pub const METHOD_NOT_FOUND: i64 = -32601;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_messages() {
        let mut buf = Vec::new();
        write(&mut buf, &notification("initialized", json!({}))).unwrap();
        write(&mut buf, &response(&json!(1), json!("é"))).unwrap();
        let mut input = io::Cursor::new(buf);
        let first = read(&mut input).unwrap().unwrap();
        assert_eq!(kind(&first), Kind::Notification);
        assert_eq!(method(&first), "initialized");
        let second = read(&mut input).unwrap().unwrap();
        assert_eq!(kind(&second), Kind::Response);
        assert_eq!(second["result"], "é");
        assert!(read(&mut input).unwrap().is_none());
    }

    #[test]
    fn accepts_extra_headers_and_rejects_garbage() {
        let body = r#"{"jsonrpc":"2.0","id":"a","method":"x"}"#;
        let text = format!("content-length: {}\r\nContent-Type: application/vscode-jsonrpc\r\n\r\n{body}", body.len());
        let message = read(&mut io::Cursor::new(text)).unwrap().unwrap();
        assert_eq!(kind(&message), Kind::Request);
        assert!(read(&mut io::Cursor::new("Content-Length: 2\r\n\r\n{x")).is_err());
        assert!(read(&mut io::Cursor::new("Content-Length: x\r\n\r\n")).is_err());
    }
}
