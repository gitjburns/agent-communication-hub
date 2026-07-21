//! Service-log initialization (DIAGNOSTICS.md).
//!
//! The service log is the durable diagnostic record: once initialized,
//! every meaningful lifecycle boundary and error path must leave evidence
//! there. The subscriber installed here also mirrors every event to
//! stdout so the operator can watch the hub live; the file remains the
//! authoritative record and the console never substitutes for it. This
//! module only installs the logger; the channel rule for the pre-logger
//! boundary (fatal startup errors that can only reach stderr) is owned by
//! the server's `main`.

use std::fmt;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use tracing_appender::rolling::{InitError, RollingFileAppender, Rotation};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::{SubscriberInitExt, TryInitError};

/// Why service-log initialization failed. This failure happens at the
/// pre-logger boundary, so its `Display` output on stderr is the only
/// evidence the operator gets — it must name the path and cause.
#[derive(Debug)]
pub enum ServiceLogError {
    /// The configured path has no file-name component (e.g. ends in a
    /// separator), so no log file could be named.
    InvalidPath { path: PathBuf },
    /// The append-only file writer could not be created (missing
    /// directory, permission denied, ...).
    Appender { path: PathBuf, source: InitError },
    /// A global tracing subscriber was already installed. Reaching this
    /// is a programming error: `init` must be called exactly once.
    Subscriber { source: TryInitError },
}

impl fmt::Display for ServiceLogError {
    // Operator-facing startup evidence on stderr; names path and cause.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServiceLogError::InvalidPath { path } => {
                write!(
                    f,
                    "service log path {} has no file name component",
                    path.display()
                )
            }
            ServiceLogError::Appender { path, source } => {
                write!(f, "cannot open service log {}: {}", path.display(), source)
            }
            ServiceLogError::Subscriber { source } => {
                write!(f, "cannot install service logger: {}", source)
            }
        }
    }
}

impl std::error::Error for ServiceLogError {
    // Exposes the underlying appender/subscriber error for source walks.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ServiceLogError::InvalidPath { .. } => None,
            ServiceLogError::Appender { source, .. } => Some(source),
            ServiceLogError::Subscriber { source } => Some(source),
        }
    }
}

/// Installs the global tracing subscriber writing every event to both
/// the service log file at `path` (created if needed, always appending)
/// and stdout.
///
/// The file layer is the durable authoritative record (DIAGNOSTICS.md);
/// the stdout layer is a live operator mirror that repeats the same
/// facts and never substitutes for the file. One shared INFO filter
/// keeps the two views identical event-for-event.
///
/// The file writer is deliberately synchronous (no `tracing_appender`
/// non-blocking worker): the non-blocking channel drops lines under
/// pressure and loses its tail on a crash, which violates the durable-
/// evidence rule. At this service's log volume, per-event file writes are
/// the simpler and safer trade.
pub fn init(path: &Path) -> Result<(), ServiceLogError> {
    // The rolling appender addresses files as directory + file name, so
    // split the configured path; an empty parent means the current
    // directory.
    let file_name = path
        .file_name()
        .ok_or_else(|| ServiceLogError::InvalidPath {
            path: path.to_path_buf(),
        })?;
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };

    // Rotation::NEVER with only a filename prefix yields exactly the
    // configured file name, appended forever.
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::NEVER)
        .filename_prefix(file_name.to_string_lossy())
        .build(directory)
        .map_err(|source| ServiceLogError::Appender {
            path: path.to_path_buf(),
            source,
        })?;

    // ANSI colors are terminal decoration: they would corrupt the file
    // record, so the file layer never emits them, and the stdout mirror
    // emits them only when stdout is actually a terminal (a redirected
    // stdout is a file record too). Level is fixed at INFO for both
    // layers: DIAGNOSTICS.md coverage is written at info/warn/error, and
    // an environment-driven filter would be an undocumented behavior
    // override.
    tracing_subscriber::registry()
        .with(LevelFilter::INFO)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(appender)
                .with_ansi(false),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stdout)
                .with_ansi(std::io::stdout().is_terminal()),
        )
        .try_init()
        .map_err(|source| ServiceLogError::Subscriber { source })?;
    Ok(())
}
