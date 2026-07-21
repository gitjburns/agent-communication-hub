//! Envelope schema and validation: the message unit the hub routes.
//!
//! The envelope is the spec's normative wire schema. The hub validates the
//! fields defined here and never inspects, interprets, or transforms
//! `body`; all task/result semantics belong to the skills layer. The
//! protocol's extensibility seam is the `kind` value, not extra fields, so
//! the field set itself is strict (`deny_unknown_fields`).

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One routed message. Field names on the wire follow the spec exactly
/// (`correlationId` is camelCase); everything except `body` is validated
/// by [`Envelope::validate`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Envelope {
    /// Unique per envelope, assigned by the sender. UUIDv4 is recommended
    /// by the spec but any non-empty string is accepted.
    pub id: String,
    /// Sender identity. Always overwritten by the hub with the
    /// authenticated name before queuing or auditing — on an accepted
    /// envelope this is identity, never a client claim.
    pub from: String,
    /// Destination agent name. Unknown destinations are deliberately
    /// accepted and queued; the agent may connect later.
    pub to: String,
    /// Sender clock, RFC 3339. Informational only — the audit log's
    /// append order is the authoritative ordering. Kept as the sender's
    /// exact string (not a parsed datetime) so accepted envelopes are
    /// audited byte-for-byte without re-serialization normalization.
    pub ts: String,
    /// Message kind: `task` or `result` in v1. Extensible — the hub
    /// routes unknown kinds without complaint, so no value check here.
    pub kind: String,
    /// `None`, or the `id` of the envelope this one answers.
    pub correlation_id: Option<String>,
    /// Opaque payload. The hub never inspects, interprets, or transforms
    /// it.
    pub body: Value,
}

/// Why an envelope failed validation. Variants carry only small local
/// facts that are safe for the service log and client error lines;
/// `body` content is never included.
#[derive(Debug)]
pub enum EnvelopeError {
    /// `id` is empty; every envelope needs a sender-assigned unique id.
    EmptyId,
    /// `to` is empty; a destination queue cannot be named.
    EmptyTo,
    /// `kind` is empty. Unknown kinds route fine, but an absent kind is
    /// a malformed envelope, not an extension.
    EmptyKind,
    /// `correlationId` was present but empty — it must be `null` or a
    /// real envelope id.
    EmptyCorrelationId,
    /// `ts` is not a parseable RFC 3339 timestamp.
    InvalidTs {
        ts: String,
        source: chrono::ParseError,
    },
}

impl fmt::Display for EnvelopeError {
    // Names the exact defective field: this text becomes the client's
    // validation error line and the service-log evidence.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EnvelopeError::EmptyId => write!(f, "envelope id is empty"),
            EnvelopeError::EmptyTo => write!(f, "envelope to is empty"),
            EnvelopeError::EmptyKind => write!(f, "envelope kind is empty"),
            EnvelopeError::EmptyCorrelationId => {
                write!(
                    f,
                    "envelope correlationId is empty; use null or an envelope id"
                )
            }
            EnvelopeError::InvalidTs { ts, source } => {
                write!(f, "envelope ts {:?} is not RFC 3339: {}", ts, source)
            }
        }
    }
}

impl std::error::Error for EnvelopeError {
    // Exposes the underlying timestamp parse error for source walks.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            EnvelopeError::InvalidTs { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl Envelope {
    /// Validates the spec-defined envelope fields: non-empty `id`, `to`,
    /// and `kind`; parseable `ts`; `correlationId` either absent or
    /// non-empty. `from` is deliberately not validated — the hub replaces
    /// it with the authenticated identity regardless of what the sender
    /// wrote. `body` is opaque and never checked.
    pub fn validate(&self) -> Result<(), EnvelopeError> {
        if self.id.is_empty() {
            return Err(EnvelopeError::EmptyId);
        }
        if self.to.is_empty() {
            return Err(EnvelopeError::EmptyTo);
        }
        if self.kind.is_empty() {
            return Err(EnvelopeError::EmptyKind);
        }
        // Any RFC 3339 offset is accepted, not only `Z`: the spec says
        // ts is informational, so parseability is the whole requirement.
        if let Err(source) = chrono::DateTime::parse_from_rfc3339(&self.ts) {
            return Err(EnvelopeError::InvalidTs {
                ts: self.ts.clone(),
                source,
            });
        }
        if matches!(&self.correlation_id, Some(id) if id.is_empty()) {
            return Err(EnvelopeError::EmptyCorrelationId);
        }
        Ok(())
    }
}
