//! Strict operational configuration loader.
//!
//! Config is operationally significant and therefore hostile to guessing:
//! a missing file, a missing key, or an unknown key is a fatal startup
//! error, and no runtime defaults exist. Defaults live only in
//! `config.example.toml`, where the operator can see and edit them.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Operational configuration for `agent-hub-server`, parsed from
/// `config.toml`. Every field is required; `deny_unknown_fields` makes a
/// mistyped or leftover key fatal instead of silently ignored.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// TCP port bound on localhost. The bind address itself is not
    /// configurable: loopback-only is a hard v1 constraint of the spec.
    pub port: u16,
    /// Append-only JSONL audit log, written solely by the hub. Distinct
    /// from `service_log_path`; the two records never merge.
    pub audit_log_path: PathBuf,
    /// Service diagnostics log per DIAGNOSTICS.md.
    pub service_log_path: PathBuf,
    /// Agent roster TOML file mapping agent name -> token (operator-owned,
    /// mode 600, never committed).
    pub roster_path: PathBuf,
}

/// Why configuration loading failed. Each variant keeps the config path
/// and the underlying error so the startup diagnostic names the exact
/// file and cause instead of a generic "bad config".
#[derive(Debug)]
pub enum ConfigError {
    /// The config file could not be read at all (missing file, permission
    /// denied, ...).
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The config file was read but is not a valid strict `Config`:
    /// syntax error, missing key, unknown key, or wrong type.
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
}

impl fmt::Display for ConfigError {
    // Renders the failure with path and source cause, since this message is
    // the operator-facing startup evidence on stderr and in the service log.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Read { path, source } => {
                write!(f, "cannot read config file {}: {}", path.display(), source)
            }
            ConfigError::Parse { path, source } => {
                write!(f, "invalid config file {}: {}", path.display(), source)
            }
        }
    }
}

impl std::error::Error for ConfigError {
    // Exposes the underlying io/toml error for callers that walk sources.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::Read { source, .. } => Some(source),
            ConfigError::Parse { source, .. } => Some(source),
        }
    }
}

/// Reads and strictly parses the config file at `path`. Pure synchronous
/// I/O plus parsing: no defaults are applied and no environment is
/// consulted, so the returned `Config` reflects the file and nothing else.
pub fn load(path: &Path) -> Result<Config, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_str(&text).map_err(|source| ConfigError::Parse {
        path: path.to_path_buf(),
        source,
    })
}
