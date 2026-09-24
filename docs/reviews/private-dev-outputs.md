# Private dev output review

Status: repair batch complete and repository gate passing. Round 2 recorded before
dispatch for bounded fix validation.

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
TypeScript checking and `git diff --check` passed. The first full gate and subsequent
repair evidence are recorded below.

## Review budget

This follow-on has its own two-round limit. Round 1 independently reviews
Standards and Spec at the same committed revision. A second round is available only
for validation of one repair batch. Closed milestone reviews are not reopened.

Model discovery confirms GPT-6 Astra is available. Reviewers inherit Astra under the
harness restriction on model overrides. The review assignment calls for close
attention to file lifetime, frontend switching, failure restoration, and interaction
with standalone publication.

Round 1 reviews `fb1f33e37502f6499feb452fd41c157dd00268b9` against the base above.
Standards session: `ses_f2b083a55ffeJK7KTECHHFpQMV`.
Spec session: `ses_f2b0839bdffe1NR3RFDmDmbLih`.
Both received the same fixed contract and supplied verification evidence. Full-gate
results were pending and identified as such in both assignments. Complete
reports and the reconciled finding ledger will be added after delivery.

Round 1 delivered Standards CLEAR and Spec BLOCKED. `SPEC-1` is accepted: private
remapping bypasses normal extension resolution in the static builder and Vite.
Extensionless generated-binding imports worked at baseline initial startup but
fail at this revision. Repair both resolvers and exercise an extensionless facade
import in the real output-ownership browser contract. This and the gate repairs
below are the single round-1 repair batch; round 2 will validate only that delta
and affected interactions.

The complete round-1 reports are archived in
[`private-dev-outputs-reports.md`](private-dev-outputs-reports.md). No independent
follow-ups or advisories were reported; no tracker was needed or created.

## Gate repair evidence

The first full gate passed formatting, Clippy, portability, docs, dependency policy,
all 28 CLI cases, native/browser SDK, protocol, journey, and four Chromium scenarios,
including the final output-ownership regression. Rust-watch then failed during its
first expected native reload. The lifecycle smoke command was not reached.

The preserved trace `/tmp/opencode/private-dev-outputs-gate-failure.zip` gives the
direct cause. `test.trace` call `pw:api@48` rejected `page.evaluate` with "Execution
context was destroyed" at the reload boundary. Its enclosing `expect.poll` aborted.
Cleanup call `pw:api@49` tried `goto(about:blank)` during navigation and stalled until
the test's 180-second deadline. Browser console/network/snapshots show the new page
booted and continued healthy polling. No product stall or JS/WASM mismatch appeared.

The existing assertion raced navigation; the new output regression copied the same
pattern. Both now use Playwright's navigation-tolerant `waitForFunction` for the same
page-identity promise. Cleanup closes the page and uses nested `finally` blocks so
page errors cannot skip server/source cleanup. The failed full gate is the red
evidence; its precise trace makes speculative hypotheses or a test of Playwright
internals unnecessary. Focused repeat and the full gate will verify this repair.

The first focused repeat passed five of six cases, then exposed another test-ordering
race. The gated native edit had reached HTTP readiness but had not logged acceptance
when the test injected its next failure. Snap correctly discarded that superseded
candidate and restored the last accepted version, which differed from the test's
provisional `beforeFailure` token. The contract now waits for the matching CLI
`Rust generation ready` signal before injecting the startup failure. Consumer
assertions and the product's acceptance order are preserved.

## Repair batch verification and round 2

The extensionless-import variant of the real browser contract failed against
`fb1f33e` at initial packaging with `File not found .../bindings/healthy_wasm`.
The static builder now runs Bun's normal resolution on the redirected private path.
Vite delegates the redirected path to its normal resolver and fails explicitly if
the private module is missing, without falling back to shared published bindings.

After the resolver and test-timing repairs, all six focused repeated cases passed:
`mise exec -- bunx playwright test tests/browser/dev-outputs.spec.ts
tests/browser/rust-watch.spec.ts --repeat-each=3`, in 1.6 minutes. Cargo build and
TypeScript checking passed. Then `mise exec -- ./bin/check` passed in full: 12 dev,
7 build, 4 check and 5 architecture CLI cases; native/browser SDK, shared
three-client contract, protocol and journey; all five Chromium scenarios; Clippy,
formatting, portability, Rustdoc, dependency checks and listener replacement/cleanup.
No skips or known environment limits remain.

Round 2 is the only fix-validation round. Validate `SPEC-1`, the browser wait/cleanup
and acceptance-signal repairs, and affected interactions. The original ownership
implementation has no further Rust changes. Do not repeat a whole-diff review.
