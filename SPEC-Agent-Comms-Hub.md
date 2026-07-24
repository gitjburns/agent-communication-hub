# SPEC: Agent Communication Hub

## Purpose And Status

Normative v1 contract for the Agent Communication Hub ("the hub"): the
persistent process that routes messages between Claude Code agent sessions.
Co-designed in session; built and operated by the user in the hub's own
repository as an independent Rust service, per the services precedent
(classifier, data store). Consumers are the two protocol skills — Mneme's
dispatch skill and the repo-resident inbox skill — specified separately in a
companion document.

This spec fixes the wire contract and operational guarantees. Internal
implementation (storage structures, concurrency model, config file format,
binary layout) is the builder's discretion wherever this document is silent.

## Role In The Architecture

Topology is hub-and-spoke. The user works interactively with Mneme only.
Repo-resident sessions are unattended thin routers: each polls nothing and
computes nothing itself — a background `await` process wakes it when a
message arrives; it spawns subagents for actual work (read-only in the first
MVP), replies, and re-arms. Mneme orchestrates dispatches and relays
judgment questions to the user.

The hub satisfies the daemon gate ("no daemon until a capability requires a
persistent process"): a message hub between N
sleeping turn-based sessions requires exactly one always-on process to hold
queues, verify identity, and push the events that wake watchers. It is not a
proxy. It is the seed of two anticipated components: the agent-to-agent
communication protocol and daemon-as-presence; rooms-service integration is
an extension seam, not a v1 concern.

## Wire Protocol

### Transport

- TCP, loopback only. The hub listens on a single configured port bound to
  localhost. Multi-machine operation is out of v1.
- All traffic is newline-delimited JSON (JSONL): one JSON object per line,
  UTF-8, no framing beyond the newline.

### Connection Lifecycle

1. Client connects and sends a hello line:

   ```json
   {"hello": {"name": "datastore", "token": "<token>"}}
   ```

2. The hub verifies `token` against its configured `name → token` map. On
   mismatch or unknown name: one error line, connection closed, auth failure
   logged. On success: one ok line, connection enters verb mode.
3. The client issues exactly one verb per connection (`send` or `await`).
   Connections are short-lived and disposable; clients reconnect freely.

### Envelope Schema

Every message is an envelope:

```json
{
  "id": "<unique id, UUIDv4 recommended>",
  "from": "mneme",
  "to": "datastore",
  "ts": "2026-07-20T21:04:00Z",
  "kind": "task",
  "correlationId": null,
  "body": { }
}
```

- `id` — unique per envelope, assigned by the sender.
- `from` — always overwritten by the hub with the authenticated name. A
  client cannot speak as anyone it did not authenticate as.
- `to` — destination agent name. Unknown destinations are accepted and
  queued (the agent may connect later); this is deliberate.
- `ts` — sender clock, ISO 8601 UTC. Informational; the audit log's
  ordering is authoritative.
- `kind` — `task` or `result` in v1. The set is extensible; the hub routes
  unknown kinds without complaint.
- `correlationId` — `null`, or the `id` of the envelope this one answers.
- `body` — opaque to the hub. The hub validates envelope fields and never
  inspects, interprets, or transforms `body`. All task/result semantics
  (including the `needs-ruling` result status) belong to the skills spec.

## Client CLI

The hub's repository ships the client CLI; the skills invoke it and never
speak TCP themselves. It is the retargeting seam: if the hub is later
subsumed by the platform's rooms service, only the CLI's internals change.
Verb and flag names below are normative; the binary name is the builder's
choice (referred to as `hub` here).

### `hub send`

```
hub send --as <name> --token-file <path> --to <agent> --kind <kind> \
         [--correlation-id <id>] --body <json | '-' for stdin>
```

Connects, authenticates, submits one envelope, prints the accepted
envelope's `id`, exits.

- Exit 0: the hub accepted and queued (or delivered) the envelope.
  Acceptance means queued, not delivered.
- Exit nonzero: connection, auth, or validation failure. Nothing was queued.

### `hub await`

```
hub await --as <name> --token-file <path> [--reply-to <id>] [--kind <kind>] [--timeout <secs>]
```

Connects, authenticates, blocks until one matching envelope is available,
emits it as one JSON line on stdout, exits 0. This one-shot,
exit-on-event contract is what makes `await` double as the resident's
background watcher: process exit wakes the session.

- Without filters: matches any envelope addressed to `<name>`.
- With `--reply-to <id>`: matches only envelopes whose `correlationId`
  equals `<id>` — the blocking send-and-wait composite.
- With `--kind <kind>`: matches only envelopes of that kind. Filters
  conjoin: an envelope must pass every filter given.
- `--timeout` elapsed: exit with a distinct nonzero code, nothing consumed.
- Routing when multiple `await`s are active for one agent: an envelope goes
  to at most one consumer; among awaits whose filters all pass, the most
  specific wins — reply-to plus kind, then reply-to, then kind, then
  unfiltered — oldest first within a tier; else the envelope queues.

## Authentication

- The hub's config maps each agent name to a token. Asserted name must
  match presented token; there is no shared secret, so an authenticated
  envelope's `from` is identity, not a claim.
- Tokens are stored in plain text files, one per agent, mode 600, and never
  committed (gitignored where a repo is involved). The CLI reads the token
  only via `--token-file`; no environment variables, no discovery.
- Token issuance, rotation, and the hub-side config are the operator's.
- Rationale on the record: hub messages are instructions that agent
  sessions act on with real permissions. An unauthenticated local port
  would be a prompt-injection surface for any errant local process. Token
  auth closes the only unauthorized write path into the protocol at
  near-zero implementation cost.

## Delivery Semantics

- Queues are in-memory, one per agent name, FIFO per queue.
- The hub's own guarantee is best-effort, at-most-once per envelope:
  consumption commits when the socket accepts the first byte of the
  write to a matched `await` client. A write that fails with zero bytes
  accepted returns the envelope to routing (next matching waiter, else
  queue front). Only a partial write — or a peer that dies after
  accepting bytes — loses the envelope.
- The protocol achieves at-least-once end-to-end, by contract with the
  endpoints rather than by hub machinery:
  - Mneme treats every dispatch as outstanding until its correlated result
    arrives; silence past a deadline means re-dispatch, not mourn. Hub
    restart therefore loses nothing that matters.
  - Consumers are idempotent: residents mark work handled only after the
    reply is sent, and tolerate re-delivered or re-dispatched tasks.
- This is the end-to-end argument applied deliberately: the hub stays
  simple because reliability lives at the endpoints, which can retry.

## Audit Log

- Append-only file, owned and written solely by the hub: every accepted
  envelope in full, every delivery, every connection, every auth failure.
- The log's append order is the authoritative ordering of protocol events —
  the single owner of the cross-agent record.
- Write-only in v1: grep is the query interface. No replay, no retention
  machinery.

## Operational Contracts

- Uptime is the operator's guarantee. Queue loss on restart is an accepted
  consequence, recovered by Mneme's re-dispatch discipline.
- The hub never interprets message bodies, never originates envelopes, and
  never modifies envelope content beyond stamping `from`.
- Loopback binding is a hard v1 constraint, not a default.

## Deliberately Out Of v1

Each exclusion applies the same admission test: consequence × likelihood
weighed against mitigation cost.

- **Queue durability across restarts** — low consequence (re-dispatch
  recovers), low likelihood (operator keeps the service up), real storage
  complexity.
- **Presence / roster queries** — no v1 consumer; Mneme's deadline
  discipline covers "is anyone there" indirectly.
- **Broadcast / multi-recipient** — hub-and-spoke has one orchestrator;
  fan-out is Mneme sending N envelopes.
- **Multi-machine / non-loopback transport** — no second machine exists in
  the MVP; TCP already leaves the door open.
- **TTLs, delivery receipts** — the audit log plus end-to-end retries make
  both redundant at this scale.
- **Mid-task conversations (task parking)** — one-shot tasks with
  re-dispatch chains cover read-only work; parking machinery is the most
  complex feature the design considered and v1's traffic cannot justify it.
- **Auth beyond per-agent tokens (TLS, key exchange)** — loopback-only
  transport; revisit with multi-machine.

## Extension Seams

Recorded so v2 discussions start from intent rather than archaeology:

- New `kind` values pass through the hub unchanged; richer lifecycles
  (parking, events, rulings as first-class kinds) are protocol-level
  changes only.
- Presence becomes a third CLI verb against state the hub already holds.
- Rooms-service integration replaces hub internals behind the CLI seam;
  the skills' contract is designed not to change.
