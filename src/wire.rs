//! Shared wire line types and JSONL framing helpers.
//!
//! Every protocol line is one externally tagged JSON object per line:
//! `{"hello": {...}}`, `{"accepted": {...}}`, and so on. Both binaries
//! share these types, so client and server agree on the wire at compile
//! time. The helpers are pure string <-> type conversions: each binary
//! owns its own socket I/O (sync `std::net` in the client, tokio in the
//! server), so nothing in this module touches a stream.

use std::fmt;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::envelope::Envelope;

/// Lines a client may send to the hub. `Hello` must be the first line on
/// every connection; exactly one verb line (`Send` or `Await`) follows.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
pub enum ClientLine {
    /// Authentication opener: asserted name plus the token that must
    /// match the hub roster's entry for that name.
    Hello { name: String, token: String },
    /// The `send` verb: submit one envelope for validation and routing.
    Send(Envelope),
    /// The `await` verb: block until one matching envelope is available.
    /// The timeout rides in the request because timeout is enforced
    /// hub-side — the hub answers with a `timeout` line and consumes
    /// nothing, a guarantee a client-side socket deadline cannot make.
    #[serde(rename_all = "camelCase")]
    Await {
        /// `None` matches any envelope addressed to the authenticated
        /// name; `Some(id)` matches only envelopes whose `correlationId`
        /// equals `id`.
        reply_to: Option<String>,
        /// `None` blocks indefinitely (the CLI's `--timeout` is
        /// optional).
        timeout_secs: Option<u64>,
    },
}

/// Lines the hub may send to a client. Variants with no payload are
/// empty-struct (not unit) variants so every wire line serializes as a
/// tagged JSON object, never a bare string.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
pub enum ServerLine {
    /// Hello succeeded; the connection is in verb mode.
    Ok {},
    /// Auth, validation, or protocol failure. `message` preserves the
    /// specific cause (PRINCIPLES.md: never a generic error) and is
    /// written before the hub closes the connection.
    Error { message: String },
    /// The `send` verb's success reply: the accepted envelope's id.
    /// Acceptance means queued (or delivered), not delivered.
    Accepted { id: String },
    /// A delivered envelope, answering an `await`. Wrapped in the tag on
    /// the wire for unambiguous parsing; the client CLI unwraps it and
    /// prints the bare envelope per the spec's stdout contract.
    Envelope(Envelope),
    /// The `await` verb's hub-side timeout reply: nothing was consumed.
    Timeout {},
}

/// Why a wire line failed to serialize or parse. The offending line text
/// is deliberately not captured: a hello line contains a token, and this
/// error must stay safe to log verbatim. (`serde_json` errors carry
/// position, not input content.)
#[derive(Debug)]
pub enum WireError {
    /// A line value could not be serialized to JSON.
    Serialize { source: serde_json::Error },
    /// A received line is not valid JSON for the expected line type.
    Parse { source: serde_json::Error },
}

impl fmt::Display for WireError {
    // Distinguishes our-side serialization bugs from peer-side malformed
    // lines; the source cause carries the JSON specifics.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WireError::Serialize { source } => {
                write!(f, "cannot serialize wire line: {}", source)
            }
            WireError::Parse { source } => {
                write!(f, "invalid wire line: {}", source)
            }
        }
    }
}

impl std::error::Error for WireError {
    // Exposes the underlying serde_json error for source walks.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WireError::Serialize { source } | WireError::Parse { source } => Some(source),
        }
    }
}

/// Serializes one wire value to its JSONL form, trailing newline
/// included. JSON string escaping guarantees the payload itself contains
/// no raw newline, so the result is always exactly one line.
pub fn to_jsonl<T: Serialize>(value: &T) -> Result<String, WireError> {
    let mut line =
        serde_json::to_string(value).map_err(|source| WireError::Serialize { source })?;
    line.push('\n');
    Ok(line)
}

/// Parses one received JSONL line into the expected wire type. Accepts
/// the line with or without its trailing newline (`serde_json` permits
/// trailing whitespace).
pub fn from_jsonl<T: DeserializeOwned>(line: &str) -> Result<T, WireError> {
    serde_json::from_str(line).map_err(|source| WireError::Parse { source })
}
