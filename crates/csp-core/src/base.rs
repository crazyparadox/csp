//! Base types shared across CSP, kept structurally compatible with the Language
//! Server Protocol so that CSP coverage and diagnostics map onto the same editor
//! positions an LSP already uses. A CSP server can therefore run alongside an LSP
//! for the same documents without any coordinate translation.
//!
//! The one deliberate divergence from LSP is that enums serialize as lower-case
//! strings rather than integers. CSP is a fresh protocol, so we favour
//! human-readable, schema-friendly JSON; [`DiagnosticSeverity`] documents the 1:1
//! mapping back to LSP's numeric severities.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A document identifier. Like LSP, this is a URI string (e.g. `file:///abs/path`).
pub type DocumentUri = String;

/// Zero-based line/character position, identical in shape to LSP `Position`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Position {
    /// Zero-based line number.
    pub line: u32,
    /// Zero-based character offset within the line (UTF-16 code units, as in LSP).
    pub character: u32,
}

impl Position {
    pub fn new(line: u32, character: u32) -> Self {
        Self { line, character }
    }
}

/// A range between two positions, end-exclusive — identical to LSP `Range`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

impl Range {
    pub fn new(start: Position, end: Position) -> Self {
        Self { start, end }
    }

    /// Convenience for a range spanning a single whole line (0-based).
    pub fn whole_line(line: u32) -> Self {
        Self {
            start: Position::new(line, 0),
            end: Position::new(line + 1, 0),
        }
    }
}

/// A location inside a document — a URI plus a range. Mirrors LSP `Location`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Location {
    pub uri: DocumentUri,
    pub range: Range,
}

/// Identifies a text document by URI. Mirrors LSP `TextDocumentIdentifier`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TextDocumentIdentifier {
    pub uri: DocumentUri,
}

/// A versioned document identifier. The `version` lets the client and server
/// reason about staleness: coverage computed against version N is stale once the
/// document advances to N+1. Mirrors LSP `VersionedTextDocumentIdentifier`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VersionedTextDocumentIdentifier {
    pub uri: DocumentUri,
    pub version: i32,
}

/// Severity of a quality diagnostic.
///
/// Serialized as a lower-case string. The mapping to LSP's numeric
/// `DiagnosticSeverity` is: `error` → 1, `warning` → 2, `information` → 3,
/// `hint` → 4.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

/// A quality diagnostic. Structurally a subset of the LSP `Diagnostic`, carrying
/// an extra `source`/`code` so coverage-gap and flaky-test findings can be told
/// apart in the UI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Diagnostic {
    pub range: Range,
    pub severity: DiagnosticSeverity,
    pub message: String,
    /// Producer of the diagnostic, e.g. `"csp.coverage"` or `"csp.flaky"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Machine-readable code, e.g. `"uncovered-line"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}
