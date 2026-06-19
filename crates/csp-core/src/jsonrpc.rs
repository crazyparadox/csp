//! JSON-RPC 2.0 message types and `Content-Length`-framed transport, identical to
//! the framing LSP uses. This is deliberately synchronous and `std::io`-only so
//! the reference server stays dependency-light; the neumann client re-implements
//! the same framing over tokio, exactly as it already does for LSP.

use std::io::{self, BufRead, Write};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A JSON-RPC request or response id. Servers must echo the request id back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    Number(i64),
    String(String),
}

/// A JSON-RPC request (has an `id`) or notification (no `id`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: JsonRpcVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Id>,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Request {
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }
}

/// A JSON-RPC response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: JsonRpcVersion,
    pub id: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ResponseError>,
}

impl Response {
    pub fn ok(id: Option<Id>, result: Value) -> Self {
        Self {
            jsonrpc: JsonRpcVersion,
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: Option<Id>, error: ResponseError) -> Self {
        Self {
            jsonrpc: JsonRpcVersion,
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// JSON-RPC error object.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResponseError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl ResponseError {
    pub fn method_not_found(method: &str) -> Self {
        Self {
            code: error_code::METHOD_NOT_FOUND,
            message: format!("method not found: {method}"),
            data: None,
        }
    }

    pub fn invalid_params(msg: impl Into<String>) -> Self {
        Self {
            code: error_code::INVALID_PARAMS,
            message: msg.into(),
            data: None,
        }
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            code: error_code::INTERNAL_ERROR,
            message: msg.into(),
            data: None,
        }
    }
}

/// Standard JSON-RPC 2.0 error codes.
pub mod error_code {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
}

/// A zero-sized type that (de)serializes as the constant string `"2.0"`, so the
/// `jsonrpc` field is impossible to get wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JsonRpcVersion;

impl Serialize for JsonRpcVersion {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str("2.0")
    }
}

impl<'de> Deserialize<'de> for JsonRpcVersion {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = String::deserialize(d)?;
        if v == "2.0" {
            Ok(JsonRpcVersion)
        } else {
            Err(serde::de::Error::custom(format!(
                "unsupported jsonrpc version: {v}"
            )))
        }
    }
}

/// Errors from the framed transport.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("malformed header: {0}")]
    Header(String),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    /// Clean end of stream — the peer closed the connection.
    #[error("connection closed")]
    Closed,
}

/// Write a serializable message with a `Content-Length` header and the required
/// blank-line separator, then flush.
pub fn write_message<W: Write, T: Serialize>(w: &mut W, msg: &T) -> Result<(), TransportError> {
    let body = serde_json::to_vec(msg)?;
    write!(w, "Content-Length: {}\r\n\r\n", body.len())?;
    w.write_all(&body)?;
    w.flush()?;
    Ok(())
}

/// Read one `Content-Length`-framed message body as raw bytes. Returns
/// [`TransportError::Closed`] on a clean EOF at a message boundary.
pub fn read_frame<R: BufRead>(r: &mut R) -> Result<Vec<u8>, TransportError> {
    let mut content_length: Option<usize> = None;

    // Read headers until the blank separator line.
    loop {
        let mut line = String::new();
        let n = r.read_line(&mut line)?;
        if n == 0 {
            // EOF. Clean only if it happens before any header of a message.
            return Err(TransportError::Closed);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break; // end of headers
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = Some(
                    value
                        .trim()
                        .parse()
                        .map_err(|_| TransportError::Header(format!("bad length: {value}")))?,
                );
            }
            // Other headers (e.g. Content-Type) are accepted and ignored.
        } else {
            return Err(TransportError::Header(trimmed.to_string()));
        }
    }

    let len =
        content_length.ok_or_else(|| TransportError::Header("missing Content-Length".into()))?;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

/// Read one framed message into a [`Request`] (a request or notification).
/// Returns [`TransportError::Closed`] on a clean EOF at a message boundary.
pub fn read_message<R: BufRead>(r: &mut R) -> Result<Request, TransportError> {
    let buf = read_frame(r)?;
    Ok(serde_json::from_slice(&buf)?)
}

/// Read one framed message as an untyped JSON value — useful for tests and for
/// peers that need to inspect a message before deciding how to decode it (a
/// request, a notification, or a response).
pub fn read_value<R: BufRead>(r: &mut R) -> Result<serde_json::Value, TransportError> {
    let buf = read_frame(r)?;
    Ok(serde_json::from_slice(&buf)?)
}
