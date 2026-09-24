# Private dev output review

Status: implementation complete; repository verification running, round 1 recorded
before dispatch.

This is the bounded follow-on accepted on 2026-09-24, based on
`9cec7d36203424e057edb28d8ef9d9020d7dbbc1`. The six tooling slices are closed and
their review histories are unchanged. Work uses the authorized `main` checkout,
without a new worktree, remote, or issue tracker.

## Contract and assumptions

The user required private dev files from initial startup onward, isolated from
ordinary build and build-enabled check publication. Each session/version owns its
executable, generated JS bindings, WASM, and browser assets. Vite must resolve the
facade import to private bindings, not only select a private WASM file. Outputs are
worktree-local. Existing Cargo caches and per-project build serialization may remain
shared; concurrent compilation is not required.

Accepted configuration, Build identity, artifact paths, and cleanup ownership form
one internal development version, with named activation and restoration operations.
Compilation failures retain the working app; superseded candidates are discarded;
startup failures restore the old version. Files outlive the services that use them.
Keep prescribed-port replacement, bounded HTTP Build readiness, React/CSS HMR,
native restart/new Build, WASM-only reload without native restart, and process cleanup.
Release packages stay standalone/static. Portable application/client behavior and
selected wire behavior remain unchanged.

The regression must observe a real live dev browser while a separate build produces
different JS/WASM, then prove later Rust edits still work. Verification uses the
existing CLI/browser contracts and the required repository gate.

Linux and trusted local project configuration/hooks remain the supported boundary.
The user did not request sandboxing, simultaneous compilation, cross-worktree output
sharing, or persistence across SIGKILL. Authy, protocol expansion, generic Cargo
metadata refactoring, and broad runtime/dispatch redesign are excluded.

## Implementation

- `build::DevSession` owns `.snap/dev/<session>/`; the shared development builder
  creates complete private generations for initial startup and watched changes.
  Native-only rebuilds copy the previous private bindings and browser package.
- `Version` groups configuration, Build identity, and reference-counted generation
  ownership. `Running::activate` and `Running::restore` use that value. A WASM-only
  activation keeps the native process's original executable/assets alive.
- Vite resolves configured binding imports to private JS and switches binding/WASM
  paths together through its control channel. Its driver/cache are also private.
  It retains exposed generations until frontend shutdown to support older module
  requests. This deliberately costs disk space during long sessions. Unexposed
  failed/superseded candidates clean up immediately; session shutdown removes all
  remaining files.
- Standalone build/check keeps `.snap/build/{debug,release}` and configured binding
  publication. Dev never publishes or consumes those outputs. No dependencies added.

## Verification

The new browser regression failed against the baseline CLI. After a separate build,
the expected `dev-owned` string became empty on a fresh dev page. Its fixture uses
a command-local build-script environment variable to change a generated export from
a string return to a numeric return, without editing watched sources. The repaired
test passes. It also opens the standalone package and checks `73`, then proves
WASM-only and native edits still reload the private dev version.

Focused browser ownership, Rust-watch and HMR scenarios passed together in 35.1s
before the final standalone-package/native-edit assertions were added. Cargo build,
TypeScript checking and `git diff --check` passed. The full repository gate is
running against the final test.

## Review budget

This follow-on has its own two-round limit. Round 1 independently reviews
Standards and Spec at the same committed revision. A second round is available only
for validation of one repair batch. Closed milestone reviews are not reopened.

Model discovery confirms GPT-6 Astra is available. Reviewers inherit Astra under the
harness restriction on model overrides. The review assignment calls for close
attention to file lifetime, frontend switching, failure restoration, and interaction
with standalone publication.

Complete reports, reviewer sessions, immutable reviewed revisions and the finding
ledger will be added here after dispatch.
