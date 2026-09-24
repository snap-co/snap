# Project tooling before Authy

## Purpose and status

Prepare the Rust Snap spike for multiple applications and the Authy port without
turning Healthy's shortcuts into framework conventions. Work in independently
verifiable slices that fit a fresh agent session.

The command model, architectural direction, tooling choices, and reload policy below
are accepted conversation decisions. The user approved starting the six-slice plan
on 2026-09-23. This is a local plan, not an issue tracker or a new publication workflow.

Starting code revision: `ec2b34bd7bcc2bacb377da89943dcc0fcdda86b8`.
Work directly on this repository's `main`, without Factory or worktrees.

All six slices are complete and reviewed as of 2026-09-23. Latest reviewed code:
`e9f64499daad0b9e1790e38a6a3d4bedcf84076c`. The full repository gate passes.
Authy's first password/session flow is the next separately scoped piece of work.

## Accepted decisions

### Project commands

Support these commands from an application directory, or from a parent with a
project directory argument:

```sh
snap build [project]
snap build [project] --release
snap dev [project]
snap check [project]
```

Use the same upward discovery and nearest-config validation for all commands.
The optional argument selects a directory, not a name in a global registry.
Resolve project paths relative to its `snap.toml`. Five sibling applications must
not require a special parent workspace registry. Cargo workspaces remain supported.

`build` and `dev` call a shared Rust build implementation. Dev does not shell out
to the CLI's build subcommand. Build returns explicit artifact locations for the
launcher and packaging code. Development and optimized release builds are distinct
profiles. Starting a dev server does not require passing every style/test check.

Configuration selects native and WASM composition targets, browser inputs, and
exceptional executable-plus-argument hooks. Application Rust code stays statically
linked into the selected host. No dynamic Rust configuration loading is needed.

### Development reload policy

| Changed code | Required behavior |
| --- | --- |
| React/CSS | Real browser HMR, with React Fast Refresh where supported |
| Rust/WASM client | Rebuild, then full page reload |
| Native server | Rebuild, then process restart |

Use established browser tooling for HMR. Vite with its React integration is the
leading candidate; the particular tool has not been selected by implementation.
Bun can remain the package manager. Snap owns coordination and process lifetime.
Do not implement a custom React refresh protocol or Rust hot code replacement.

Successful rebuilds replace the running version. Failed compilation leaves the
previous working version available and reports diagnostics. Watch generated outputs
carefully so builds do not cause rebuild loops. Handle edits during a build without
publishing a mixed or obsolete application generation.

The user accepts rebuilding client state after a WASM page reload by resynchronizing
with the server. Healthy does not implement session/document repush yet. Authy's
contracts must establish that behavior. A server restart also loses in-memory
server state; tooling must not claim durable session preservation by itself.

### Organization and checks

- Keep portable application/runtime/client code `no_std` with `alloc`.
- Use modules to organize behavior. Add packages/crates for actual dependency,
  compilation-target, or independent-consumer requirements.
- Put concrete application selection in app-owned composition targets. Shared
  native/browser implementations must not accumulate application dependencies.
- Keep browser entrypoint/lifecycle reusable; no application-authored boot script
  that duplicates host startup.
- Give generated bindings/assets application-owned output locations.
- Prefer compiler and check diagnostics to a growing rules document. Failures should
  name the violated constraint and a useful next action.
- Project checks cover the selected project and relevant dependencies. Repository
  checks additionally cover Snap's cross-application/framework contracts.
- Reference TypeScript compatibility checks are explicit repository requirements,
  not implicit requirements of every independent Snap application.

Use existing formatting, Clippy, target compilation, and consumer contracts. Add
Cargo-metadata dependency rules, automatic coverage of declared portable packages,
Rustdoc checks, cargo-machete, and a small cargo-deny policy. Pin added tools and make
missing prerequisites actionable. Treat heuristic dependency findings deliberately.

Agent comment guidance should be short: document ownership, ordering, cancellation,
error/recovery guarantees, and non-obvious compatibility decisions beside the
relevant interface. Avoid comments that repeat syntax or mandatory prose on every
function. Tooling can enforce links and selected documentation requirements, not
the truth or usefulness of every comment.

## Starting implementation and evidence

- Eight workspace packages separate protocol, runtime, client, native/browser
  execution, WASM bindings, Healthy, and CLI tooling.
- `snap dev` already discovers config, runs ordered preparation, builds the native
  host and browser assets, replaces current-user listeners, and cleans child groups.
- `bindings/wasm` and the shared TypeScript facade select Healthy directly. Native
  application composition lives in the native package's examples.
- Rust CLI builds and shell release builds duplicate WASM/tool-install knowledge.
  Generated bindings currently use one shared TypeScript directory.
- Portable compilation in `bin/check` explicitly names Healthy and the client.
  It does not automatically cover a newly introduced portable application.
- The resident client accepts Start/Wake/Completed and returns Query/Wait/Stop.
  It cannot yet receive interactive user commands. Server handlers return immediate
  outcomes and do not yet support external-work completion inputs.
- Full verification passed for the previous milestone. Its two review rounds are
  complete and recorded in `docs/reviews/snap-dev.md`.
- During the subsequent architecture sanity check, bare-WASM compilation and
  `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` passed. The full
  behavior suite was not rerun for that read-only assessment.

## Implementation slices

### 01. Run Healthy through app-owned composition

**Blocked by:** None.

**Delivers:** Healthy still runs through dev, release browser, and native client
journey, with application selection and generated bindings owned by the application.

Acceptance:

- [x] Portable Healthy stays isolated from native and WASM platform dependencies.
- [x] Native and WASM composition select Healthy outside reusable platform packages.
- [x] Shared binding/facade support is reusable without importing the Healthy app.
- [x] React still loads an application definition through the reusable host.
- [x] Application outputs cannot overwrite a sibling app's generated bindings.
- [x] Existing consumer behavior and portable compilation pass after moving wiring.

Keep this a relocation and interface-ownership slice. Do not redesign the runtime
or introduce an app-plugin framework. Name packages/modules according to the actual
build and dependency requirements discovered during implementation.

### 02. Build a complete selected application with snap build

**Blocked by:** 01, for the intended app-owned artifact layout.

**Delivers:** `snap build` and `snap build foo --release` produce runnable native
plus browser artifacts; `snap dev` builds through the same implementation.

Acceptance:

- [x] Implicit, nested, and explicit directory selection follow the same rules.
- [x] Both native and WASM compilation use deliberate profile selection.
- [x] A shared internal result identifies the executable and browser artifacts.
- [x] A release directory runs on a compatible machine without the source checkout,
  Bun, or a Rust toolchain, matching the existing packaging promise.
- [x] Build performs no listener replacement or server launch.
- [x] Standard build/version/tool-install knowledge has one implementation; remaining
  shell entrypoints delegate instead of maintaining separate build recipes.
- [x] Ordered hooks preserve literal argv, config-relative cwd, failure status,
  cancellation, and descendant cleanup. Define shared-build versus dev-only hook
  semantics explicitly so a preparation command does not accidentally run twice.
- [x] Independent project fixtures prove selection, failure, and artifact behavior;
  Healthy proves the real native/browser package.

Select and document artifact layout and config amendments in this slice. Respect
custom Cargo target directories. Keep the existing local mise PATH workflow.

### 03. Run project verification with snap check

**Blocked by:** 02, for shared project/build setup and build-dependent contracts.

**Delivers:** `snap check` and `snap check foo` run the selected project's declared
verification, with useful diagnostics and a reliable exit status.

Acceptance:

- [x] Independent sibling projects select their own checks and artifacts.
- [x] A documented declarative check configuration combines standard checks and
  exceptional project commands without hidden shell interpretation.
- [x] Check prepares required artifacts through the shared builder as needed.
- [x] Failure, missing tools, interruption, and process cleanup are CLI contracts.
- [x] A project check neither launches a persistent dev server nor replaces a
  development listener. Network tests own their processes and ephemeral ports.
- [x] Healthy's relevant consumer contracts are available through project checking.
- [x] Repository-wide verification remains a clear entrypoint and does not become
  an accidental dependency of an independent application's check command.
- [x] Known required checks cannot silently skip and still report full success.

Define the first check selection and prerequisite model here. Do not build a generic
task scheduler. Retain existing assertions and test ownership from TESTING.md.

### 04. Make architecture and dependency mistakes produce feedback

**Blocked by:** 03.

**Delivers:** `snap check` reports relevant structural violations with an explanation
of what to change, while newly declared portable applications receive target checks.

Acceptance:

- [x] Project/package declarations identify portable and platform roles explicitly.
- [x] Cargo metadata checks enforce allowed dependency directions, distinguishing
  normal, development, and build dependencies and relevant target/feature contexts.
- [x] Portable target checks derive from declarations instead of a Healthy-only list.
- [x] Diagnostics identify the offending package/edge, rule, and bounded remedy.
- [x] Rustdoc checks, cargo-machete, and a minimal cargo-deny policy are integrated
  at the appropriate project/repository scope, with reproducible tool setup.
- [x] Broken project fixtures prove the important check diagnostics and exit status.
- [x] Brief agent guidance explains interface comments and points to commands that
  enforce architecture; current architecture/run documentation reflects the layout.

Keep policies specific to this repository's real constraints. Do not enable every
pedantic lint or ban all dependency-version duplication. Scope network-dependent
checks explicitly rather than making them an implicit prerequisite of every edit.

### 05. React and CSS HMR under snap dev

Human recovery ruling, 2026-09-23: Snap owns port selection and conflict handling.
It prescribes the frontend and backend addresses, replaces existing current-user
listeners by default, and checks readiness over HTTP instead of a log handshake.
Servers bind the supplied address or fail. The public default remains 3846; private
loopback defaults to 3847 with `dev.backend-address` / `SNAP_BACKEND_ADDR` overrides.
Port zero is resolved by Snap for isolated tests. User explicitly authorized this
repair and resuming the remaining plan. Keep the original review history and run
one additional, bounded recovery validation on the changed lifecycle contract.

**Blocked by:** 02. Recommended after 04; checks are useful but not a technical blocker.

**Delivers:** Editing Healthy's renderer or stylesheet updates the running browser
through established HMR tooling, without restarting its native server.

Acceptance:

- [x] Snap launches and cleans up the browser development server and native host.
- [x] Compatible React edits use Fast Refresh; CSS edits update without full reload.
  Tool-documented refresh fallbacks remain allowed.
- [x] Native server and Rust client lifetime survive representative renderer/CSS edits.
- [x] Browser requests and future session cookies retain coherent origin semantics.
  Define public address/proxy ownership before introducing a second listener.
- [x] Release artifacts remain static and do not require the development server.
- [x] HMR wiring works from both app-root and explicit-project invocation.
- [x] Browser/lifecycle contracts verify actual updates and shutdown on owned ports.

Choose the established browser tool here, starting with Vite/React evaluation. Keep
the app definition and reusable browser host model. A separate throwaway UI is not
needed; Healthy is the end-to-end tracer.

### 06. Rebuild Rust changes and restart or reload

**Blocked by:** 02 and 05, reusing the build implementation and browser reload channel.

**Delivers:** Rust edits automatically rebuild the affected targets; native changes
restart the server, WASM changes reload the page, and compilation failures preserve
the previous successful application.

Acceptance:

- [x] Watch app sources and relevant shared/path dependencies, manifests, and config.
  Exclude generated bindings, assets, and build/tool outputs from rebuild loops.
- [x] Debounce edits and handle changes during compilation; do not publish stale or
  partially built generations. Shared dependencies can invalidate both targets.
- [x] Native-only changes restart the server; WASM changes trigger full page reload
  after the relevant successful artifacts and server generation are ready.
- [x] Existing operation Build identity rules remain coherent across restart/reload;
  stale browser clients can recover rather than remaining stuck on build mismatch.
- [x] Build failures show useful diagnostics and leave the previous working version
  serving. Later successful edits recover without manually restarting snap dev.
- [x] Ctrl-C/SIGTERM clean watchers, build commands, frontend server, and native host.
- [x] Real edit/failure/recovery scenarios are tested through observable CLI/browser
  behavior, with bounded timeouts and fixture-owned processes.

Reuse file-watching machinery. State-preserving native/WASM hot replacement is
explicitly out of scope. Define recovery after a successful build whose replacement
process fails to start; do not imply that retaining old artifacts guarantees uptime.

## Sequence and Authy entry point

Recommended working order: **01 → 02 → 03 → 04 → 05 → 06**.
The real dependency branches are **01 → 02 → 03 → 04** and **02 → 05 → 06**.
Complete and verify one slice before beginning the next; report remaining scope and
record its commit/check evidence here. On 2026-09-23 the user authorized unattended
continuation through all six slices, with verification and review at each milestone.

Authy can begin after 04; HMR/watch work is a developer-experience improvement rather
than an authentication prerequisite. If desired, finish 05–06 first so the larger
port benefits immediately.

The first Authy implementation should be a separately scoped password/session flow,
selected after inspecting reference behavior. It must exercise user commands,
host-owned external work, completion inputs, and observable client session state.
Use native and browser consumers to prove the same portable behavior. Let that flow
drive the runtime interface changes rather than generalizing from polling alone.

Account setup, sign-in, and session observation are candidate tracer steps, not a
finished acceptance contract. Specify wire/cookie behavior, reload resynchronization,
and storage lifetime before implementation. Passkeys, reset, OAuth, and the rest of
Passport should follow as their own consumer-visible slices.

## Deferred

- Native/WASM state-preserving hot replacement.
- A general application plugin or dynamic Rust configuration mechanism.
- Full legacy CLI compatibility, deployment/infra commands, and project-version dispatch.
- Generic task orchestration and speculative package extraction.
- `snap check --watch`: desirable later, with selective cheap checks and explicit
  slow/network gates; not required for the first project-check command.
- TypeScript type generation from Rust: consider when Authy's growing binding contract
  establishes the need; preserve selected wire behavior when choosing tooling.

## Resume and verification

Read AGENTS.md, README.md, TESTING.md, and this plan. Inspect git status before edits.
Choose one uncompleted slice whose blockers are satisfied. If decomposition is still
unconfirmed, settle that first. Read applicable skill instructions for implementation,
tests, agent guidance, and review when performing that work.

Use compiler/architecture checks for structure and SDK/protocol/CLI/browser contracts
for consumer behavior. Do not add tests that merely mirror an internal refactor.
Run relevant gates during iteration and the repository milestone gate before handoff.
Update commands when composition targets move; preserve the existing contract suite.

Prior review evidence applies to the existing code revision, not these future slices.
Each new milestone needs its own bounded review record under the applicable workflow.
There is no remote or configured issue tracker; do not provision one as a side effect.

### Progress

Slice 01 is complete and READY. Implementation commit:
`8b8654fbdae3bfff25bef570c661264cf6342ea2`. Independent Standards and Spec reviews
were CLEAR in round 1, with no required repairs. One optional pre-existing browser
startup message cleanup is recorded for a later application integration.
Healthy now owns `native/`, `wasm/`, `client.ts`, its browser application declaration,
and generated bindings under `.snap/bindings/`. Shared runtimes and binding/facade
support no longer select Healthy. Native SDK assertions remain in their existing
files, compiled by the app-owned native package.

`mise exec -- ./bin/check` passed on 2026-09-23, including all eight CLI tests,
portable compilation, four native SDK tests, three browser SDK tests, the shared
three-client contract, native journey, two Chromium tests, and listener lifecycle.
TypeScript checking passed again after moving the ambient application declaration.
Cargo metadata confirmed no shared package depends on Healthy in any dependency
kind, and the portable Healthy dependency list is unchanged. `git diff --check`
passed. Review evidence lives in `docs/reviews/app-owned-composition.md`.

Slice 02 is READY at `70ed65479bb6cdbcec806dc43025b85672c0b072`, with both reviews
CLEAR in round 1 and no findings. Build packages live at
`.snap/build/{debug,release}`; dev consumes the same debug builder. Shared preparation
runs before dev-only preparation. The selected WASM dependency graph owns bindgen
version selection. Shell build wrappers delegate to the CLI, with `bin/build`
copying the complete release package to the compatibility `dist/` location.
The full `mise exec -- ./bin/check` gate and `git diff --check` passed. Review evidence
lives in `docs/reviews/snap-build.md`.

Slice 03 is READY at `ae7f22e6f214025c4c6a9e71808b3922a2ea685a`, both reviews CLEAR
in round 1 without findings. Reports are in `docs/reviews/snap-check.md`.
Project checks select Rust manifests, optional build prerequisites, and literal
commands. Healthy runs its SDK/browser checks independently of the reference suite.
Slice 04 is READY at `ab74cb9d05147c6732feaf1c070e6ad18f2b8624`. Round 1 found an
explicit-rlib rejection and separate-workspace optional-feature gap. Both are fixed,
with regressions and CLEAR round 2 validation. Reports are in
`docs/reviews/structural-checks.md`. Package roles, host/browser/bare-WASM dependency
graphs, and automatic bare-WASM compilation run through snap check. Repository
gates add pinned dependency tools; network advisory checking is explicit. Agent
guidance now points to structural diagnostics and meaningful interface comments.
Slice 05 was blocked at `da14da4e0fbcbda99da3d8b312913a8a7c17e714` after two review
rounds by an uncovered startup timeout regression. The user resolved the design
decision by assigning port selection/replacement and HTTP readiness to Snap. See
`docs/reviews/browser-hmr.md` for the preserved history and recovery evidence.
Vite/React serves the public development origin and proxies to an owned loopback
native host. Chromium proves state-preserving React/CSS updates and shutdown.
Human recovery is READY at `313bda57312803eff58e43ec185808f405f13861` under the
port-ownership ruling above. Full checks passed and both authorized recovery
validators returned CLEAR. STD-2 / SPEC-5 are resolved.

Slice 06 is READY at `e9f64499daad0b9e1790e38a6a3d4bedcf84076c`. Both final review
axes are CLEAR. Round 1 found missed separate-workspace manifests and newly created
ancestor Cargo configuration. Both were repaired and validated through a real CLI
contract without source edits. Reports are in `docs/reviews/rust-watch.md`.
Native/WASM dependency watching, private generation staging, Vite reload, compilation
failure retention and startup-failure restoration pass the real browser editing
contract. Full repository checks pass, including four Chromium scenarios. Native
generations reload browser clients to rediscover Build identity; WASM-only edits
retain the native process. Rust state-preserving hot replacement remains out of scope.
