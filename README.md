# Agent Communication Hub

A small, persistent message hub that routes envelopes between Claude Code
agent sessions. One always-on process holds per-agent queues, verifies
identity, and wakes sleeping sessions when a message arrives. Topology is
hub-and-spoke: an orchestrator session dispatches tasks; unattended
repo-resident sessions block on `await`, do the work, and reply.

The hub never interprets message bodies, never originates envelopes, and
never modifies envelope content beyond stamping the authenticated sender.

## Components

One Cargo package builds a shared protocol library and two binaries:

- **`agent-hub-server`** — the hub process. Loopback-only TCP listener,
  token authentication, in-memory FIFO queues, append-only audit log.
  Runs only in a protected environment; clients never have access to it.
- **`agent-hub`** — the client CLI, installed on the shared PATH. Two
  verbs, `send` and `await`, over a synchronous connection. Skills invoke
  this binary and never speak TCP themselves.

Both binaries share the envelope schema and wire line types at compile
time, so client and server cannot disagree about the protocol.

## How it works

### Wire protocol

Transport is TCP bound to `127.0.0.1` only, carrying newline-delimited
JSON: one JSON object per line, UTF-8. Every connection is short-lived
and performs exactly one verb:

1. The client sends a hello line: `{"hello":{"name":"...","token":"..."}}`.
2. The hub verifies the name/token pair against its roster. Failure gets
   one error line and a closed connection; success gets an ok line.
3. The client sends one verb line (`send` or `await`), receives the
   reply, and the connection ends.

### Envelopes

The unit of routing:

```json
{
  "id": "<sender-assigned unique id, UUIDv4>",
  "from": "mneme",
  "to": "datastore",
  "ts": "2026-07-21T06:40:51Z",
  "kind": "task",
  "correlationId": null,
  "body": { }
}
```

- `from` is always overwritten by the hub with the authenticated name — a
  client cannot speak as anyone it did not authenticate as.
- `to` may name an agent the hub has never seen; unknown destinations are
  accepted and queued deliberately (the agent may connect later).
- `kind` is `task` or `result` in v1, but the set is extensible: the hub
  routes unknown kinds without complaint.
- `correlationId` is `null` or the `id` of the envelope this one answers.
- `body` is opaque. The hub validates envelope fields and never inspects,
  interprets, or transforms the body.
- `ts` is the sender's clock and informational only; the audit log's
  append order is the authoritative ordering of protocol events.

### Delivery semantics

- One in-memory FIFO queue per agent name.
- A `send` either hands the envelope directly to a parked `await`
  (correlation-filtered waiters match first, then the oldest unfiltered
  waiter) or queues it.
- An `await` consumes at most one envelope. Consumption happens at the
  write-to-client boundary: the hub's own guarantee is best-effort,
  **at-most-once** per envelope.
- **At-least-once is achieved end-to-end, not by the hub**: the
  orchestrator treats every dispatch as outstanding until its correlated
  result arrives and re-dispatches after a deadline; consumers are
  idempotent. Queue loss on hub restart is an accepted consequence,
  recovered by the same re-dispatch discipline.
- The `await` timeout is enforced hub-side. A timeout reply guarantees
  nothing was consumed — a client-side socket deadline could not make
  that guarantee.

### Authentication

The hub's roster maps each agent name to a token; asserted name must
match presented token. Rationale: hub messages are instructions that
agent sessions act on with real permissions, so an unauthenticated local
port would be a prompt-injection surface for any errant local process.
Tokens live in plain files, mode 600, never committed; the CLI reads a
token only via `--token-file` — no environment variables, no discovery.

### Records

The hub writes two records that never merge:

- **Audit log** (`audit_log_path`, JSONL): the authoritative ordering of
  protocol events — every connection, every auth failure, every accepted
  envelope in full (body included, `from` already stamped), every
  delivery. Append-only, written solely by the hub. Write-only in v1:
  grep is the query interface.
- **Service log** (`service_log_path`): operational diagnostics — startup
  boundaries, connection lifecycles, send/await outcomes with elapsed
  times, and error paths. Token values and body content never appear
  here.

## Setup

### 1. Build

From the repository root:

```
cargo build --release
```

The binaries land in `target/release/`:

- `target/release/agent-hub-server`
- `target/release/agent-hub`

(A plain `cargo build` produces debug builds at `target/debug/` instead;
the paths below assume the release build.)

Deployment boundary: copy or symlink `agent-hub` onto the shared PATH
for agents to use; keep `agent-hub-server` out of any client-accessible
PATH — it belongs only to the protected environment that operates the
hub.

### 2. Create the server config

Copy the committed example and edit it:

```
cp config.example.toml config.toml
```

```toml
port = 46110                      # TCP port on 127.0.0.1
audit_log_path = "audit.jsonl"    # append-only protocol record
service_log_path = "service.log"  # operational diagnostics
roster_path = "roster.toml"       # name -> token map
```

Config is strict: every key is required, unknown keys are fatal, and
there are no runtime defaults. Relative paths in `config.toml` resolve
against the directory the server is started from, so either start the
server from the directory containing these files or use absolute paths.

### 3. Create the roster

Create the file named by `roster_path`, restrict it to the operator, and
add one entry per agent:

```
touch roster.toml && chmod 600 roster.toml
```

```toml
[agents]
mneme = "<token>"
datastore = "<token>"
```

Any unique secret string works as a token; e.g. generate one with
`openssl rand -hex 16`. A missing roster file, an empty roster, a blank
name or token, or keys outside the `[agents]` table are fatal startup
errors: a hub that can authenticate nobody must not start.

### 4. Distribute agent tokens

Give each agent its own token in a file of its own, containing exactly
that agent's roster token (`tokens/<name>.token` is the convention used
here):

```
mkdir -p tokens
printf '%s\n' '<token>' > tokens/mneme.token
chmod 600 tokens/mneme.token
```

A trailing newline in the token file is tolerated; the CLI trims it
before authenticating. Roster and token files are credential material:
mode 600 and never committed (the repository's `.gitignore` already
excludes `roster.toml`, `*.token`, and `tokens/`).

### 5. Run the server

From the directory containing `config.toml` (assuming relative paths in
the config):

```
./target/release/agent-hub-server --config config.toml
```

On success the service log ends its startup sequence with
`listener bound; hub ready`. The server runs until killed; uptime is the
operator's guarantee. Fatal startup errors go to stderr, and also to the
service log once it is initialized.

## Using the client

Both verbs require `--as` (agent name), `--token-file`, and `--port`.
The port is deliberately explicit — no default, no environment variable:
the client never silently chooses which service it talks to.

### Send

```
agent-hub send --as mneme --token-file tokens/mneme.token --port 46110 \
    --to datastore --kind task --body '{"op":"reindex"}'
```

Prints the accepted envelope's id on stdout and exits 0. Acceptance
means queued (or delivered), not delivered. Use `--body -` to read the
JSON body from stdin, and `--correlation-id <id>` when answering a
previous envelope.

### Await

```
agent-hub await --as datastore --token-file tokens/datastore.token --port 46110 \
    [--reply-to <id>] [--timeout <secs>]
```

Blocks until one matching envelope is available, prints it as one JSON
line on stdout, and exits 0. Without `--reply-to` it matches any
envelope addressed to the authenticated name; with `--reply-to <id>` it
matches only envelopes whose `correlationId` equals `<id>` — the
blocking send-and-wait composite. Without `--timeout` it blocks
indefinitely.

This one-shot, exit-on-event contract is what lets `await` double as a
resident session's background watcher: process exit wakes the session.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Success (`send`: queued; `await`: envelope delivered on stdout) |
| 1 | Connection, auth, validation, usage, or protocol failure — nothing queued or consumed |
| 2 | `await` hub-side timeout — nothing consumed; the caller may re-arm |

Hub error messages pass through to stderr verbatim, prefixed with
`agent-hub:`.

### A complete exchange

```sh
# Resident arms its watcher (typically a background process):
agent-hub await --as datastore --token-file tokens/datastore.token --port 46110

# Orchestrator dispatches a task and blocks for its result:
id=$(agent-hub send --as mneme --token-file tokens/mneme.token --port 46110 \
         --to datastore --kind task --body '{"op":"reindex"}')
agent-hub await --as mneme --token-file tokens/mneme.token --port 46110 \
    --reply-to "$id" --timeout 300

# Resident replies to the task it received (its id was in the envelope):
agent-hub send --as datastore --token-file tokens/datastore.token --port 46110 \
    --to mneme --kind result --correlation-id "$id" --body '{"status":"done"}'
```

## Scope and specification

Deliberate v1 exclusions (queue durability, presence queries, broadcast,
multi-machine transport, delivery receipts, mid-task conversations) and
the extension seams for lifting them are recorded in
`SPEC-Agent-Comms-Hub.md`, the normative wire contract. `PLAN.md` records
the implementation's fixed design decisions.
