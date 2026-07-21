//! Append-only audit log: the hub-owned record of protocol events.
//!
//! The audit log's append order is the authoritative ordering of
//! cross-agent protocol events (per spec): every accepted envelope in
//! full, every delivery, every connection, every auth failure — one JSON
//! object per line. It is a separate record from the service log and the
//! two never merge: this file answers "what protocol events happened, in
//! what order"; the service log answers "what did the process do at each
//! boundary". Write-only in v1; grep is the query interface.
//!
//! Phase 3 defines the connection and auth-failure events; Phase 4 adds
//! the accepted-envelope event; Phase 5 adds the delivery event.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::envelope::Envelope;

/// One protocol event, serialized as one externally tagged JSONL line,
/// e.g. `{"connection":{...}}`. Timestamps are the hub's clock and are
/// informational; the file's append order, not `ts`, is the authoritative
/// event ordering.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditEvent {
    /// A TCP connection was accepted, before any authentication.
    Connection {
        ts: DateTime<Utc>,
        /// Peer socket address as reported by the listener.
        peer: String,
    },
    /// A hello line failed verification against the roster. Never carries
    /// the presented token.
    AuthFailure {
        ts: DateTime<Utc>,
        peer: String,
        /// The name the client asserted — not an authenticated identity.
        name: String,
        reason: AuthFailureReason,
    },
    /// A send verb's envelope was accepted: validated, `from`-stamped, and
    /// queued. Carries the envelope in full per spec — `body` included —
    /// with `from` already stamped to the authenticated identity, so this
    /// record shows what was queued, never what the client claimed.
    Accepted {
        ts: DateTime<Utc>,
        envelope: Envelope,
    },
    /// An envelope was delivered: written to a matched `await` client at
    /// the write-to-client consumption boundary. Compact by design — the
    /// envelope already appears in full in its `accepted` record; this
    /// record joins to it by `id`.
    Delivery {
        ts: DateTime<Utc>,
        /// The delivered envelope's id.
        id: String,
        /// The authenticated recipient that consumed it.
        to: String,
        /// Peer socket address of the consuming `await` connection.
        peer: String,
    },
}

/// Why authentication failed. The two cases are deliberately
/// distinguishable: errors must preserve their specific cause
/// (PRINCIPLES.md), and loopback-only transport makes the
/// name-exists oracle a negligible concern.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthFailureReason {
    /// The asserted name has no roster entry.
    UnknownName,
    /// The roster knows the name but the presented token does not match.
    TokenMismatch,
}

impl fmt::Display for AuthFailureReason {
    // Human-readable form for service-log lines and client error lines.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuthFailureReason::UnknownName => write!(f, "unknown agent name"),
            AuthFailureReason::TokenMismatch => write!(f, "token mismatch"),
        }
    }
}

/// Why an audit-log operation failed. Variants keep the audit path so the
/// diagnostic names the exact file; event content is available separately
/// to the caller and never duplicated here.
#[derive(Debug)]
pub enum AuditError {
    /// The audit file could not be opened for append (missing directory,
    /// permission denied, ...).
    Open {
        path: PathBuf,
        source: std::io::Error,
    },
    /// An event could not be serialized to JSON. Reaching this is a hub
    /// bug: `AuditEvent` contains nothing that can fail to serialize.
    Serialize { source: serde_json::Error },
    /// Appending a serialized line to the file failed.
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for AuditError {
    // Names the file and cause; this text is the service-log evidence
    // when an audit append is lost.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuditError::Open { path, source } => {
                write!(f, "cannot open audit log {}: {}", path.display(), source)
            }
            AuditError::Serialize { source } => {
                write!(f, "cannot serialize audit event: {}", source)
            }
            AuditError::Write { path, source } => {
                write!(
                    f,
                    "cannot append to audit log {}: {}",
                    path.display(),
                    source
                )
            }
        }
    }
}

impl std::error::Error for AuditError {
    // Exposes the underlying io/serde error for callers that walk sources.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AuditError::Open { source, .. } | AuditError::Write { source, .. } => Some(source),
            AuditError::Serialize { source } => Some(source),
        }
    }
}

/// The audit log writer: sole owner of the audit file handle for the
/// process lifetime. The server wraps it in a mutex so concurrent
/// connections serialize their appends; each append is one complete
/// line, so the file is always parseable line-by-line.
pub struct AuditLog {
    /// Kept for error context: io errors alone do not name the file.
    path: PathBuf,
    file: File,
}

impl AuditLog {
    /// Opens the audit log at `path` for append, creating the file if
    /// absent. Purely synchronous; the caller owns the diagnostic
    /// boundary.
    pub fn open(path: &Path) -> Result<Self, AuditError> {
        let file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
            .map_err(|source| AuditError::Open {
                path: path.to_path_buf(),
                source,
            })?;
        Ok(AuditLog {
            path: path.to_path_buf(),
            file,
        })
    }

    /// Appends one event as a single JSONL line. Serialization happens
    /// before any write so a failure cannot leave a partial line. `File`
    /// is unbuffered — `write_all` hands the complete line to the OS, so
    /// no userspace buffer can strand a tail on crash; that is the
    /// "append + flush per line" contract, and why no `BufWriter` is
    /// used.
    pub fn write(&mut self, event: &AuditEvent) -> Result<(), AuditError> {
        let mut line =
            serde_json::to_string(event).map_err(|source| AuditError::Serialize { source })?;
        line.push('\n');
        self.file
            .write_all(line.as_bytes())
            .map_err(|source| AuditError::Write {
                path: self.path.clone(),
                source,
            })
    }
}
