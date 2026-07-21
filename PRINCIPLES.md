# Principles

## Core Principles

These principles are dual requirements: the system must produce accurate,
complete information, and the operational evidence behind that information must
be preserved. Accurate data that cannot be audited is untrustworthy. Visible
data that is not accurate is misleading. Neither condition alone is sufficient.

- **Simplicity**: Proven patterns over cleverness. Simplicity means
  straightforward implementation, not reduced capability.
- **Transparency**: Do not hide operationally relevant data. Summaries may
  supplement raw data, but must not replace or obscure it. Secrets are the
  exception.
- **Accuracy**: Do not present guesses as facts. Use authoritative data. If
  only an estimate is possible, label it clearly and never use it as a
  correctness source of truth.
- **Quality**: Do not patch symptoms when the source of truth is wrong. Fix the
  authoritative behavior. If a narrow fix is unavoidable, state why and do not
  disguise it as a structural solution.
- **Separation**: The server owns logic; the client owns presentation. The
  server is authoritative for data access, validation, and domain logic. The
  client owns interaction and UI, and must not implement, duplicate, or override
  server logic.
- **Code quality over compiler satisfaction**: Fix errors to improve the code,
  not merely to silence the compiler or satisfy borrow checker pressure.
- **No hidden fallback logic**: Do not add silent fallback paths that mask broken
  primary behavior. Fail clearly or surface degraded behavior explicitly.
- **Avoid needless async**: Async is allowed only at genuine async boundaries:
  axum handlers, streaming HTTP bodies, network calls, and timers. Keep
  synchronous work synchronous. Never propagate async to match surrounding code
  or to prepare for speculative concurrency.

## Observability Is Lossless

Observability is not a summary layer. It is a lossless record of what happened.

Rules:

- Raw external request and response payloads are authoritative audit material
  and must be preserved complete, redacting only secrets, in a durable audit
  medium designated by the owning service's specification. They need not
  appear in the user interface. What may appear in operator logs is governed
  separately by `DIAGNOSTICS.md`; when log rules exclude payload content, the
  designated medium, not the log, carries the authoritative record.
- Do not replace raw payloads with summaries, projections, or cleaned-up shapes.
- Do not copy only known fields into narrower view models when round-trip
  fidelity matters; unknown fields must survive the pipeline or remain available
  in the retained raw payload.
- Derived displays are allowed only as additions alongside the raw data.
- Sanitization is allowed only for secrets; omission for convenience is
  forbidden.

Operational losslessness means logs preserve enough boundary facts to
reconstruct what happened without guessing. Each user-triggered operation must
record start, meaningful boundaries, terminal success or failure, and the local
facts available at each error boundary.

Missing expected lifecycle logs are diagnostic failures. If an operation can
fail without leaving enough evidence to identify the failed boundary and
available context, the implementation is incomplete.

Summaries are acceptable only after authoritative boundary facts have been
recorded.

## Source Of Truth

- Each kind of data has one authoritative source of truth. Derived, cached, or
  generated representations are produced from it, never hand-maintained as a
  stale parallel copy.
- Every stated fact must trace to the source of truth. Derived estimates,
  metadata, or counts are never presented as authoritative answers.

## Separation Of Concerns

- A stateless server may receive full client-supplied context per request; this
  never makes the client authoritative for a server-owned concern.
- Client-side state exists only for presentation, optimistic display, or local
  interaction; it is never a source of truth.

## Code Comments

- Every function must have a useful comment immediately before it explaining
  its purpose or key invariant. Public Rust items should use doc comments.
- Comments are not limited to functions. Include them anywhere the code is not
  completely intuitive or clear to someone unfamiliar with the codebase.
- Comments must explain intent, invariants, side effects, ownership boundaries,
  failure modes, or non-obvious design constraints. Do not write comments that
  merely restate names, types, parameters, or syntax.
- Add brief comments at semantic boundaries inside functions when behavior is
  not obvious from local code alone: validation, normalization, persistence,
  external calls, external-payload transforms, blocking boundaries, truncation,
  and intentional error behavior.
- When data changes meaning across a pipeline, comment the boundary where the
  meaning changes.
- Update nearby comments when changing code. Stale comments are bugs.

## External-Language Artifacts

Large external-language artifacts do not belong inline inside application
function bodies. Prompts, templates, scripts, HTML, JavaScript, CSS, and
similar artifacts must live in dedicated files or named constants with clear
names.

- Application code owns execution: error handling, safety limits, and domain
  mapping stay in Rust.
- Inline literals are acceptable only when they are short, local, and clearer
  than indirection.

## Rust Design Rules

- Use typed structs for data shapes owned by this application: config, internal
  state, limits, domain events, and stable API/event contracts.
- Preserve raw `serde_json::Value` or retain the original raw payload at
  external protocol boundaries where unknown fields or byte-for-byte semantics
  matter.
- If a provider payload is parsed into typed structs, the raw payload must still
  be retained or logged for audit unless the project has explicitly decided the
  unknown fields are irrelevant.
- Prefer explicit `Result` handling. `unwrap` and `expect` are panic paths: they
  extract `Ok` or `Some` values, but panic on `Err` or `None` instead of
  preserving normal error flow. Use them only for startup fail-fast, controlled
  verification code, and locally proven invariants with clear comments. In
  ordinary runtime code, handle or propagate errors explicitly so callers and
  logs keep the source context.
- Do not use compiler-silencing patterns to avoid ownership clarity. Clones,
  `Arc`, interior mutability, and boxed dynamic errors must each have a concrete
  reason.
- Shared process state should be small, explicit, and stable. Use `Arc` for
  intentionally shared server state, not as a default escape hatch.
- Keep pure transformations synchronous and independently reviewable without
  axum, Tokio, or network access.
- Cross-file behavioral contracts must be compile-time safe where possible. Use
  shared constants, enums, or structs rather than duplicated strings and
  assumptions.

## Async And Blocking Work

Async boundaries must be intentional.

- Async is appropriate for axum handlers, response streaming, network calls,
  timers, cancellation-aware coordination, and other operations that naturally
  suspend.
- Do not make synchronous code async just to match a helper, copied pattern, or
  speculative future need.
- Config parsing and pure protocol transformations should remain synchronous
  unless there is a concrete reason otherwise.
- Blocking work must not run directly on Tokio worker threads. Reach it through
  `spawn_blocking`, a dedicated worker, or another explicit blocking boundary
  when called from async runtime code.
- If avoiding async would introduce more complexity than async itself, document
  that tradeoff at the boundary.
- Expanding async across a call chain changes ordering, error handling,
  cancellation, and reentrancy. Treat it as an architectural change, not a
  mechanical refactor.

## Configuration

- Operational configuration lives in `config.toml`.
- Secrets live outside version control (environment, secrets file, or similar)
  and must never be committed.
- Missing config files, missing required keys, unknown keys, and missing required
  secrets are fatal startup errors.
- Never add runtime defaults for operational settings. Defaults belong in
  `config.example.toml`, where the operator can see and edit them.
- Environment overrides must be explicit and documented. They must not silently
  change correctness, data source, model, safety limits, or audit behavior.
- Config parsing should use `deny_unknown_fields` and avoid `serde(default)` for
  required operational settings.

## Protocol Boundaries

- External request/response shapes are protocol data. Preserve the fields
  required for round-trip correctness.
- Never narrow external responses into lossy structs unless raw-payload
  retention preserves auditability and the narrowed shape preserves the protocol
  contract.
- Events streamed to clients must remain stable, explicit, and easy to audit.
  Adding or changing event fields is a cross-boundary change.
- Streamed events must make terminal state explicit: every stream ends in a
  clear terminal success or error event.

## Structural Fixes

- Fix behavior at the authoritative source. Do not patch symptoms locally when
  the source of truth is wrong.
- When logic is spread across multiple places, consolidate toward one source of
  truth instead of adding another branch or copy.
- If a second issue appears in the same subsystem, stop and reassess the
  structure before continuing.
- If a narrower fix is chosen instead of the structural fix, explicitly explain
  why.
- Do not add branching, fallback paths, duplicated logic, or local special cases
  when the correct fix is to clarify ownership.

## Verification

- Automated tests are not currently part of this repository's verification
  workflow unless they are explicitly re-enabled.
- The absence of automated tests does not weaken correctness requirements.
  Verification still includes formatting, compilation, linting, strict
  configuration validation, startup and runtime guard checks, timeout behavior,
  terminal stream events, and clear diagnostics.
- Pure logic should still be written so it can be checked or tested later
  without starting the HTTP server, Tokio runtime, or network dependencies.
- Do not add test modules, test functions, test fixture files, or integration
  tests unless automated testing has been explicitly re-enabled.
