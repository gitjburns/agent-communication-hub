//! `agent-hub-server`: the persistent hub process.
//!
//! Phase 5 scope: strict startup (config, service log, roster, audit
//! log), a loopback-only listener, per-connection hello authentication
//! against the roster, verb dispatch, the send path — validation,
//! `from`-stamping, audit, and enqueue or live handoff — and the await
//! path: queue consumption, the parked-waiter registry, hub-side
//! timeout, and delivery at the write-to-client consumption boundary.
//!
//! Async is confined to this binary's network layer per PLAN.md; parsing
//! and validation are synchronous lib code.
//!
//! Startup channel rule (DIAGNOSTICS.md): every fatal startup error goes
//! to stderr; once the service log is initialized it must also carry the
//! same error. Failures before the logger exists can only use stderr, and
//! for that boundary stderr is the authoritative evidence.

use std::collections::{HashMap, VecDeque};
use std::fmt::{self, Display};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use chrono::Utc;
use clap::Parser;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use agent_communication_hub::audit::{AuditEvent, AuditLog, AuthFailureReason};
use agent_communication_hub::envelope::Envelope;
use agent_communication_hub::roster::Roster;
use agent_communication_hub::wire::{self, ClientLine, ServerLine};
use agent_communication_hub::{config, roster, service_log};

/// Hard cap on one protocol line read from a peer. A pre-auth local
/// process must not be able to grow hub memory without bound by never
/// sending a newline; 1 MiB is far above any hello line and generous for
/// v1 envelope bodies. Phase 4 kept this cap; revisit only if a real
/// envelope body outgrows it.
const MAX_LINE_BYTES: usize = 1024 * 1024;

/// Command-line arguments. The config path is required rather than
/// defaulted: a runtime default for an operational setting would violate
/// the no-defaults config policy.
#[derive(Parser)]
#[command(version, about = "Agent Communication Hub server")]
struct Args {
    /// Path to the strict operational config file (see config.example.toml).
    #[arg(long)]
    config: PathBuf,
}

/// Routing state behind one mutex, per PLAN.md: an envelope must be
/// consumed exactly once, so every queue mutation — and, from Phase 5 on,
/// waiter matching and consumption — happens inside this single lock.
struct HubState {
    /// Destination agent name -> FIFO queue of envelopes awaiting
    /// delivery. Entries are created lazily on first send: unknown
    /// destinations are deliberately accepted and queued per spec, so no
    /// roster check applies here. Entries are removed when emptied so
    /// arbitrary destination names cannot grow the map without bound.
    queues: HashMap<String, VecDeque<Envelope>>,
    /// Agent name -> parked `await` consumers in arrival order. Matching
    /// scans in order, so among equally eligible waiters the oldest
    /// wins. Entries are removed when emptied, like `queues`.
    waiters: HashMap<String, Vec<Waiter>>,
    /// Monotonic source of waiter ids; an id lets the timeout path
    /// deregister exactly its own waiter.
    next_waiter_id: u64,
}

/// One parked `await` consumer. Registered under the state lock and
/// removed exactly once — by a matching send (which hands an envelope
/// through `tx` under that same lock) or by its own timeout path. That
/// single-lock atomicity of "removed" and "envelope sent" is what the
/// timeout race resolution relies on.
struct Waiter {
    /// Registry removal key for the timeout path.
    id: u64,
    /// `None` matches any envelope for the agent; `Some(id)` only
    /// envelopes whose `correlationId` equals `id`.
    reply_to: Option<String>,
    /// Handoff channel: sending transfers envelope ownership to the
    /// waiter's connection task, which owns the delivery boundary.
    tx: oneshot::Sender<Envelope>,
}

/// Locks the routing state, recovering from a poisoned lock. Queue and
/// registry mutations either complete or panic before mutating, so a
/// poisoned state lock holds no half-applied routing state; recovery is
/// safe and preserves all queued traffic and parked waiters.
fn lock_state(shared: &Shared) -> MutexGuard<'_, HubState> {
    match shared.state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::error!("state mutex poisoned by an earlier panic; recovering state");
            poisoned.into_inner()
        }
    }
}

/// State shared by every connection task: the roster for hello
/// verification (immutable after startup), the audit writer, whose mutex
/// serializes appends so each line lands complete, and the routing state.
///
/// Lock order rule: audit before state, and only `commit_send` holds
/// both. Every future phase must keep that order (or hold one at a time)
/// to stay deadlock-free.
struct Shared {
    roster: Roster,
    audit: Mutex<AuditLog>,
    state: Mutex<HubState>,
}

/// Reports a fatal startup error at the pre-logger boundary and exits.
/// Only stderr is available here; per DIAGNOSTICS.md this makes stderr
/// the authoritative evidence for this failure.
fn fatal_pre_logger(stage: &str, error: &dyn Display) -> ExitCode {
    eprintln!("fatal startup error ({stage}): {error}");
    ExitCode::FAILURE
}

/// Reports a fatal startup error after the service log is initialized:
/// the same error goes to both stderr and the service log, satisfying the
/// dual-channel startup rule. The tracing line also reaches stdout via
/// the console mirror layer, so an interactive terminal shows the error
/// twice; the plain stderr copy stays because DIAGNOSTICS.md makes stderr
/// the mandatory fatal-startup channel.
fn fatal(stage: &str, error: &dyn Display) -> ExitCode {
    tracing::error!(stage, error = %error, "fatal startup error");
    eprintln!("fatal startup error ({stage}): {error}");
    ExitCode::FAILURE
}

/// Runs strict startup (config, service log, roster, audit log), binds
/// the loopback listener, and serves connections until the process is
/// killed. Uptime and shutdown are the operator's guarantee per spec;
/// there is no signal handling in v1.
#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();

    // Pre-logger liveness: the service log path comes from config, so no
    // subscriber exists yet and this one plain line is the only way the
    // console shows the process is alive. Every later startup and runtime
    // fact reaches stdout through the subscriber's stdout mirror layer.
    println!(
        "starting agent-hub-server; config: {}",
        args.config.display()
    );

    // Config load precedes the logger by necessity: the service log path
    // itself comes from config. Failures here are stderr-only.
    let config = match config::load(&args.config) {
        Ok(config) => config,
        Err(error) => return fatal_pre_logger("load config", &error),
    };

    if let Err(error) = service_log::init(&config.service_log_path) {
        return fatal_pre_logger("initialize service log", &error);
    }

    // The service log exists from here on; mirror startup facts that so
    // far were known only to the process.
    tracing::info!(
        config_path = %args.config.display(),
        port = config.port,
        audit_log_path = %config.audit_log_path.display(),
        roster_path = %config.roster_path.display(),
        "startup: config loaded, service log initialized"
    );

    let roster = match roster::load(&config.roster_path) {
        Ok(roster) => roster,
        Err(error) => return fatal("load roster", &error),
    };

    // Credential publication status: names and count are safe identifiers;
    // token values must never reach any log.
    let mut agent_names: Vec<&str> = roster.agents.keys().map(String::as_str).collect();
    agent_names.sort_unstable();
    tracing::info!(
        agent_count = agent_names.len(),
        agents = ?agent_names,
        "startup: roster loaded"
    );

    let audit_log = match AuditLog::open(&config.audit_log_path) {
        Ok(audit_log) => audit_log,
        Err(error) => return fatal("open audit log", &error),
    };
    tracing::info!(
        audit_log_path = %config.audit_log_path.display(),
        "startup: audit log open for append"
    );

    let shared = Arc::new(Shared {
        roster,
        audit: Mutex::new(audit_log),
        state: Mutex::new(HubState {
            queues: HashMap::new(),
            waiters: HashMap::new(),
            next_waiter_id: 0,
        }),
    });

    // Loopback-only is a hard spec constraint; 127.0.0.1 is used
    // explicitly rather than resolving "localhost", which can yield a
    // non-loopback or IPv6 address depending on the host resolver.
    let bind_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, config.port));
    tracing::info!(%bind_addr, "startup: binding listener");
    let listener = match TcpListener::bind(bind_addr).await {
        Ok(listener) => listener,
        Err(error) => return fatal("bind listener", &error),
    };
    tracing::info!(%bind_addr, "startup: listener bound; hub ready");

    loop {
        match listener.accept().await {
            Ok((stream, peer)) => spawn_connection(Arc::clone(&shared), stream, peer),
            // A failed accept affects one incoming connection, not the
            // listener; log it and keep serving.
            Err(error) => tracing::warn!(error = %error, "accept failed"),
        }
    }
}

/// Spawns the per-connection task plus a monitor task awaiting its
/// handle: a fire-and-forget spawn cannot surface panics, and
/// DIAGNOSTICS.md requires panic/cancellation evidence for every spawned
/// task. Normal completion evidence is the handler's own "connection
/// closed" log.
fn spawn_connection(shared: Arc<Shared>, stream: TcpStream, peer: SocketAddr) {
    let handle = tokio::spawn(handle_connection(shared, stream, peer));
    tokio::spawn(async move {
        if let Err(join_error) = handle.await {
            tracing::error!(%peer, error = %join_error, "connection task panicked or was cancelled");
        }
    });
}

/// Brackets one connection's lifetime: accepted log plus connection audit
/// event, the conversation itself, then the closed log with elapsed time.
async fn handle_connection(shared: Arc<Shared>, stream: TcpStream, peer: SocketAddr) {
    let started = Instant::now();
    tracing::info!(%peer, "connection accepted");
    audit_append(
        &shared,
        AuditEvent::Connection {
            ts: Utc::now(),
            peer: peer.to_string(),
        },
    )
    .await;
    serve_connection(&shared, stream, peer).await;
    tracing::info!(%peer, elapsed_ms = started.elapsed().as_millis() as u64, "connection closed");
}

/// Drives one conversation: hello authentication, then exactly one verb
/// (spec: connections are short-lived and disposable). Every terminating
/// path logs its own boundary evidence; the caller owns the
/// accepted/closed bracket around this function.
async fn serve_connection(shared: &Arc<Shared>, stream: TcpStream, peer: SocketAddr) {
    let (read_half, mut writer) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // Hello boundary. The raw line may contain a token, so failures here
    // must never echo line content — ReadLineError and WireError both
    // guarantee that by construction.
    let hello_line = match read_line_bounded(&mut reader).await {
        Ok(Some(line)) => line,
        Ok(None) => {
            tracing::info!(%peer, "peer closed before sending hello");
            return;
        }
        Err(error) => {
            tracing::warn!(%peer, error = %error, "failed reading hello line");
            let message = format!("invalid hello line: {error}");
            write_line(&mut writer, peer, &ServerLine::Error { message }).await;
            return;
        }
    };
    let hello: ClientLine = match wire::from_jsonl(&hello_line) {
        Ok(line) => line,
        Err(error) => {
            tracing::warn!(%peer, error = %error, "hello line is not a valid wire line");
            let message = error.to_string();
            write_line(&mut writer, peer, &ServerLine::Error { message }).await;
            return;
        }
    };
    let (name, token) = match hello {
        ClientLine::Hello { name, token } => (name, token),
        other => {
            tracing::warn!(%peer, verb = client_verb(&other), "first line was not hello");
            let message = "first line must be hello".to_string();
            write_line(&mut writer, peer, &ServerLine::Error { message }).await;
            return;
        }
    };

    // Roster verification: the asserted name must exist and its token
    // must match. From here on `name` is authenticated identity, not a
    // client claim.
    let auth_failure = match shared.roster.agents.get(&name) {
        None => Some(AuthFailureReason::UnknownName),
        Some(expected) if *expected != token => Some(AuthFailureReason::TokenMismatch),
        Some(_) => None,
    };
    if let Some(reason) = auth_failure {
        tracing::warn!(%peer, name = %name, reason = %reason, "authentication failed");
        audit_append(
            shared,
            AuditEvent::AuthFailure {
                ts: Utc::now(),
                peer: peer.to_string(),
                name: name.clone(),
                reason,
            },
        )
        .await;
        let message = format!("authentication failed for {name:?}: {reason}");
        write_line(&mut writer, peer, &ServerLine::Error { message }).await;
        return;
    }
    tracing::info!(%peer, name = %name, "authenticated");
    if !write_line(&mut writer, peer, &ServerLine::Ok {}).await {
        return;
    }

    // Verb boundary: exactly one verb per connection.
    let verb_line = match read_line_bounded(&mut reader).await {
        Ok(Some(line)) => line,
        Ok(None) => {
            tracing::info!(%peer, name = %name, "peer closed without sending a verb");
            return;
        }
        Err(error) => {
            tracing::warn!(%peer, name = %name, error = %error, "failed reading verb line");
            let message = format!("invalid verb line: {error}");
            write_line(&mut writer, peer, &ServerLine::Error { message }).await;
            return;
        }
    };
    let verb: ClientLine = match wire::from_jsonl(&verb_line) {
        Ok(line) => line,
        Err(error) => {
            tracing::warn!(%peer, name = %name, error = %error, "verb line is not a valid wire line");
            let message = error.to_string();
            write_line(&mut writer, peer, &ServerLine::Error { message }).await;
            return;
        }
    };

    // Verb dispatch: exactly one verb runs, then the connection ends.
    match verb {
        ClientLine::Send(envelope) => {
            handle_send(shared, &mut writer, peer, &name, envelope).await;
        }
        ClientLine::Await {
            reply_to,
            timeout_secs,
        } => {
            handle_await(shared, &mut writer, peer, &name, reply_to, timeout_secs).await;
        }
        ClientLine::Hello { .. } => {
            tracing::warn!(%peer, name = %name, "duplicate hello after authentication");
            let message = "duplicate hello: connection is already authenticated".to_string();
            write_line(&mut writer, peer, &ServerLine::Error { message }).await;
        }
    }
}

/// Runs the send verb for an authenticated connection: validates the
/// envelope, stamps `from` with the authenticated identity, commits it
/// (audit, then handoff to a parked waiter or enqueue), and answers with
/// `accepted` or a specific error. Nothing is queued on any failure path
/// — acceptance and the audit record always agree.
async fn handle_send(
    shared: &Arc<Shared>,
    writer: &mut OwnedWriteHalf,
    peer: SocketAddr,
    name: &str,
    mut envelope: Envelope,
) {
    let started = Instant::now();
    if let Err(error) = envelope.validate() {
        tracing::warn!(%peer, name = %name, error = %error, "send rejected: envelope validation failed");
        let message = format!("invalid envelope: {error}");
        write_line(writer, peer, &ServerLine::Error { message }).await;
        return;
    }
    // From here on `from` is identity: whatever the client wrote is
    // discarded per spec, so an agent cannot speak as anyone it did not
    // authenticate as.
    envelope.from = name.to_string();

    // Safe identifiers for the completion/failure records; `body` content
    // never reaches the service log.
    let id = envelope.id.clone();
    let to = envelope.to.clone();
    let kind = envelope.kind.clone();
    let correlated = envelope.correlation_id.is_some();

    match commit_send(shared, envelope).await {
        // Consolidated success records (DIAGNOSTICS.md): validation,
        // audit, and routing completed; per-stage splits are below
        // measurement noise at this scale. The two dispositions log
        // distinctly because "handed to a live waiter" and "parked at
        // queue depth N" are different operational facts.
        Ok(SendOutcome::HandedOff) => {
            tracing::info!(
                %peer,
                name = %name,
                id = %id,
                to = %to,
                kind = %kind,
                correlated,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "send committed: envelope audited and handed to a parked waiter"
            );
            write_line(writer, peer, &ServerLine::Accepted { id }).await;
        }
        Ok(SendOutcome::Queued(queue_depth)) => {
            tracing::info!(
                %peer,
                name = %name,
                id = %id,
                to = %to,
                kind = %kind,
                correlated,
                queue_depth,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "send committed: envelope audited and queued"
            );
            write_line(writer, peer, &ServerLine::Accepted { id }).await;
        }
        Err(cause) => {
            // Refusal policy: an envelope the hub cannot audit is not
            // accepted — the audit log must never miss an accepted
            // envelope.
            tracing::error!(
                %peer,
                name = %name,
                id = %id,
                to = %to,
                error = %cause,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "send refused: envelope could not be committed"
            );
            let message = format!("envelope not accepted: {cause}");
            write_line(writer, peer, &ServerLine::Error { message }).await;
        }
    }
}

/// How a committed send was routed: handed directly to a parked waiter,
/// or queued on the destination FIFO for a later await.
enum SendOutcome {
    /// A parked waiter matched; its connection task now owns the
    /// envelope and the write-to-client delivery boundary.
    HandedOff,
    /// No waiter matched; queued at this depth on the destination FIFO.
    Queued(usize),
}

/// Commits one validated, `from`-stamped envelope: audit append, then
/// waiter handoff or enqueue, all inside the audit mutex. Holding the
/// audit lock across the routing step makes the audit file's append
/// order and routing order provably agree — the spec makes audit order
/// authoritative, and no interleaving may contradict it. On error,
/// nothing was routed. File appends are blocking work, so the whole
/// commit runs on the blocking pool.
async fn commit_send(shared: &Arc<Shared>, envelope: Envelope) -> Result<SendOutcome, String> {
    let shared = Arc::clone(shared);
    let result = tokio::task::spawn_blocking(move || {
        // Lock order rule (see `Shared`): audit before state.
        let mut audit = match shared.audit.lock() {
            Ok(guard) => guard,
            // Same recovery rationale as `audit_append`: the writer holds
            // no userspace buffer, so recovering preserves later events.
            Err(poisoned) => {
                tracing::error!("audit mutex poisoned by an earlier panic; recovering writer");
                poisoned.into_inner()
            }
        };
        // The clone is the audited copy; the original moves to a waiter
        // or the queue. AuditError's Display embeds the path and io
        // cause, so the String preserves the source context.
        let event = AuditEvent::Accepted {
            ts: Utc::now(),
            envelope: envelope.clone(),
        };
        audit.write(&event).map_err(|error| error.to_string())?;
        let mut state = lock_state(&shared);
        // Matching order per spec: a correlation-filtered waiter whose
        // reply-to equals this envelope's correlationId wins first, then
        // the oldest unfiltered waiter, else the envelope queues. The
        // loop exists because a matched waiter can be dead (its task
        // gone after registering): a failed handoff returns the envelope
        // and the next candidate is tried.
        let mut envelope = envelope;
        while let Some(waiters) = state.waiters.get_mut(&envelope.to) {
            let matched = waiters
                .iter()
                .position(|waiter| {
                    // The is_some guard keeps an unfiltered waiter from
                    // matching here when the envelope has no correlation.
                    waiter.reply_to.is_some() && waiter.reply_to == envelope.correlation_id
                })
                .or_else(|| waiters.iter().position(|waiter| waiter.reply_to.is_none()));
            let Some(index) = matched else {
                break;
            };
            let waiter = waiters.remove(index);
            if waiters.is_empty() {
                state.waiters.remove(&envelope.to);
            }
            match waiter.tx.send(envelope) {
                Ok(()) => return Ok(SendOutcome::HandedOff),
                // Receiver dropped: the waiter's task died after
                // registering. Reclaim the envelope; the dead waiter
                // stays removed.
                Err(returned) => {
                    tracing::warn!(
                        waiter_id = waiter.id,
                        to = %returned.to,
                        "purged dead waiter during handoff"
                    );
                    envelope = returned;
                }
            }
        }
        let queue = state.queues.entry(envelope.to.clone()).or_default();
        queue.push_back(envelope);
        Ok(SendOutcome::Queued(queue.len()))
    })
    .await;
    match result {
        Ok(outcome) => outcome,
        // Join failure means the commit's fate is unknown; refuse so the
        // client re-sends (end-to-end at-least-once tolerates duplicates).
        Err(join_error) => Err(format!("commit task failed: {join_error}")),
    }
}

/// How an await begins under the state lock: an already-queued envelope
/// is consumed immediately, or a waiter parks until handoff or timeout.
enum AwaitStart {
    /// A queued envelope matched; deliver without parking.
    Immediate(Envelope),
    /// No queued match; carries the registered waiter's handoff receiver
    /// and the id the timeout path needs to deregister it.
    Parked {
        waiter_id: u64,
        rx: oneshot::Receiver<Envelope>,
    },
}

/// Runs the await verb for an authenticated connection: consumes a
/// queued matching envelope immediately when one exists, otherwise parks
/// a waiter and blocks until a send hands an envelope over or the
/// hub-side timeout elapses. A timeout reply guarantees nothing was
/// consumed.
async fn handle_await(
    shared: &Arc<Shared>,
    writer: &mut OwnedWriteHalf,
    peer: SocketAddr,
    name: &str,
    reply_to: Option<String>,
    timeout_secs: Option<u64>,
) {
    let started = Instant::now();
    // Mirror of the envelope rule (EmptyCorrelationId): accepted
    // envelopes never carry an empty correlationId, so an empty reply-to
    // could match nothing, ever — reject it as a client error.
    if matches!(&reply_to, Some(id) if id.is_empty()) {
        tracing::warn!(%peer, name = %name, "await rejected: replyTo is empty");
        let message = "invalid await: replyTo is empty; omit it or use an envelope id".to_string();
        write_line(writer, peer, &ServerLine::Error { message }).await;
        return;
    }
    tracing::info!(
        %peer,
        name = %name,
        reply_to = ?reply_to,
        timeout_secs = ?timeout_secs,
        "await accepted"
    );

    // Under the state lock, either consume a queued envelope now or
    // register a waiter. Consumption and registration share the lock
    // with send-side matching, so an envelope is consumed exactly once
    // and no envelope can slip past between "queue is empty" and
    // "waiter is parked". The critical section is short and does no I/O,
    // so locking a std mutex directly from async context is fine here
    // (unlike `commit_send`, whose file append needs the blocking pool).
    let start = {
        let mut state = lock_state(shared);
        match take_queued(&mut state, name, reply_to.as_deref()) {
            Some(envelope) => AwaitStart::Immediate(envelope),
            None => {
                let (tx, rx) = oneshot::channel();
                let waiter_id = state.next_waiter_id;
                state.next_waiter_id += 1;
                state
                    .waiters
                    .entry(name.to_string())
                    .or_default()
                    .push(Waiter {
                        id: waiter_id,
                        reply_to: reply_to.clone(),
                        tx,
                    });
                AwaitStart::Parked { waiter_id, rx }
            }
        }
    };

    let (envelope, source) = match start {
        AwaitStart::Immediate(envelope) => (envelope, "queue"),
        AwaitStart::Parked { waiter_id, rx } => {
            tracing::info!(%peer, name = %name, waiter_id, "await parked; no queued match");
            match wait_for_handoff(shared, name, waiter_id, rx, timeout_secs).await {
                HandoffOutcome::Delivered(envelope) => (envelope, "handoff"),
                HandoffOutcome::TimedOut => {
                    tracing::info!(
                        %peer,
                        name = %name,
                        waiter_id,
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        "await timed out; nothing consumed"
                    );
                    write_line(writer, peer, &ServerLine::Timeout {}).await;
                    return;
                }
                HandoffOutcome::ChannelLost => {
                    tracing::error!(
                        %peer,
                        name = %name,
                        waiter_id,
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        "await handoff channel closed without delivery; hub bug"
                    );
                    let message = "internal error: await handoff lost".to_string();
                    write_line(writer, peer, &ServerLine::Error { message }).await;
                    return;
                }
            }
        }
    };
    deliver_envelope(shared, writer, peer, name, envelope, source, started).await;
}

/// What ended a parked waiter's wait.
enum HandoffOutcome {
    /// A send handed this envelope over; the caller owns delivery.
    Delivered(Envelope),
    /// The hub-side timeout elapsed with the waiter still registered:
    /// nothing was consumed, guaranteed.
    TimedOut,
    /// The handoff channel closed without an envelope. A waiter is only
    /// removed by handing an envelope over or by its own timeout path,
    /// so this is unreachable absent a hub bug; it is still handled so a
    /// bug surfaces as a loud error, not a hang.
    ChannelLost,
}

/// Blocks a parked waiter until handoff or hub-side timeout. The timeout
/// race — a send matching this waiter at the same instant the timer
/// fires — is resolved under the state lock: if the timeout path finds
/// the waiter still registered, removing it guarantees no envelope was
/// or will be handed over; if the waiter is already gone, the handoff
/// won the race and the envelope is already in the channel (removal and
/// channel send happen under one lock acquisition), so it is delivered
/// rather than timed out.
async fn wait_for_handoff(
    shared: &Arc<Shared>,
    name: &str,
    waiter_id: u64,
    mut rx: oneshot::Receiver<Envelope>,
    timeout_secs: Option<u64>,
) -> HandoffOutcome {
    // No timeout: block until handoff (the CLI's --timeout is optional;
    // an absent timeout blocks indefinitely per spec).
    let Some(secs) = timeout_secs else {
        return match rx.await {
            Ok(envelope) => HandoffOutcome::Delivered(envelope),
            Err(_) => HandoffOutcome::ChannelLost,
        };
    };
    tokio::select! {
        result = &mut rx => match result {
            Ok(envelope) => HandoffOutcome::Delivered(envelope),
            Err(_) => HandoffOutcome::ChannelLost,
        },
        _ = tokio::time::sleep(Duration::from_secs(secs)) => {
            let removed = remove_waiter(&mut lock_state(shared), name, waiter_id);
            if removed {
                HandoffOutcome::TimedOut
            } else {
                // Lost the race: the handoff committed first, so the
                // envelope is already in the channel and this resolves
                // immediately.
                match rx.await {
                    Ok(envelope) => HandoffOutcome::Delivered(envelope),
                    Err(_) => HandoffOutcome::ChannelLost,
                }
            }
        }
    }
}

/// Consumes the first queued envelope matching an await from `name`:
/// with a reply-to filter, the first envelope whose correlationId equals
/// it — which may sit mid-queue; correlation matches deliberately jump
/// the FIFO per the spec's matching rules — and without a filter, the
/// queue front. Returns `None` when nothing matches; the caller then
/// parks a waiter.
fn take_queued(state: &mut HubState, name: &str, reply_to: Option<&str>) -> Option<Envelope> {
    let queue = state.queues.get_mut(name)?;
    let index = match reply_to {
        Some(reply_to) => queue
            .iter()
            .position(|envelope| envelope.correlation_id.as_deref() == Some(reply_to))?,
        None => 0,
    };
    let envelope = queue.remove(index)?;
    // Drop the entry when emptied so arbitrary destination names cannot
    // grow the queue map without bound.
    if queue.is_empty() {
        state.queues.remove(name);
    }
    Some(envelope)
}

/// Removes a parked waiter by id, returning whether it was still
/// registered. "Still registered" proves no envelope was or will be
/// handed to it, because handoff removes the waiter and sends the
/// envelope under the same lock the caller holds.
fn remove_waiter(state: &mut HubState, name: &str, waiter_id: u64) -> bool {
    let Some(waiters) = state.waiters.get_mut(name) else {
        return false;
    };
    let Some(index) = waiters.iter().position(|waiter| waiter.id == waiter_id) else {
        return false;
    };
    waiters.remove(index);
    if waiters.is_empty() {
        state.waiters.remove(name);
    }
    true
}

/// Delivers one envelope to this connection's await client. The write to
/// the client is the consumption boundary per spec: on write success the
/// delivery audit record is appended; on failure the envelope is lost —
/// the spec's accepted at-most-once outcome, recovered by end-to-end
/// re-dispatch — and the service log carries the loss evidence,
/// distinguishing the successful backend match from the failed delivery.
async fn deliver_envelope(
    shared: &Arc<Shared>,
    writer: &mut OwnedWriteHalf,
    peer: SocketAddr,
    name: &str,
    envelope: Envelope,
    source: &str,
    started: Instant,
) {
    // Safe identifiers for the completion/failure records; the envelope
    // itself moves into the wire line and `body` never reaches the
    // service log.
    let id = envelope.id.clone();
    let from = envelope.from.clone();
    let kind = envelope.kind.clone();
    if !write_line(writer, peer, &ServerLine::Envelope(envelope)).await {
        // write_line logged the socket failure; this record adds the
        // protocol consequence and the lost envelope's identifiers.
        tracing::error!(
            %peer,
            name = %name,
            id = %id,
            source,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "delivery failed: envelope consumed and lost (at-most-once)"
        );
        return;
    }
    audit_append(
        shared,
        AuditEvent::Delivery {
            ts: Utc::now(),
            id: id.clone(),
            to: name.to_string(),
            peer: peer.to_string(),
        },
    )
    .await;
    // Consolidated success record (DIAGNOSTICS.md): match, write, and
    // audit completed; `source` says whether the envelope came off the
    // queue or via live handoff.
    tracing::info!(
        %peer,
        name = %name,
        id = %id,
        from = %from,
        kind = %kind,
        source,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "await delivered envelope"
    );
}

/// Appends one event to the audit log from async context. File appends
/// are blocking work, so they run on the blocking pool rather than a
/// Tokio worker thread; the call is awaited so the caller knows the event
/// is appended before proceeding. Failure policy: a lost connection/auth
/// event is logged and the hub keeps serving — unlike `commit_send`,
/// which refuses an envelope it cannot audit.
async fn audit_append(shared: &Arc<Shared>, event: AuditEvent) {
    let shared = Arc::clone(shared);
    let result = tokio::task::spawn_blocking(move || {
        let mut audit = match shared.audit.lock() {
            Ok(guard) => guard,
            // Poisoning means an earlier append panicked. The writer
            // holds no userspace buffer, so the file is at worst missing
            // that event's line; recovering the handle preserves every
            // later event.
            Err(poisoned) => {
                tracing::error!("audit mutex poisoned by an earlier panic; recovering writer");
                poisoned.into_inner()
            }
        };
        if let Err(error) = audit.write(&event) {
            tracing::error!(error = %error, event = ?event, "audit append failed; event lost");
        }
    })
    .await;
    if let Err(join_error) = result {
        tracing::error!(error = %join_error, "audit append task failed; event lost");
    }
}

/// Why reading one protocol line from a peer failed. Line content is
/// deliberately never captured: the hello line contains a token, so these
/// errors must stay safe to log and to echo back to the peer.
#[derive(Debug)]
enum ReadLineError {
    /// The socket read itself failed.
    Io(std::io::Error),
    /// The peer closed mid-line: bytes arrived but EOF came before a
    /// newline, so no complete line exists.
    TruncatedLine { received: usize },
    /// The peer exceeded `MAX_LINE_BYTES` without sending a newline.
    TooLong,
    /// The completed line is not valid UTF-8.
    NotUtf8,
}

impl fmt::Display for ReadLineError {
    // Rendered into service-log lines and client error lines.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadLineError::Io(source) => write!(f, "socket read failed: {source}"),
            ReadLineError::TruncatedLine { received } => {
                write!(f, "connection closed mid-line after {received} bytes")
            }
            ReadLineError::TooLong => write!(f, "line exceeds {MAX_LINE_BYTES} byte limit"),
            ReadLineError::NotUtf8 => write!(f, "line is not valid UTF-8"),
        }
    }
}

/// Reads one newline-terminated line with a hard byte cap, returning
/// `Ok(None)` on a clean EOF before any bytes arrive. tokio's `read_line`
/// has no cap and would let a peer grow hub memory without bound, hence
/// this manual fill_buf/consume loop.
async fn read_line_bounded(
    reader: &mut BufReader<OwnedReadHalf>,
) -> Result<Option<String>, ReadLineError> {
    let mut line: Vec<u8> = Vec::new();
    loop {
        let buf = reader.fill_buf().await.map_err(ReadLineError::Io)?;
        if buf.is_empty() {
            // EOF: clean between lines, truncated within one.
            return if line.is_empty() {
                Ok(None)
            } else {
                Err(ReadLineError::TruncatedLine {
                    received: line.len(),
                })
            };
        }
        if let Some(newline) = buf.iter().position(|&b| b == b'\n') {
            line.extend_from_slice(&buf[..newline]);
            reader.consume(newline + 1);
            break;
        }
        line.extend_from_slice(buf);
        let consumed = buf.len();
        reader.consume(consumed);
        // The cap is enforced between refills; the reader's internal
        // buffer size bounds each step's overshoot.
        if line.len() > MAX_LINE_BYTES {
            return Err(ReadLineError::TooLong);
        }
    }
    if line.len() > MAX_LINE_BYTES {
        return Err(ReadLineError::TooLong);
    }
    // Content bytes are dropped on failure, never attached to the error.
    match String::from_utf8(line) {
        Ok(text) => Ok(Some(text)),
        Err(_) => Err(ReadLineError::NotUtf8),
    }
}

/// Serializes one server line and writes it to the peer, logging any
/// failure. Returns false when the line did not reach the socket and the
/// connection should be abandoned. Delivery failure is logged distinctly
/// from the backend outcome per DIAGNOSTICS.md.
async fn write_line(writer: &mut OwnedWriteHalf, peer: SocketAddr, line: &ServerLine) -> bool {
    let jsonl = match wire::to_jsonl(line) {
        Ok(jsonl) => jsonl,
        Err(error) => {
            // Failing to serialize our own typed line is a hub bug, not a
            // peer problem; log it as such.
            tracing::error!(%peer, error = %error, "cannot serialize server line");
            return false;
        }
    };
    if let Err(error) = writer.write_all(jsonl.as_bytes()).await {
        tracing::warn!(%peer, error = %error, "failed writing server line to peer");
        return false;
    }
    true
}

/// Names a client line's verb for diagnostics without echoing its payload
/// (a hello line contains a token).
fn client_verb(line: &ClientLine) -> &'static str {
    match line {
        ClientLine::Hello { .. } => "hello",
        ClientLine::Send(_) => "send",
        ClientLine::Await { .. } => "await",
    }
}
