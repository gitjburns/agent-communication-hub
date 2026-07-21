# Implementation Plan

## Fixed design decisions

- Single Cargo package (`agent-communication-hub`): shared library `src/lib.rs`
  plus two binaries — `agent-hub-server` (`src/bin/agent-hub-server.rs`) and
  `agent-hub` (`src/bin/agent-hub.rs`). Envelope schema, wire line types, and
  JSONL framing live once in the lib; both binaries share them at compile time.
- Deployment boundary: `agent-hub-server` runs only in a protected environment;
  `agent-hub` is installed on the shared PATH for all clients. Clients never
  have access to the server binary.
- No database. Queues are in-memory per spec. The audit log is a hub-owned
  append-only JSONL file (one event per line: accepted envelopes in full,
  deliveries, connections, auth failures). The service log (DIAGNOSTICS.md) is
  a separate file; the two records serve different purposes and never merge.
- Async exists only in the server's network layer (tokio: listener,
  per-connection tasks, await-timeout timers). Parsing, validation, and
  matching are synchronous pure functions in the lib. The client is fully
  synchronous over `std::net::TcpStream` and does not depend on tokio.
- Server state is `Arc<Mutex<HubState>>`: per-agent FIFO queues plus the
  waiter registry. Matching, consumption, and timeout cancellation all happen
  inside this one lock so an envelope is consumed exactly once.
- `config.toml` (strict: `deny_unknown_fields`, every key required, no
  defaults): `port`, `audit_log_path`, `service_log_path`, `roster_path`.
  Defaults live only in `config.example.toml`.
- Agent roster: a dedicated TOML file at `roster_path` mapping agent name →
  token. Operator-owned, mode 600, never committed. Missing file, empty
  roster, or unknown keys are fatal startup errors. Client-side tokens remain
  per-agent files read via `--token-file` only, per spec.
- `await` timeout is enforced hub-side: the await request line carries the
  timeout, the hub answers with a distinct timeout line, and nothing is
  consumed. (A client-side socket deadline cannot guarantee "nothing
  consumed" — the hub could write an envelope at the same instant.)
- Envelope `body` is `serde_json::Value` end-to-end; the hub never parses it
  into a typed shape.
- Dependencies: tokio, serde, serde_json, toml, clap, uuid (v4), chrono,
  tracing, tracing-subscriber, tracing-appender. Nothing else without
  approval.
- Verification after every phase touching Rust: `cargo fmt`, `cargo check`,
  `cargo clippy`. No test modules or `cargo test` (testing is disabled per
  AGENTS.md).

## Phases

- [ ] Phase 1 — Scaffolding & config. `cargo init`; crate/binary layout
  above; `.gitignore` (`/target`, `config.toml`, roster and token files,
  log files); `config.example.toml`; strict config loader; roster loader;
  service-log initialization with DIAGNOSTICS.md startup coverage (fatal
  startup errors to stderr, plus the service log once initialized).
- [ ] Phase 2 — Protocol core (lib). Envelope struct; hello / ok / error /
  accepted / envelope / timeout line types as shared typed shapes; JSONL
  read/write helpers; envelope validation. All synchronous.
- [ ] Phase 3 — Server: listener & auth. Loopback-only bind of configured
  port; per-connection task; hello verification against roster; auth-failure
  path (one error line, close, audit + service log); verb dispatch; audit-log
  writer (append + flush per line, single owner).
- [ ] Phase 4 — Server: send path. Envelope validation; `from` stamped from
  authenticated identity; enqueue to per-agent FIFO; accepted response line;
  full-envelope audit record.
- [ ] Phase 5 — Server: await path. Waiter registry; matching order per
  spec (correlation-filtered first, then unfiltered, else queue); hub-side
  timeout; consumption at the write-to-client boundary; delivery audit
  records; live handoff when a send arrives while a waiter is parked.
- [ ] Phase 6 — Client CLI. `agent-hub send` / `agent-hub await` with the
  spec's normative flags; token read via `--token-file`; `--body -` stdin
  support; prints accepted id (send) or the envelope line (await); distinct
  exit codes for success, connection/auth/validation failure, and timeout.
- [ ] Phase 7 — End-to-end verification. Manual smoke: start
  `agent-hub-server` (requires explicit user approval per AGENTS.md), run
  send/await flows including correlation matching and timeout, inspect audit
  and service logs against DIAGNOSTICS.md coverage.
