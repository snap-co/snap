# Rust watching review

Final status: **READY** at `e9f64499daad0b9e1790e38a6a3d4bedcf84076c`. Both axes
CLEAR after two rounds. SPEC-1 and STD-1 / SPEC-2 are resolved; no follow-ups or
advisories remain. Complete round reports are archived in
[`rust-watch-reports.md`](rust-watch-reports.md).

Slice 06 of `docs/plans/project-tooling-and-authy.md`. Base
`579f1c3a630571bfbbb06e0c24354d8ac306af4b`. Linux, trusted local projects, HTTP-only
Healthy scope. The user authorized continuing all six slices and explicitly
resumed work after the slice-05 port-ownership recovery. That milestone is closed.
This milestone starts its own two-round review budget. No remote or issue tracker.

## Contract and implementation

Snap watches selected native/WASM packages and local/path dependencies discovered
through Cargo metadata, plus manifests, lockfiles, config and ancestor Cargo/toolchain
configuration. `notify` owns event delivery. Source directories are watched without
recursing through generated/tool/build outputs. Frontend extensions stay with Vite.
250ms quiet windows merge edits. Events during a build discard the candidate and
retain all outstanding target invalidations until successful activation. Failed
config/dependency/build attempts leave the working service running for later edits.

The shared builder handles both initial builds and reloads. Preparation remains
literal argv; build hooks precede dev hooks once at startup, before graph discovery.
Only build hooks rerun for edits. The project lock spans preparation and compilation.
Reloads stage bindings, static assets and native executables privately, then publish
only after successful compilation and startup. Native-only edits compile native;
WASM-only edits compile WASM/bindings/browser assets; shared inputs invalidate both.

Snap retains the prescribed native address across native restarts. Replacement has
a short outage. Failed replacement restores the previous executable and Build token;
failed restoration exits explicitly. Config changes can restart both services and
explicit address changes require navigating to the new printed URL. Native changes
get a fresh Build token unless SNAP_BUILD is set and reload the browser to rediscover
it. WASM-only changes preserve native lifetime/Build and reload after bindings publish.
Vite acknowledges its current asset generation over development HTTP before old
generation files are removed. Its stdin control handle is kept separate from
Tokio Child::wait, which otherwise closes stdin even when the wait is cancelled.

Initial builds still use .snap/build/debug; edit generations own .snap/dev directories
and delete superseded candidates on drop. Process groups own hooks, compilation,
frontend and backend. Interrupts during builds release both listeners and watchers.
Release builds remain static. No runtime application or selected wire protocol edits.

## Verification

`mise exec -- ./bin/check` passed on 2026-09-23: 11 dev, 7 build, 4 check, 5 architecture
CLI tests, all native/WASM SDK/protocol/journey checks, four Chromium tests, portable
compilation, Rustdoc, Clippy, dependency tools, both-port replacement and shutdown.
No skips. New notify dependency is in the existing tool-role package; structural
workspace checking passes. TypeScript passes. The Rust edit browser contract takes
about 23 seconds and the full gate about 95 seconds.

The owned editing fixture copies Healthy's application/native/WASM source into a
separate Cargo workspace and reuses the target/tool cache. Consumer assertions prove
native restart/new Build, WASM-only reload/no native restart, shared-code compilation
failure with the prior page/Build still serving, recovery, a gated build superseded
by a newer edit, startup-failure rollback, invalid-config recovery, and shutdown in a
manifest-triggered build hook. Existing React/CSS HMR state/lifetime checks pass.
During iteration this new contract caught Tokio wait closing the control pipe;
retaining ChildStdin separately fixed it. No lower-seam behavioral tests were added.

Round 1 recorded before dispatch. Standards and Spec reviewers inherit Astra under
the harness model-override policy. Review covers the fixed implementation delta and
required interactions, not a fresh audit of closed prior milestones.

## Round 1 findings and repair batch

Reviewed SHA `f195eae50a78c1d02e79b04328a0dcc49a1ec205`. Both axes BLOCKED.
Standards session `ses_f2e528dd8ffegDXGF6LAkPuLja`, complete report
`/tmp/opencode/rust-watch-standards-r1.md`. Spec session
`ses_f2e528da5ffez54Tc84n5yqUKm`, complete report
`/tmp/opencode/rust-watch-spec-r1.md`. The original unabridged reports and their
real-CLI reproduction scripts/logs remain at the reported paths.

| Finding | Accepted remedy |
| --- | --- |
| SPEC-1 | Ask Cargo for the workspace manifest of every reachable local/path package, including separate workspaces. Watch its parent for replacement saves. |
| STD-1 / SPEC-2 | Record ancestor `.cargo` and expected config paths even before they exist. Creation/replacement of the directory invalidates config and refreshes its watch. |

One repair batch complete. The added real-CLI regression serves a dependency's
inherited version and a compile-time Cargo-config environment value over HTTP. It
edits only the separate workspace manifest, then creates an absent ancestor config
directory and edits that config again. Source files stay untouched throughout.

Targeted CLI regression passed in 4.9s. `mise exec -- ./bin/check` passed after the
repair, now 12 dev CLI cases and all prior gates/four Chromium scenarios, no skips.
`git diff --check` passed. Round 2 recorded before dispatch; validate these findings
and repair-induced interactions only. This is the final authorized review round
for slice 06; unresolved or new blockers require human intervention.

Round 2 reviewed `e9f64499daad0b9e1790e38a6a3d4bedcf84076c`. Standards session
`ses_f2e528dd8ffegDXGF6LAkPuLja` returned CLEAR, complete original report
`/tmp/opencode/rust-watch-standards-r2.md`. Spec session
`ses_f2e528da5ffez54Tc84n5yqUKm` returned CLEAR, complete original report
`/tmp/opencode/rust-watch-spec-r2.md`. Both independently ran the actual CLI
regression, passing all three compiled-value transitions in about five seconds.
Build/diff checks passed. Full-suite results were supplied rather than repeated.
Directory replacement behavior was inspected; initial directory creation and later
file edits were exercised. No missing review evidence, skips, or unresolved findings.
