# Agent Rules

## Rule Precedence

When instructions conflict, follow this order:

1. Explicit user instruction in the current conversation.
2. Agent conduct rules in this file, including safety, scope, approval,
   filesystem, command, and patch process rules.
3. Repository technical policy in `PRINCIPLES.md` and diagnostics policy in
   `DIAGNOSTICS.md`.
4. Existing local code patterns, only when they do not conflict with the above.

## Verification Behavior

Follow `PRINCIPLES.md` for repository verification policy. This section defines
which verification steps agents run by default and which require explicit user
approval. For agent work, do not create or run automated tests unless the user
explicitly asks to re-enable testing:

- No Rust test modules, test functions, test fixture files, or integration tests.
- No `cargo test` unless explicitly requested.

Cargo checks are mandatory verification, not automated tests. After Rust
changes, run all appropriate Cargo checks:

- `cargo fmt`
- `cargo check`
- `cargo clippy`

Approval for Rust source edits includes permission to run these mandatory Cargo
verification commands and their normal formatting/build-artifact writes. This
does not authorize unrelated file writes or configuration changes.

Cargo checks remain mandatory after Rust changes and do not require separate
approval once the Rust change itself is approved.

`PRINCIPLES.md` describes the full verification policy. In agent work, Cargo
checks are the default required verification after Rust changes. Additional
verification from that policy that starts the server, depends on network access,
requires live external services, or otherwise changes runtime state requires
explicit user approval. When those checks are relevant but not approved, explain
the remaining risk instead.
If broader verification from `PRINCIPLES.md` is relevant but not approved or not
possible, report it as residual risk rather than treating Cargo checks as full
verification.

Do not start, stop, or restart the server unless the user explicitly asks.

Do not run network-dependent verification unless the user explicitly approves it.

If compile, format, or lint checks fail because of the approved change, fix those
errors without asking again unless the fix changes behavior beyond the approved
intent, expands scope, or requires a design decision.

When `cargo clippy` passes but still emits warnings, fix any warning introduced
by the approved change as part of that change. Report pre-existing warnings with
a proposed cleanup path and resolve them only with explicit approval. The same
behavior/scope/design exception above applies.

If verification cannot be run, explain exactly why and what risk remains.

## Code Rules

Rust code must use the project's Cargo edition and be formatted with
`cargo fmt`.

Every function must have a useful comment immediately before it explaining its
purpose or key invariant. Public Rust items should use doc comments. Comments
are not limited to functions; include them anywhere the code is not completely
intuitive or clear to someone unfamiliar with the codebase. Follow
`PRINCIPLES.md` `Code Comments`; do not write comments that merely restate
names, types, parameters, or obvious syntax.

Comment requirements are active review criteria, not passive style guidance.
Before final verification, inspect every edited region and decide whether the
code is locally understandable to a future maintainer who did not participate in
the current conversation. If not, add a concise comment explaining the intent,
invariant, ownership rule, lifecycle, or failure mode. Use Rust doc comments for
public items and ordinary comments for internal implementation notes. If non-function code
is obvious from names and structure, do not add a comment. Functions are the
deliberate exception: every function always carries a comment describing its
utility, even one that is currently obvious, because functions grow and a
once-obvious function can become non-obvious over time. The rule against
comments that merely restate names, types, or syntax still governs the content
of that function comment — it must describe purpose or invariant, not echo the
signature.

For every behavioral Rust edit, agents must decide before patching whether the
changed code introduces or relies on a non-obvious intent, invariant,
ownership/lifetime constraint, borrowing strategy, async/blocking boundary,
cancellation or shutdown rule, UI layout invariant, cache invalidation rule,
persistence/refresh coupling, ordering rule, error propagation policy, or
cross-module contract. If yes, include the explanatory comment in the same patch
as the code. Do not wait for the user to request comments, and do not leave the
explanation only in chat. Use Rust doc comments for public items and ordinary
comments for internal implementation notes. Comments should state why the code
exists or what must remain true, not restate the code mechanics.

Follow `PRINCIPLES.md` `External-Language Artifacts`: do not embed large
prompts, templates, scripts, HTML, JavaScript, CSS, or similar external-language
artifacts inside function bodies. Use dedicated files or named constants as
appropriate.

Follow `PRINCIPLES.md` for explicit error-handling policy. Do not introduce
`unwrap` or `expect` unless the use fits the allowed exceptions there and the
reason is clear at the call site.

Do not use compiler-silencing patterns to avoid ownership clarity. Clones,
`Arc`, `Mutex`, `RwLock`, boxed dynamic errors, and `allow` attributes must each
have a concrete reason.

Follow `PRINCIPLES.md` for async and blocking-work policy. Before introducing
or expanding async behavior, stop, explain why the change is needed under that
policy, and get explicit approval before implementing it.

Before applying a mechanical pattern, ask:

- Is this the right fix, or just a fix?
- Does the error reveal a deeper issue?
- Would a different approach be better?

If copying a code block 3+ times, extract to a loop or function.

Shared types/constants must be defined once and imported everywhere. If adding a
new case requires edits in multiple files, look for a single-source-of-truth
refactor.

## High-Risk Areas

These areas are high-risk. Before changing them, read the relevant code and
explain the intended change before editing.

- **Observability**: Raw request/response payloads are authoritative audit
  material. Never filter, narrow, reconstruct, rename, or omit observability
  fields except for explicit secret redaction. Derived views are additions, not
  replacements.
- **Configuration**: Config is strict and operationally significant. Missing
  files, missing keys, unknown keys, and missing required secrets are fatal
  errors.

## Debugging And User Feedback

The user and operator must be able to tell what happened. Do not leave
user-triggered work in an apparently idle or ambiguous state.

Use targeted operator logs as the primary debugging tool. Add logs when existing
logs do not explain operation start, boundary transitions, errors, completions,
or elapsed time. Do not add noisy logging.

User-facing feedback must appear inline in the CLI client output or streamed
operation events. Do not add modals, toasts, alerts, or tooltips unless the user
specifically asks for them.

Errors must preserve source context. Do not replace a specific provider,
config, or protocol error with a generic message.

## Diagnostic Hygiene

- Follow `DIAGNOSTICS.md` for day-to-day diagnostics policy and
  implementation standards.
- Before relying on diagnostics for a feature, identify the authoritative log or
  audit path from the applicable configuration, documentation, or
  implementation. If no authoritative path is documented or discoverable, treat
  the diagnostic record as unresolved and ask before proceeding.
- Before changing diagnostic behavior, identify the affected lifecycle or
  boundary logs and explain the intended change.
- When diagnostics policy requires adding or changing logs, include those log
  changes in the proposed plan and get approval before editing.
- Do not omit, narrow, reconstruct, or replace authoritative diagnostic records
  except for explicit secret redaction.
- If a likely failure cannot be diagnosed from durable logs after a change,
  treat the change as incomplete.

## Filesystem Boundary (Hard, No Exceptions)

This is a shared machine. The agent must not read from or write to anything
outside this repository.

All work must stay inside the project root. Do not inspect parent directories,
home directories, system directories, `/tmp`, or unrelated projects. Do not use
external paths for scratch files, temporary files, backups, exports, diagnostics,
or any other purpose.

The `specs/` directory is also off-limits unless the user explicitly asks for it.
Do not read from or write to `specs/`; it contains archived material that must
not be treated as current context.

Always run commands from the project root. If a command must target a
subdirectory, use an explicit working directory or `cd dir && command` for that
command only. Do not leave the repo root as the operating context.

## Commands And Git

Do not perform these actions unless the user explicitly instructs you:

- Kill processes.
- Start, stop, or restart servers.
- Run package/dependency management commands such as `cargo add`,
  `cargo update`, or dependency installation.
- Delete files.

Cargo commands are not an independent approval boundary. Once the task scope
and every relevant effect, including file writes, are approved, agents may run
the necessary Cargo commands without requesting separate permission. Existing
restrictions still govern effects produced through Cargo: starting a server,
changing dependencies, migrating data, or writing unapproved persistent state
requires approval for that effect. Ask about the effect, never for permission
to invoke Cargo itself.

Do not use git unless the user explicitly requests a git operation. If git is
explicitly requested, run it from the project root only and never commit secrets.
