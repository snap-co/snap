# Snap build review

## Contract

Slice 02 of `docs/plans/project-tooling-and-authy.md`. Base:
`1a6549a2ecbb779ec69695fa9c50e80e722c5601`. Work remains on main. The user authorized
unattended continuation through all six milestones on 2026-09-23.

Accepted implementation details:

- `snap build [directory] [--release]` uses the existing nearest-config discovery.
- Debug and release apply to native, WASM, bindgen output, and browser compilation.
- Packages at `.snap/build/{debug,release}` contain the native executable beside
  optional `web/`. Internal callers receive explicit paths. Dev consumes debug.
- `prepare.build` runs first; dev then runs `prepare.dev`. Commands are literal
  argv, in config-root cwd, once per invocation, with existing signal/group cleanup.
- Build does not start the server or replace listeners. Runtime `SNAP_ADDR` does
  not affect building. Compile failures preserve the prior completed package.
- A per-project lock rejects overlapping builds, including across profiles.
- Cargo artifact discovery respects target directories. The selected WASM normal
  dependency closure determines the exact wasm-bindgen tool version. Matching PATH
  tools may be reused; otherwise a versioned app-local tool cache is installed.
- Root Cargo workspace owns this project's wasm-bindgen pin. Shell wrappers share
  CLI bootstrap and build implementation. Legacy client/web wrappers perform the
  full release build; `bin/build` copies the finished package to `dist/`.
- Trusted source/config, Linux development, compatible-host release. Watching/HMR,
  project checks, cross-compilation of native hosts, and deployment are later work.

## Verification

Passed `mise exec -- ./bin/check` and `git diff --check` on 2026-09-23. Includes eight
dev CLI tests, seven build CLI tests, compiler/lint/portability gates, four native
SDK tests, three WASM SDK tests, shared TS/native/WASM assertions, protocol, native
journey, two Chromium dev/release tests, and dev listener lifecycle. No test skips.
The build fixture executes real native and WASM code to distinguish profiles and
runs a copied native package after deleting the original source and Cargo output.
The first native build and shared-hook tests failed before their implementation.
The real release build also exercised automatic bindgen tool installation.

## Review

Round 1 recorded before dispatch. Two independent read-only Standards and Spec
reviews inspect the fixed base and implementation commit. Agents inherit Astra;
the harness allows a model override only on explicit user request. Maximum budget
is two rounds for this milestone. Prior slice reviews are separate and complete.

Implementation: `70ed65479bb6cdbcec806dc43025b85672c0b072`.
Status: READY. Both axes CLEAR in round 1. No findings, repairs, or follow-ups.
Standards session: `ses_f2f176e04ffePI779xm6f2d34q`.
Spec session: `ses_f2f176ddeffeDw7xWOqETiCDXx`.

### Standards report

## Standards review: CLEAR

Reviewed the single commit `70ed654 Add project build command and share development packaging`.

- Base: `1a6549a2ecbb779ec69695fa9c50e80e722c5601`
- Head: `70ed65479bb6cdbcec806dc43025b85672c0b072`

### Findings

None. No evidenced introduced regression, documented standards violation, or unresolved decision found within this review's scope. No STD-N entries or follow-ups.

### Coverage

Read all 23 changed files against `AGENTS.md`, `README.md`, `TESTING.md`, slice 02, and the accepted detailed contract in `docs/reviews/snap-build.md`. Traced the relevant unchanged process-runner and native asset-discovery interactions.

- **Ownership and dependencies:** Build orchestration stays in CLI modules. No new crate or application dependency enters shared runtime code. The workspace owns the wasm-bindgen pin; portable application/runtime/client code is unchanged.
- **Shared build interface:** Build and dev consume the same Rust builder and explicit artifact paths. Listener replacement and host launch remain in dev. Shell entrypoints delegate through the shared bootstrap and builder.
- **Lifecycle and packaging:** Hooks execute under the project lock through the existing process-group runner. Shared preparation precedes dev preparation. Compilation completes in staging before replacing the profile package. Cargo artifact messages supply native/WASM paths, preserving custom target-directory support.
- **WASM/browser selection:** Version discovery traverses the selected package's normal dependency closure using WASM-filtered metadata. Profile selection reaches native compilation, WASM compilation, bindgen, and browser compilation.
- **Consumer-test line:** Added tests exercise the real CLI, independent projects, packaged executables, and generated browser/WASM output. Assertions cover observable behavior rather than Rust helpers. The dev fixture extraction preserves existing assertions. These follow the host CLI contract explicitly permitted by `TESTING.md`.

### Checks and limits

**Performed independently:**
- Resolved both supplied revisions and confirmed HEAD matches the reviewed commit.
- Inspected the complete base-to-head diff and commit list.
- Confirmed a clean working tree.
- `git diff --check BASE...HEAD`: passed.
- `mise exec -- bash -n bin/snap bin/dev bin/build bin/build-client bin/build-web bin/check`: passed.

**Supplied coordinator evidence:** Full `mise exec -- ./bin/check` passed, including 8 dev CLI tests, 7 build CLI tests without skips, formatting/Clippy/portability gates, native and WASM SDK contracts, shared health/protocol/journey checks, two Chromium tests, and listener lifecycle checks.

I did not repeat the behavior suite. This verdict covers the documented Linux, trusted-source/config, compatible-host contract and the changed code's necessary interactions. No product files were edited; no commits, publication, or delegation occurred.

### Spec report

## Spec review: CLEAR

Reviewed immutable revisions:

- Base: `1a6549a2ecbb779ec69695fa9c50e80e722c5601`
- Head: `70ed65479bb6cdbcec806dc43025b85672c0b072`
- Commit: `70ed654 Add project build command and share development packaging`

### Findings

None. I found no evidenced requirement violations, introduced regressions, or unresolved contract decisions within slice 02.

### Coverage

Read the complete `BASE...HEAD` diff, repository guidance, slice 02 acceptance criteria, and the accepted detailed contract in `docs/reviews/snap-build.md`. Inspected the existing process runner and native asset discovery where the changes depend on them.

Checked:

- Shared config discovery and Rust builder for build/dev.
- Debug/release selection across native, WASM, bindgen, and browser compilation.
- Explicit package paths, custom Cargo target-directory discovery, staging, stale-output removal, and preservation of completed packages after compilation failures.
- Shared preparation before dev-only hooks, literal arguments, cwd, failure propagation, and existing process-group cleanup.
- Per-project build locking across profiles.
- Build's separation from server launch, listener replacement, and runtime `SNAP_ADDR`.
- Selected WASM normal-dependency traversal, tool-version selection, app-local tool caching, and the shared workspace pin.
- Shell delegation, accepted full-release compatibility aliases, and `dist/` copying.
- Preservation of portable application/runtime/client behavior.

### Checks and evidence

**Performed independently:**

- Confirmed both revisions and the single-commit range.
- `git diff --check BASE...HEAD` passed.
- Copied Healthy's completed release package into `/tmp/opencode` and launched it from a temporary cwd with a clean environment and no tools on `PATH`.
- Through its ephemeral HTTP listener, verified successful HTML, JavaScript, WASM, and `health.up` responses. Checked WASM magic bytes and the health completion payload.
- Verified graceful shutdown with exit status zero.
- Confirmed HEAD remains the reviewed revision and the working tree is clean.

**Supplied coordinator evidence:** full `mise exec -- ./bin/check` passed, including eight dev CLI tests, seven build CLI tests without skips, compiler/lint/portability gates, SDK/protocol/journey contracts, two Chromium tests, and listener lifecycle checks.

### Limits

I did not repeat the full suite. The independent relocation probe used the existing completed release artifact; browser execution and source-removal fixture coverage rely on the supplied passing gates. Review scope was Linux, trusted project inputs, and compatible-host release packaging. No product files were edited.
