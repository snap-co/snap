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
