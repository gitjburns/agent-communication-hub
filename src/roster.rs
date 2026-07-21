//! Agent roster loader: the hub's `name -> token` authentication map.
//!
//! The roster is credential material, kept in its own operator-owned TOML
//! file (mode 600, never committed) rather than in `config.toml`. It is as
//! strict as the config: a missing file, unparseable content, keys outside
//! the `[agents]` table, an empty roster, or an empty name or token is a
//! fatal startup error — a hub with no verifiable identities must not
//! start, because authenticated `from` is identity, not a claim.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The parsed roster. Tokens live in memory for the process lifetime;
/// they must never be logged or echoed — diagnostics may name agents and
/// counts only.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Roster {
    /// Agent name -> token. Arbitrary names are valid keys here; strictness
    /// against unknown *structure* is enforced by `deny_unknown_fields` on
    /// the outer shape (only the `[agents]` table may exist).
    pub agents: HashMap<String, String>,
}

/// Why roster loading failed. Variants keep the roster path and local
/// facts (offending agent name where applicable) so startup evidence is
/// specific without ever including a token value.
#[derive(Debug)]
pub enum RosterError {
    /// The roster file could not be read (missing file, permission
    /// denied, ...).
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The roster file is not a valid strict `Roster`: syntax error,
    /// wrong types, or keys outside the `[agents]` table.
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    /// The `[agents]` table exists but contains no entries. A hub that can
    /// authenticate nobody is misconfigured, not idle.
    Empty { path: PathBuf },
    /// An entry has an empty name or empty token, which would make an
    /// identity unauthenticatable or trivially forgeable.
    BlankEntry { path: PathBuf, name: String },
}

impl fmt::Display for RosterError {
    // Operator-facing startup evidence: names the file and the exact
    // defect. Token values are deliberately never part of any message.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RosterError::Read { path, source } => {
                write!(f, "cannot read roster file {}: {}", path.display(), source)
            }
            RosterError::Parse { path, source } => {
                write!(f, "invalid roster file {}: {}", path.display(), source)
            }
            RosterError::Empty { path } => {
                write!(
                    f,
                    "roster file {} has an empty [agents] table; at least one agent is required",
                    path.display()
                )
            }
            RosterError::BlankEntry { path, name } => {
                write!(
                    f,
                    "roster file {} has an entry with an empty name or empty token (name: {:?})",
                    path.display(),
                    name
                )
            }
        }
    }
}

impl std::error::Error for RosterError {
    // Exposes the underlying io/toml error for callers that walk sources.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RosterError::Read { source, .. } => Some(source),
            RosterError::Parse { source, .. } => Some(source),
            RosterError::Empty { .. } | RosterError::BlankEntry { .. } => None,
        }
    }
}

/// Reads and strictly parses the roster file at `path`, then validates
/// that it is usable: non-empty, with no blank names or tokens. Purely
/// synchronous; performs no logging so the caller owns the diagnostic
/// boundary.
pub fn load(path: &Path) -> Result<Roster, RosterError> {
    let text = std::fs::read_to_string(path).map_err(|source| RosterError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let roster: Roster = toml::from_str(&text).map_err(|source| RosterError::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    if roster.agents.is_empty() {
        return Err(RosterError::Empty {
            path: path.to_path_buf(),
        });
    }
    // Blank names or tokens parse fine as TOML but are operationally
    // meaningless credentials; reject them at startup rather than at the
    // first confusing auth failure.
    for (name, token) in &roster.agents {
        if name.is_empty() || token.is_empty() {
            return Err(RosterError::BlankEntry {
                path: path.to_path_buf(),
                name: name.clone(),
            });
        }
    }
    Ok(roster)
}
