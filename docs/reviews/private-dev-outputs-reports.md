# Private dev output review reports

These are the complete leaf reports. Coordinator dispositions and verification
are in [`private-dev-outputs.md`](private-dev-outputs.md).

## Standards round 1

### Result

CLEAR for the Standards axis. I found no evidenced introduced regression, documented-standard violation, or maintainability defect requiring a repair in this bounded change. There are no BLOCKER, FOLLOW_UP, ADVISORY, or DECISION findings.

This assessment does not certify the pending repository gate. Its result remains for the coordinator to record.

### Reviewed revision and scope

- Base: `9cec7d36203424e057edb28d8ef9d9020d7dbbc1`
- Head: `fb1f33e37502f6499feb452fd41c157dd00268b9`
- Commit: `fb1f33e Isolate dev outputs and own active development versions`
- Checkout: `/home/cc444/code/snapco/snap`, authorized `main`
- Axis: Standards conformance and local maintainability, round 1 of the follow-on's separate two-round budget.

I inspected the complete committed diff across its eleven files. I read `AGENTS.md`, `README.md`, `TESTING.md`, the private-development-output follow-on in `docs/plans/project-tooling-and-authy.md`, and `docs/reviews/private-dev-outputs.md`. The checkout had an uncommitted coordinator update to the review record. I also read that record directly from the immutable head and excluded the uncommitted update from the code assessment.

Interaction coverage included `tools/cli/src/process.rs`, `tools/cli/src/watch.rs`, `tools/cli/src/config.rs`, `scripts/build-web.ts`, the project/server test adapters, `tests/browser/rust-watch.spec.ts`, and Playwright configuration. I also inspected the installed Vite module-graph invalidation code while tracing frontend switching. This was a bounded review of the changed ownership and publication behavior, not a repository-wide audit.

### Assessment

#### Ownership and build boundaries

`build::DevSession` and `Generation` give initial and watched builds the same private-output path. `build::development` copies the executable into each generation and directs generated bindings and browser packaging into that generation. Native-only candidates copy the previous private bindings and package. The standalone `build::run` path still publishes under `.snap/build/{debug,release}`, and build-enabled check uses that path.

The shared per-project lock still covers build work. Keeping Cargo caches and the embedded static-build driver shared is consistent with the accepted serialization boundary. Neither is a live dev asset dependency after packaging.

The Vite plugin resolves the facade's configured relative binding imports into private bindings. Launch arguments and reload messages carry the private JS and WASM locations together. The frontend driver and Vite cache now live inside a private generation. This addresses the actual JS import boundary as well as the separately served WASM file.

#### Version lifetime and recovery

`Version` groups accepted configuration, Build identity, and generation ownership. `Running::activate` and `Running::restore` name the transition and recovery operations. Compile failures and pre-activation supersession leave the accepted value available; native startup failure restores from that value.

The separate `host_files` reference preserves the native host's package across WASM-only changes. `browser_files` retains exposed generations for the frontend's lifetime. Unexposed candidates retain local ownership and clean up on drop. The session owner is declared before versions and services, and `Running` declares services before retained file owners. These relationships are documented beside the relevant types, as `AGENTS.md` requires. The README also records the intentional disk cost of retaining exposed generations.

The change continues to use the existing process-group ownership, prescribed-port replacement, bounded HTTP readiness, and Vite/React HMR mechanisms. It adds no crate or dependency and changes no portable application, client, or wire-protocol implementation.

#### Consumer regression and local maintainability

The new regression exercises real CLI publication and browser observations. A command-local build-script variable produces different binding ABIs without editing watched sources. The final committed test observes the standalone numeric result `73`, reloads dev pages expecting `dev-owned`, and then exercises WASM and native edits with Build-identity assertions.

Build invocation stays in `tests/adapters/project.ts`; server launch and cleanup use the existing adapter. Assertions concern rendered values, page lifetime, and HTTP Build identity rather than Rust helpers or private ownership counts. This follows `TESTING.md`'s consumer-interface guidance and tests a meaningful regression rather than mirroring the refactor.

The implementation stays within the existing build/dev modules. Its ownership comments explain constraints that are not evident from paths alone. I found no reason under the documented standards to require another abstraction, crate, or broader coordinator redesign.

### Verification

Checks I performed:

- Confirmed checkout HEAD equals the requested immutable head and inspected working-tree status.
- Inspected the committed diff and the interactions listed above.
- `git diff --check 9cec7d36203424e057edb28d8ef9d9020d7dbbc1...fb1f33e37502f6499feb452fd41c157dd00268b9`: passed.
- `mise exec -- cargo fmt --all -- --check`: passed.

Supplied evidence, not independently rerun:

- The ownership regression failed against the baseline CLI after standalone publication and passed after implementation.
- Focused dev-output, Rust-watch, and HMR scenarios passed together in 35.1 seconds.
- Cargo build and TypeScript checking passed.
- The focused run preceded the final standalone-package and native-edit assertions. The full `mise exec -- ./bin/check` was running against the final committed code when this review was assigned.

I did not run competing builds or browser fixtures while that gate was running. Recovery, browser behavior, and shutdown conclusions combine source inspection with the supplied verification; they are not additional independent runtime results. I made no product edits, commits, publication, or delegated review.

### Findings

None. No `STD-N` IDs are allocated, and no repair or broader-scope decision is requested by this axis. Final gate completion remains an outstanding verification item, not a confirmed code finding.

## Spec round 1

Status: **BLOCKED**

- Base: `9cec7d36203424e057edb28d8ef9d9020d7dbbc1`
- Head: `fb1f33e37502f6499feb452fd41c157dd00268b9`
- Scope: the committed private-dev-output follow-on and its affected build, process, and browser interactions.
- Findings: one BLOCKER, `SPEC-1`. No separate FOLLOW_UP, ADVISORY, or DECISION findings.

### Coverage

I read `AGENTS.md`, `README.md`, `TESTING.md`, the follow-on section of `docs/plans/project-tooling-and-authy.md`, and `docs/reviews/private-dev-outputs.md`. I inspected the complete implementation/test diff and the surrounding build, configuration, watch, process supervision, browser driver, static builder, and fixture code.

The main ownership design follows the accepted contract:

- Initial startup and watched candidates use session-local generation directories. Executables are copied out of Cargo outputs. Generated bindings and browser packages are private before launch.
- Standalone build and build-enabled check still use the separate publication builder. The shared lock spans preparation and build work.
- `Version` groups configuration, Build identity, and artifact ownership. Named activation and restoration use that value.
- The native service retains its own generation across WASM-only edits. The frontend retains exposed generations, including its initial driver/cache directory, until it stops. The documented disk cost matches this implementation.
- Failed and superseded unexposed candidates release their owned directories. Existing compile-failure retention, native startup restoration, Build-token rules, prescribed ports, and bounded readiness remain recognizable in the coordinator and process code.
- The new regression uses a separate CLI invocation and different binding ABIs. The final test also launches the standalone package, observes `73`, and exercises subsequent WASM and native edits.
- No new crate, portable runtime/client change, wire payload change, Authy work, or broad dispatch redesign appears in this diff.

One existing import form regresses at initial startup, as detailed below.

### SPEC-1: private binding remapping bypasses extension resolution

Disposition: **BLOCKER**
Severity: **medium**

#### Location

- `tools/cli/src/build.rs:166-177`, especially the private `bindings.as_deref()` argument at line 175, newly applies the binding-remap path to initial dev builds.
- `scripts/build-web.ts:24-28` returns the remapped filename directly. This code already existed, but initial dev startup now exercises it.
- `scripts/dev-web.ts:77-82` introduces the same unresolved-filename behavior in the live frontend resolver.

#### Trigger and contract

A facade imports its generated JS without an explicit extension, for example:

```js
import { marker } from "./.snap/bindings/example";
```

The generated file is `example.js`. This relative import works during initial dev startup at the base revision. At the head revision, private remapping returns an absolute filename ending in `example` and bypasses the bundler's normal `.js` resolution.

The accepted contract requires dev to resolve the facade's configured binding import to private JS while preserving application behavior. `README.md:126-127` explicitly describes resolving that same import to the private version. Neither the configuration interface nor that contract introduces a requirement to rewrite existing imports with explicit extensions. This is an evidenced regression, not a proposed import-style rule.

#### Evidence

I ran isolated probes using the actual committed scripts, with temporary files exclusively under `/tmp/opencode`, ephemeral frontend ports, and owned process cleanup. The fixture used an extensionless relative import and a matching `.js` file in both the configured and private binding directories.

1. Static build boundary:
   - The unchanged `scripts/build-web.ts`, called with the baseline initial-build arguments where binding source and output are the same, exited **0** and produced the package.
   - The same script, called with the head initial-build arguments selecting a private binding directory, exited **1** with `File not found ".../private-bindings/example"`.
   - The changed Rust startup/build flow selects this second path before launching dev. Thus this project now fails at initial compilation/packaging.
2. Live frontend boundary:
   - The base `scripts/dev-web.ts` transformed the import to `/.snap/bindings/example.js`; fetching that module returned **200** with the expected export.
   - The head driver transformed it to `/.snap/dev/session/generation/bindings/example`, without `.js`. Fetching that module failed. Vite logged `Failed to load url .../bindings/example ... Does the file exist?` and the request fell through to the fixture's unavailable backend, producing **502**.

The static remapper already affected watched rebuilds at the base revision. The introduced regression is that private remapping now breaks initial startup too, and the new Vite resolver independently repeats the problem. These are two parts of one finding, not two independent defects.

#### Bounded remedy

Preserve normal extension resolution after redirecting an import into the private binding directory. Apply the fix to both the static builder and Vite resolver; fixing only Vite still leaves initial dev builds failing. Keep resolution confined to the intended private output instead of falling back to published bindings. Extend a consumer-facing fixture to use an extensionless generated-binding import and verify startup plus a watched reload.

### Verification and limits

Checks I performed:

- Read-only comparison of the specified immutable base/head and affected call paths.
- Static-builder and live-driver HTTP module probes described above. All temporary files and processes were cleaned up.
- `git diff --check` for the specified base/head passed.
- Confirmed checkout HEAD equals the requested head.

Checks supplied by the coordinator, not rerun by me:

- The ABI-isolation regression failed against the baseline CLI after standalone publication, then passed with the implementation.
- Focused dev-output, Rust-watch, and HMR tests passed together in 35.1 seconds before the final standalone-package/native-edit assertions were added.
- Cargo build, TypeScript checking, and diff whitespace checks passed.
- The full `mise exec -- ./bin/check` was running against the final committed code. Its result remains for the coordinator to record.

I did not run competing builds, the full gate, or an additional full CLI/browser scenario. The finding's reproduction exercises the shipped static build script and live frontend HTTP module boundary; the initial CLI failure follows from the inspected call path. Process restoration and lifetime conclusions beyond these probes are based on code inspection and the supplied consumer-test evidence.

The checkout was initially clean. At the final status check, only `docs/reviews/private-dev-outputs.md` had an external working-tree edit; it is outside the immutable code reviewed here. I made no product changes, commits, publication, or further delegation. This report is the only retained review artifact.

## Standards round 2

### Result

**CLEAR.** The resolver and test repairs conform to the documented standards and preserve the consumer assertions. I found no repair-induced blocker or unresolved Standards finding. Accepted finding `SPEC-1` is resolved within this validation's scope.

This is the single final fix-validation round. No additional review round is requested.

### Revisions and coverage

- Original base: `9cec7d36203424e057edb28d8ef9d9020d7dbbc1`
- Prior reviewed head and repair-delta base: `fb1f33e37502f6499feb452fd41c157dd00268b9`
- Repaired head: `53ae50d17e84254f79a2454876e6dcf5b7861cbb`
- Worktree: `/home/cc444/code/snapco/snap`
- Axis: Standards conformance and local maintainability.

I confirmed the checkout was clean and HEAD matched the repaired revision. I reviewed only the seven-file repair delta and its affected interactions: the static and Vite binding resolvers, both changed browser contracts, the testing documentation and archived review evidence. I inspected the unchanged coordinator's activation, supersession, reload and acceptance-log ordering to assess the new test synchronization. I did not reopen the original whole-diff review.

### Finding dispositions

#### SPEC-1: private binding remapping bypasses extension resolution

- Previous disposition: BLOCKER, medium severity, accepted by the coordinator.
- Validation disposition: **RESOLVED**.
- Repair locations: `scripts/build-web.ts:23-28`, `scripts/dev-web.ts:77-87`, and `tests/browser/dev-outputs.spec.ts:26-63`.
- Contract: preserve existing facade import behavior while resolving generated JS to dev-owned files. The repair must cover both initial static packaging and live Vite module resolution without falling back to published bindings.

The static remapper now asks `Bun.resolveSync` to resolve the absolute redirected private path. It no longer hands Bun an unresolved extensionless filename. The Vite plugin delegates the private target to `this.resolve` with `skipSelf: true`, preserving Vite's normal extension handling without recursive remapping. A missing resolution raises an explicit error rather than returning control for resolution of the original published import.

The existing real CLI/browser contract now uses an extensionless facade import. It retains the distinct dev string and standalone numeric ABI assertions, separate standalone launch, fresh dev-page checks, and subsequent WASM/native edits. This covers both repaired boundaries through the consumer interface. The supplied pre-fix failure at packaging and post-fix repeated/full-gate passes support closure. I did not independently rerun those scenarios.

The bounded remedy is complete. No additional change is requested for this finding.

#### Standards ledger

Round 1 had no `STD-N` findings. This validation adds none. There are no unresolved BLOCKER, FOLLOW_UP, ADVISORY, or DECISION findings from the Standards axis.

### Test-repair assessment

At `tests/browser/dev-outputs.spec.ts:56-63` and `tests/browser/rust-watch.spec.ts:40-52`, `waitForFunction` retains the same page-lifetime condition while allowing Playwright to retry across navigation. The tests still require a healthy rendered page afterward and still assert the expected Build identity and native-start behavior. The change does not replace these outcomes with a delay or suppress a failing assertion.

At `tests/browser/rust-watch.spec.ts:75-82`, the added wait matches the existing CLI acceptance message to the exact `beforeFailure` Build before injecting a startup failure. The coordinator performs supersession checking and frontend acknowledgement before that message at `tools/cli/src/dev.rs:142-154`. This synchronization establishes the intended rollback precondition. The test still separately exercises edits during compilation, supersession, startup-failure restoration and HTTP Build recovery. `TESTING.md` explicitly permits CLI diagnostics and requires explicit readiness/completion signals; this wait follows that guidance.

The outer cleanup changes at `tests/browser/dev-outputs.spec.ts:68-73` and `tests/browser/rust-watch.spec.ts:107-112` close the page instead of starting another navigation. Nested `finally` blocks attempt server shutdown and fixture removal even if earlier cleanup rejects. The Rust-watch listener-release assertions remain in place.

The resolver changes reuse the owning bundlers' resolution behavior. They add no dependency or bespoke extension-search implementation. The nearby comment explains why missing private modules must fail. No Rust product changes or expanded protocol/runtime scope appear in the repair delta.

### Verification and limits

Own checks:

- Confirmed exact HEAD and a clean working tree.
- Inspected the repair diff, changed consumer assertions, and coordinator ordering described above.
- `git diff --check fb1f33e37502f6499feb452fd41c157dd00268b9...53ae50d17e84254f79a2454876e6dcf5b7861cbb`: passed.
- Read the preserved failure archive without extracting files. `test.trace` confirms `pw:api@48` failed with `page.evaluate: Execution context was destroyed` and `pw:api@49` attempted `goto(about:blank)` before eventually rejecting with `net::ERR_ABORTED`. This supports the stated navigation-race and cleanup repair.

Supplied verification, not independently rerun:

- The extensionless CLI/browser scenario failed pre-fix during initial packaging with `File not found .../bindings/healthy_wasm`.
- Ownership and Rust-watch scenarios each passed three repetitions, six of six total, in 1.6 minutes.
- Cargo build, TypeScript checking and staged diff whitespace checking passed.
- Full `mise exec -- ./bin/check` passed: formatting, Clippy, four portable packages, Rustdoc, dependency checks, 12 dev / 7 build / 4 check / 5 architecture CLI cases, SDK/protocol/journey/three-client compatibility, all five Chromium scenarios, and prescribed-port replacement/cleanup. No skips or environment limitations were reported.

I did not duplicate browser fixtures or full gates. Runtime closure relies on the supplied successful runs together with the inspected repairs; the trace inspection is independent confirmation of the earlier test failure, not a new runtime pass. I made no product edits, commits, publication, or delegation. This report is the only file I wrote.

## Spec round 2

Status: **CLEAR**

- Original base: `9cec7d36203424e057edb28d8ef9d9020d7dbbc1`
- Prior reviewed head and repair-diff base: `fb1f33e37502f6499feb452fd41c157dd00268b9`
- Repaired head: `53ae50d17e84254f79a2454876e6dcf5b7861cbb`
- Checkout: `/home/cc444/code/snapco/snap`
- `SPEC-1`: **resolved**.
- New repair-induced findings: none. No BLOCKER, FOLLOW_UP, ADVISORY, or DECISION remains from this validation.

### Scope and coverage

This is the single final fix-validation round. I inspected only the committed diff from the prior reviewed head to the repaired head and the interactions needed to validate those repairs. I did not reopen the original ownership review or the six closed milestones.

Coverage includes both binding resolvers, resolution into private outputs, extensionless and explicit-extension imports, generation switching, the updated output-ownership browser contract, the Rust-watch wait/acceptance ordering changes, cleanup changes in both affected tests, and the accompanying verification/review documentation. There are no Rust product changes in this repair diff.

### SPEC-1 resolution

Original disposition: BLOCKER, medium severity. Current disposition: **resolved**.

#### Locations and contract

- `scripts/build-web.ts:24-28` now resolves the redirected private filename through `Bun.resolveSync`.
- `scripts/dev-web.ts:77-88` now delegates the redirected private target to Vite through `this.resolve(..., { skipSelf: true })`. If resolution fails, it reports a private-binding error instead of returning control to resolution of the original published import.
- `tests/browser/dev-outputs.spec.ts:28` changes the real consumer facade to use an extensionless binding import.

The accepted requirement is to resolve the facade's existing import to dev-owned JS without losing ordinary extension resolution or consuming standalone publication. Both repaired boundaries meet that requirement in inspection and targeted probes.

#### Own verification

I used the scripts from the exact repaired commit in isolated fixtures under `/tmp/opencode`. The configured binding directory and private generations exported distinct markers. Frontend processes used ephemeral ports, and all temporary files and processes were cleaned up.

| Boundary | Case | Observed result |
| --- | --- | --- |
| Static private build | Extensionless import | Successful bundle containing `PRIVATE_ONE`, not the published marker |
| Static private build | Explicit `.js` import | Successful bundle containing `PRIVATE_ONE`, not the published marker |
| Static standalone build | Both import forms, source and output unchanged | Successful bundle containing the published marker |
| Static private build | Private module absent, published module present, both import forms | Build rejected the missing private module |
| Live Vite driver | Both import forms | Imports resolved to the private generation's `example.js`; module requests returned 200 and `PRIVATE_ONE` |
| Live Vite driver | Published file replaced while serving | Both import forms continued to return `PRIVATE_ONE` |
| Live Vite driver | Control-channel switch to a second private generation | Both import forms resolved to the second generation and returned `PRIVATE_TWO` |
| Live Vite driver | Requests naming the prior generation after switching | Returned 200 with `PRIVATE_ONE` |
| Live Vite driver | Private module absent, published module present, both import forms | Logged `Private binding module not found` for the private target; did not resolve the published module |

The first missing-module probe assumed an HTTP 500 response and stopped on an assertion because the response was 502. A focused follow-up showed that Vite emitted the intended private-resolution error, while the existing middleware callback forwarded the failed request to the deliberately unavailable fixture backend. This was an incorrect HTTP-status expectation in my probe, not evidence of fallback to published bindings. The required resolver behavior was confirmed for both import forms.

The supplied real CLI/browser regression adds the full generated-JS/WASM evidence. It failed before the fix at initial packaging with `File not found .../bindings/healthy_wasm`, then passed after the repair. Its final assertions retain the distinct standalone numeric ABI, private dev string ABI, and subsequent WASM/native edits. My script-level probes complement that evidence by checking both import forms and private-only resolution directly.

### Gate-repair assessment

The changed waits preserve the existing consumer promises:

- `waitForFunction` checks that the page-lifetime marker disappears across the expected reload. It allows Playwright to retry in the replacement document rather than failing because a single `page.evaluate` crossed navigation. The following rendered-status and binding-value assertions remain.
- Rust-watch now waits for `Rust generation ready: <observed Build>` before injecting the next startup failure. This distinguishes the accepted version from a candidate that has only reached HTTP readiness. It preserves the existing supersession and restoration expectations and uses the already documented CLI diagnostic boundary.
- Both affected tests close the page and use nested `finally` blocks so page-close failure cannot skip server cleanup and server-close failure cannot skip fixture cleanup. The Rust-watch listener-release assertions remain.

These changes address the reported test races without weakening the Build, reload, binding-isolation, startup-restoration, or cleanup assertions. I found no evidenced repair-induced regression or new scope requirement.

### Verification and limits

Own checks:

- Inspected the exact repair diff and relevant resolver/test interactions.
- Confirmed checkout HEAD is the repaired head and the working tree is clean.
- Ran the isolated static-builder and live-driver probes summarized above.
- `git diff --check fb1f33e37502f6499feb452fd41c157dd00268b9...53ae50d17e84254f79a2454876e6dcf5b7861cbb` passed.

Supplied verification, not independently rerun:

- The extensionless CLI/browser contract failed before the repair in approximately 0.7 seconds at packaging.
- Output-ownership and Rust-watch each passed three repetitions, six cases total in 1.6 minutes.
- Full `mise exec -- ./bin/check` passed, including formatting, Clippy, all four portable packages, Rustdoc, dependency checks, 12 dev CLI cases, seven build cases, four check cases, five architecture cases, native/browser SDK, protocol, journey, three-client compatibility, all five Chromium scenarios, and prescribed-port replacement/cleanup.
- Cargo build, TypeScript checking, and staged diff whitespace checks passed. No skips or known environment limits were reported.

I did not duplicate the full gate or run another shared-fixture build. I did not independently re-examine the archived Playwright failure trace; the test-repair assessment combines source inspection with the supplied trace diagnosis and passing repetitions/full gate. The isolated probes exercise the shipped bundler and frontend HTTP module boundaries, not a separate full browser/WASM application.

No product edits, commits, publication, or delegation were performed. This report is the only retained artifact from round 2. `SPEC-1` is closed, and this validation requests no further review round.
