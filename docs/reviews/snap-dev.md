# Rust snap dev review

## Accepted contract

Implement the local Rust `snap` executable and declarative `snap.toml`. From
`apps/healthy`, `snap dev` discovers the application, builds its native host and
browser assets, and launches it. An optional project directory and upward discovery
from nested directories preserve the useful old command behavior. Resolve paths
relative to config and fail on the nearest invalid config. Configured commands
are trusted project tooling, with literal argument arrays, project-root cwd,
ordered execution, and failure stopping startup. The CLI owns all child groups,
including build/preparation work, and handles Ctrl-C/SIGTERM and exit codes.

The CLI builds before replacing current-user port listeners using the existing
development policy. It supplies a fresh development Build identity unless overridden.
Cargo selects and builds the configured target; the CLI does not load application
Rust code. Browser compilation still uses Bun and the current pinned wasm-bindgen.
The current supported platform is Linux, consistent with the existing runner.

The user additionally requested a directory-local PATH override. Use existing mise
configuration to prioritize target/debug here and in subdirectories. Preserve the
global TypeScript launcher outside this checkout. Remain on main, with no Factory
or worktrees.

Explicit exclusions: client-CLI framework, deploy/infra, creation commands, hot
reload/file watching, project-version dispatch, dynamic Rust plugins, stable hook
RPC protocols, and a general cross-platform process runtime. This is the first
development-command slice, not full legacy CLI compatibility. The declarative
browser configuration describes the current React/WASM host's asset conventions.
Local config, hook executables, source/build tools are trusted. Ordinary bad config,
failed builds/hooks, process interruption, and missing tools are supported failures.

## Revisions and evidence

Base: `82ffd387b1f945caf02cbc0c4ada23400953ebdd`.
Implementation SHA recorded after committing this contract.

`./bin/check` passed before review: Rust formatting, strict Clippy, no_std bare-WASM
gate, native and CLI builds, six CLI black-box tests, four native SDK contracts,
WASM/release builds, TypeScript checking, three browser binding contracts, shared
health contract through reference/native/WASM adapters, protocol contracts, native
journey, two Chromium checks including snap-dev startup, and listener replacement
and shutdown. `git diff --check` passed. The warmed full gate takes about 20 seconds.

Directory-local PATH was separately verified with `mise exec` from Healthy and
the parent of this checkout. They resolve target/debug/snap and the existing
global .bun/bin/snap respectively. The latter was not edited.

Tools and Chromium are installed. A cold wasm-bindgen installation may take longer.
Temporary probes belong under ignored `.tmp` or `/tmp/opencode`; generic system
temporary storage previously hit a quota. Tests own ephemeral ports. This repo has
no remote, publication workflow, or issue tracker.

## Review budget

This is a new implementation milestone, separate from the completed Healthy review.
Round 1 is recorded before dispatch. Independent Standards and Spec reviews will
inspect the same immutable commit. At most one subsequent fix-validation round
is allowed. The coordinator owns all edits. The harness forbids explicitly choosing
a subagent model without a user selection, so reviewers use the configured default.

## Standards report

Pending.

## Spec report

Pending.

## Ledger and readiness

Pending round 1.
