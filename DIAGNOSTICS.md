# Service Diagnostics Coding Standard

## Core Rule

Once the service log is initialized, every meaningful lifecycle boundary and
every error path must leave durable, useful evidence there.

The log must let an operator determine what started, what completed, what
failed, where it failed, what durable state committed, what active state
published, what the client was told, and what remains unknown after process or
transport failure.

Terminal output, CLI output, comments, database rows, and inferred state are not
durable diagnostics. They may repeat facts, but the service log must contain
the authoritative evidence.

Startup fatal errors have an additional channel rule: every fatal startup error
must be written to stderr. Once the configured file logger has initialized, the
same error must also be written to the service log. A failure that prevents the
service from obtaining or opening the configured log path can only use stderr;
for that pre-logger boundary, stderr is the authoritative available evidence.

## Useful Logs

A useful log records at least one of these facts:

- a lifecycle boundary was reached;
- a state transition occurred;
- a durable storage boundary was reached;
- an active in-memory publish boundary was reached;
- an external process or model call started, succeeded, or failed;
- operation stream delivery succeeded or failed;
- a spawned task completed, failed, panicked, or was cancelled;
- a failure occurred with local diagnostic context.

Avoid vague activity logs. A line such as `operation failed` is insufficient
unless it includes the operation, operation ID, stage, error, relevant safe
identifiers, and elapsed time when measurable.

## Boundary Rule

Before adding or changing code, identify the diagnostic boundaries the code
crosses. At each fallible or long-running boundary, log:

- start;
- success or meaningful checkpoint;
- normal error with local context;
- elapsed milliseconds when the boundary has measurable duration.

Helper functions may return typed errors without logging when their caller owns
the diagnostic boundary. Boundary-owning functions must log or wrap failures
with the local facts known at that boundary.

On a measured high-frequency success path, a boundary's start and intermediate
success records may be consolidated into a single boundary-completion record,
provided that record carries every fact and per-phase elapsed duration the
separate records carried, and every error path keeps its full immediate local
context. Consolidation trades crash- and hang-window attribution for hot-path
throughput: a request that completes remains fully attributable from its
completion records, while one that never completes a phase is narrowed only to
the span between its last completed record and the next expected one.
Consolidation never applies to error paths.

## Required Lifecycle Coverage

Startup must log each applicable boundary among mode, bind address, config path
when known, credential or token publication status without secret values,
subsystem initialization (inference, storage, cache, external clients as
applicable), startup smoke checks, listener bind attempt and success, readiness,
and fatal startup errors. A subsystem absent by design requires no synthetic
log. A configured or expected subsystem that is skipped, unavailable, or fails
initialization must leave explicit diagnostic evidence. Fatal startup errors follow the startup
channel rule above: always stderr, and also the service log once its writer is
available.

Every operation must log accepted, validation failure when applicable, each
meaningful stage start and success/checkpoint, terminal result ready, terminal
error ready, terminal delivery success or failure, operation task finish, and
panic or cancellation when detectable. On a consolidated success path (see the
Boundary Rule), stage starts and intermediate successes may merge into
completion records carrying the same facts and per-stage elapsed durations.

Every storage transaction must log begin attempt, begin success or failure,
each persistence phase, each phase failure with local identifiers, commit
attempt, commit success or failure, rollback or abort when directly visible,
and post-commit publish start/success/failure when applicable. On a
consolidated success path, one post-commit record carrying each phase's
elapsed duration satisfies the success-path requirements; every failure record
remains mandatory.

A writer that batches multiple operations into one transaction may satisfy the
success-path requirements with one consolidated post-commit record per batch,
carrying the batch identifier, batch size, and every per-phase elapsed
duration, provided each member operation's own lifecycle records cite the
batch identifier after durable confirmation. A batch failure requires one
record per member with that member's local identifiers and the shared cause;
batch-level failure records supplement, never replace, them.

Every external service call (model inference, remote APIs) and startup smoke
check must log the target's role or identity, call purpose, start, success,
normal error, elapsed milliseconds, compact input shape facts, and configured
limits. Keep call payloads out of logs.

Every external process call must log executable identity, purpose, start,
configured timeout, process ID when available, completion status or timeout,
elapsed milliseconds, and bounded stdout/stderr diagnostics on failure.

Every spawned task must have durable visibility for start or acceptance, normal
completion, normal error, panic when detectable, and cancellation or join
failure when detectable.

Operation stream delivery is a boundary, not the backend outcome. Logs must
distinguish backend success from result delivery success, backend success from
result delivery failure, backend failure from error delivery success, backend
failure from error delivery failure, and client stream closure before terminal
delivery.

## Required Context

Include compact, local, safe facts that identify the boundary.

Do not invent facts. If an outcome is unknown because the process or transport
failed, log the last known authoritative boundary and make the unknown portion
explicit.

## Forbidden Log Data

Never log:

- authentication tokens or credentials;
- full content payloads (documents, messages, prompts, model outputs, token
  dumps);
- bulk numeric data (vectors, embedding matrices);
- unbounded stdout or stderr;
- large request or response payloads;
- secrets from configuration or environment.

Use compact identifiers, counts, dimensions, hashes, paths, statuses, elapsed
times, and bounded diagnostics instead.

## Implementation Guidance

Prefer local, direct fixes:

- add a start, success, or error log at the boundary owner;
- add missing local context before an error leaves a subsystem boundary;
- log durable transaction boundaries before and after commit;
- mirror startup progress to the service log after file logging is initialized;
- add task completion, panic, and cancellation visibility for spawned work.

Do not introduce a new observability framework, operation ledger, queue, retry
layer, fallback path, or schema change without explicit design approval.

The goal is diagnostic clarity, not log volume. A small number of specific
lifecycle logs is better than many generic lines.
