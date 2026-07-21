//! `agent-hub`: the client CLI installed on the shared PATH.
//!
//! Implements the spec's two verbs — `send` and `await` — over a
//! synchronous `std::net::TcpStream`; nothing here depends on tokio. All
//! wire shapes and JSONL framing come from the shared lib, so client and
//! server agree on the protocol at compile time. The client adds no
//! protocol logic of its own: the server owns validation and routing, and
//! hub error messages pass through to stderr verbatim.
//!
//! Exit codes are this binary's contract with the calling skills:
//! 0 success; 1 connection, auth, validation, or protocol failure
//! (nothing queued or consumed); 2 hub-side await timeout (distinct per
//! spec: nothing consumed, the caller may re-arm).

use std::fmt;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use chrono::{SecondsFormat, Utc};
use clap::error::ErrorKind;
use clap::{Parser, Subcommand};

use agent_communication_hub::envelope::Envelope;
use agent_communication_hub::wire::{self, ClientLine, ServerLine, WireError};

/// Exit code for every failure class the spec groups together:
/// connection, auth, validation, and protocol failures. Nothing was
/// queued or consumed.
const EXIT_FAILURE: u8 = 1;

/// Exit code for the hub-side await timeout, distinct per spec so a
/// caller can tell "nothing arrived" from "something failed".
const EXIT_TIMEOUT: u8 = 2;

/// Command-line surface: the spec's two verbs with their normative flags.
#[derive(Parser)]
#[command(version, about = "Agent Communication Hub client")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// The verb this invocation performs. The spec allows exactly one verb
/// per connection, so one invocation is one short-lived connection.
#[derive(Subcommand)]
enum Command {
    /// Submit one envelope; prints the accepted envelope's id.
    Send(SendArgs),
    /// Block until one matching envelope arrives; prints the envelope.
    Await(AwaitArgs),
}

/// Flags shared by both verbs: identity, credential, and the hub's port.
/// The port is a required flag rather than a default or an environment
/// variable: the client must never silently choose which service it
/// talks to, mirroring the spec's explicit-inputs rule for tokens.
#[derive(clap::Args)]
struct ConnectionArgs {
    /// Agent name to authenticate as.
    #[arg(long = "as", value_name = "name")]
    as_name: String,
    /// Path to this agent's token file (the only token source per spec).
    #[arg(long, value_name = "path")]
    token_file: PathBuf,
    /// Hub port on 127.0.0.1 (loopback-only transport per spec).
    #[arg(long, value_name = "port")]
    port: u16,
}

/// `agent-hub send` flags per the spec's normative list.
#[derive(clap::Args)]
struct SendArgs {
    #[command(flatten)]
    conn: ConnectionArgs,
    /// Destination agent name (unknown destinations queue by design).
    #[arg(long, value_name = "agent")]
    to: String,
    /// Envelope kind: task or result in v1; unknown kinds route.
    #[arg(long, value_name = "kind")]
    kind: String,
    /// Id of the envelope this one answers; omitted means null.
    #[arg(long, value_name = "id")]
    correlation_id: Option<String>,
    /// Envelope body as inline JSON, or - to read JSON from stdin.
    #[arg(long, value_name = "json | -")]
    body: String,
}

/// `agent-hub await` flags per the spec's normative list.
#[derive(clap::Args)]
struct AwaitArgs {
    #[command(flatten)]
    conn: ConnectionArgs,
    /// Deliver only envelopes whose correlationId equals this id;
    /// omitted matches any envelope addressed to the authenticated name.
    #[arg(long, value_name = "id")]
    reply_to: Option<String>,
    /// Hub-side timeout in seconds; omitted blocks indefinitely. On
    /// timeout nothing is consumed, guaranteed by the hub.
    #[arg(long, value_name = "secs")]
    timeout: Option<u64>,
}

/// Why a client invocation failed. Display output is the user-facing
/// stderr line, so every variant embeds its specific source cause
/// (PRINCIPLES.md: never replace a specific error with a generic one).
/// Token values are structurally absent from every variant.
enum ClientError {
    /// The token file could not be read.
    TokenFile { path: PathBuf, source: io::Error },
    /// Reading the `--body -` JSON from stdin failed.
    BodyStdin { source: io::Error },
    /// The body text is not valid JSON.
    BodyNotJson { source: serde_json::Error },
    /// TCP connection to the hub failed.
    Connect { addr: SocketAddr, source: io::Error },
    /// A wire line could not be serialized, or a hub line parsed.
    Wire { source: WireError },
    /// Writing a line to the hub socket failed.
    SocketWrite { source: io::Error },
    /// Reading the hub's reply failed.
    SocketRead { source: io::Error },
    /// The hub closed the connection before its expected reply.
    HubClosed { expected: &'static str },
    /// The hub answered with its error line; the message is the hub's
    /// specific cause, passed through verbatim.
    Hub { message: String },
    /// The hub answered with a line the protocol does not allow at this
    /// point in the conversation.
    UnexpectedReply {
        expected: &'static str,
        got: &'static str,
    },
}

impl fmt::Display for ClientError {
    // Rendered as the CLI's stderr failure line; each arm names the
    // failed boundary and preserves the source cause.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientError::TokenFile { path, source } => {
                write!(f, "cannot read token file {}: {}", path.display(), source)
            }
            ClientError::BodyStdin { source } => {
                write!(f, "cannot read body from stdin: {source}")
            }
            ClientError::BodyNotJson { source } => write!(f, "body is not valid JSON: {source}"),
            ClientError::Connect { addr, source } => {
                write!(f, "cannot connect to hub at {addr}: {source}")
            }
            ClientError::Wire { source } => write!(f, "{source}"),
            ClientError::SocketWrite { source } => write!(f, "cannot write to hub: {source}"),
            ClientError::SocketRead { source } => write!(f, "cannot read hub reply: {source}"),
            ClientError::HubClosed { expected } => {
                write!(
                    f,
                    "hub closed the connection before replying (expected {expected})"
                )
            }
            ClientError::Hub { message } => write!(f, "hub error: {message}"),
            ClientError::UnexpectedReply { expected, got } => {
                write!(f, "protocol violation: expected {expected}, hub sent {got}")
            }
        }
    }
}

/// Parses the CLI and runs the requested verb, mapping every failure to
/// the exit-code contract. clap's default usage-error exit code (2)
/// would collide with the timeout code, so parsing goes through
/// `try_parse` and usage errors exit 1 like every other validation
/// failure; --help and --version remain successful outputs.
fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            // clap routes help/version to stdout and usage errors to
            // stderr; `print` picks the stream by kind. A failed print
            // has no better channel to report to, hence the discard.
            let kind = error.kind();
            let _ = error.print();
            return if kind == ErrorKind::DisplayHelp || kind == ErrorKind::DisplayVersion {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(EXIT_FAILURE)
            };
        }
    };
    let result = match cli.command {
        Command::Send(args) => run_send(args),
        Command::Await(args) => run_await(args),
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("agent-hub: {error}");
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

/// Runs the send verb: assembles the envelope, submits it, prints the
/// accepted id. The client does not pre-validate the envelope — the
/// server owns validation, and its specific rejection reaches stderr
/// through the hub error passthrough.
fn run_send(args: SendArgs) -> Result<ExitCode, ClientError> {
    // Body is resolved first so malformed JSON fails fast with no
    // connection made (exit nonzero, nothing queued — per spec).
    let body = read_body(&args.body)?;
    let envelope = Envelope {
        // Sender-assigned unique id, UUIDv4 as the spec recommends; it
        // doubles as the printed acceptance receipt below.
        id: uuid::Uuid::new_v4().to_string(),
        // Informational: the hub restamps `from` with the authenticated
        // identity regardless of this value.
        from: args.conn.as_name.clone(),
        to: args.to,
        // Sender clock, informational only; the audit log's append order
        // is the authoritative ordering.
        ts: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        kind: args.kind,
        correlation_id: args.correlation_id,
        body,
    };
    let mut hub = connect_and_authenticate(&args.conn)?;
    hub.write_line(&ClientLine::Send(envelope))?;
    match hub.read_reply("accepted")? {
        // Acceptance means queued (or delivered), not delivered.
        ServerLine::Accepted { id } => {
            println!("{id}");
            Ok(ExitCode::SUCCESS)
        }
        line => Err(reply_error("accepted", line)),
    }
}

/// Runs the await verb: blocks until the hub delivers one matching
/// envelope (printed bare on stdout per the spec's contract) or answers
/// with its hub-side timeout (distinct exit code, nothing consumed).
fn run_await(args: AwaitArgs) -> Result<ExitCode, ClientError> {
    let mut hub = connect_and_authenticate(&args.conn)?;
    hub.write_line(&ClientLine::Await {
        reply_to: args.reply_to,
        timeout_secs: args.timeout,
    })?;
    match hub.read_reply("envelope or timeout")? {
        // The wire wraps the envelope in its tag for unambiguous
        // parsing; stdout carries the bare envelope per spec.
        ServerLine::Envelope(envelope) => {
            let jsonl = wire::to_jsonl(&envelope).map_err(|source| ClientError::Wire { source })?;
            // to_jsonl includes the trailing newline, so print! avoids a
            // doubled one.
            print!("{jsonl}");
            Ok(ExitCode::SUCCESS)
        }
        ServerLine::Timeout {} => Ok(ExitCode::from(EXIT_TIMEOUT)),
        line => Err(reply_error("envelope or timeout", line)),
    }
}

/// One authenticated connection in verb mode. Reader and writer are two
/// handles to the same OS-level socket: `BufReader` needs ownership, so
/// it wraps a cloned handle while writes go to the original.
struct HubConnection {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

impl HubConnection {
    /// Serializes and writes one client line. `TcpStream` is unbuffered,
    /// so a successful `write_all` has reached the socket; no flush
    /// applies.
    fn write_line(&mut self, line: &ClientLine) -> Result<(), ClientError> {
        let jsonl = wire::to_jsonl(line).map_err(|source| ClientError::Wire { source })?;
        self.writer
            .write_all(jsonl.as_bytes())
            .map_err(|source| ClientError::SocketWrite { source })
    }

    /// Reads and parses the hub's next line, blocking as long as needed
    /// (an await without a timeout blocks indefinitely by design). The
    /// hub is trusted, so no line-length cap applies here — unlike the
    /// server, whose peers are unauthenticated. `expected` names the
    /// awaited reply in EOF diagnostics.
    fn read_reply(&mut self, expected: &'static str) -> Result<ServerLine, ClientError> {
        let mut line = String::new();
        let bytes = self
            .reader
            .read_line(&mut line)
            .map_err(|source| ClientError::SocketRead { source })?;
        // Zero bytes is EOF: the hub closed without the expected reply.
        if bytes == 0 {
            return Err(ClientError::HubClosed { expected });
        }
        wire::from_jsonl(&line).map_err(|source| ClientError::Wire { source })
    }
}

/// Connects to the hub on loopback and performs the hello exchange,
/// returning the connection in verb mode. An auth refusal surfaces as
/// the hub's own error message via the hub error passthrough.
fn connect_and_authenticate(conn: &ConnectionArgs) -> Result<HubConnection, ClientError> {
    let token = read_token(&conn.token_file)?;
    // 127.0.0.1 explicitly, matching the server's bind: resolving
    // "localhost" can yield a non-loopback or IPv6 address.
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, conn.port));
    let stream =
        TcpStream::connect(addr).map_err(|source| ClientError::Connect { addr, source })?;
    // The reader's handle is a clone of the same socket; a clone failure
    // is an OS descriptor problem, reported like a connect failure.
    let read_half = stream
        .try_clone()
        .map_err(|source| ClientError::Connect { addr, source })?;
    let mut hub = HubConnection {
        reader: BufReader::new(read_half),
        writer: stream,
    };
    hub.write_line(&ClientLine::Hello {
        name: conn.as_name.clone(),
        token,
    })?;
    match hub.read_reply("ok")? {
        ServerLine::Ok {} => Ok(hub),
        line => Err(reply_error("ok", line)),
    }
}

/// Reads the agent token, trimming trailing whitespace: token files
/// conventionally end in a newline, while the hub compares tokens
/// exactly.
fn read_token(path: &Path) -> Result<String, ClientError> {
    let raw = fs::read_to_string(path).map_err(|source| ClientError::TokenFile {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(raw.trim_end().to_string())
}

/// Resolves `--body`: `-` reads JSON from stdin to EOF, anything else is
/// inline JSON. Client-side parsing is a construction necessity — the
/// envelope carries a JSON value, not a string — not a duplication of
/// server validation.
fn read_body(spec: &str) -> Result<serde_json::Value, ClientError> {
    let text = if spec == "-" {
        io::read_to_string(io::stdin()).map_err(|source| ClientError::BodyStdin { source })?
    } else {
        spec.to_string()
    };
    serde_json::from_str(&text).map_err(|source| ClientError::BodyNotJson { source })
}

/// Maps a reply that was not the expected line to the right failure: the
/// hub's own error line passes through verbatim; anything else is a
/// protocol violation named by both expected and received line kinds.
fn reply_error(expected: &'static str, line: ServerLine) -> ClientError {
    match line {
        ServerLine::Error { message } => ClientError::Hub { message },
        other => ClientError::UnexpectedReply {
            expected,
            got: server_line_kind(&other),
        },
    }
}

/// Names a server line for protocol-violation diagnostics without
/// echoing its payload.
fn server_line_kind(line: &ServerLine) -> &'static str {
    match line {
        ServerLine::Ok {} => "ok",
        ServerLine::Error { .. } => "error",
        ServerLine::Accepted { .. } => "accepted",
        ServerLine::Envelope(_) => "envelope",
        ServerLine::Timeout {} => "timeout",
    }
}
