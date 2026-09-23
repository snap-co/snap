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
Implementation: `a4996d75ed289d55132f9b84ca0b25dc07f45ea3`.
Commit: `Add Rust snap dev with project config and local PATH selection`.
Diff: `git diff 82ffd387b1f945caf02cbc0c4ada23400953ebdd...a4996d75ed289d55132f9b84ca0b25dc07f45ea3`.

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

Round 1 session: `ses_f2f6fdb93ffeWIsiT2mtLVM6A2`.

### Complete report

**Axis: BLOCKED.** One confirmed test-process cleanup violation. No additional
architectural or consumer-test-seam violations found.

Revisions: base `82ffd387b1f945caf02cbc0c4ada23400953ebdd`, HEAD
`a4996d75ed289d55132f9b84ca0b25dc07f45ea3`, commit `Add Rust snap dev with project
config and local PATH selection`. Reviewed the complete diff and log against
AGENTS, README, TESTING, and this record. A concurrent working-tree change appeared
in the review document; findings concern the committed implementation.

**STD-1 — BLOCKER — CLI contract timeout leaves build processes running**

Location: `tests/cli/dev.py:39–41`; cleanup at `:36–37`.
Requirement: TESTING.md:45–49 requires fixtures to own and release processes.
The accepted milestone includes ownership of build/preparation commands.

`run_cli` uses `subprocess.run(..., timeout=30)`. Python kills and waits for only
the immediate snap process on timeout. Its active command has a separate process
group established at `tools/cli/src/process.rs:75–89`. Killing snap prevents Rust
cleanup, so commands and descendants can survive. tearDown only deletes the fixture.

Reproduced through the existing DevContract.run_cli helper by supplying a temporary
cargo executable opening an ephemeral listener and stalling, shortening the helper
timeout to two seconds, observing TimeoutExpired, and confirming the Cargo stand-in
still listened afterward. Probe result:
`{"run_cli_timed_out":true,"cargo_descendant_still_listening":true}`.
The probe killed the surviving group and removed its temporary fixture afterward.

Bounded remedy: explicitly manage the subprocess. On timeout send SIGTERM and allow
more than the CLI's six-second grace before escalation. Collect output and reap it.
Forced fallback must account for owned command groups. Verify with a stalled-command
fixture at the CLI interface.

Coverage:

- Independent CLI crate justified by executable/IO dependencies; concerns use modules.
- Application/runtime/client/binding execution seams unchanged; no new std dependency
  in their source-level dependency graph.
- Declarative config, literal command arguments, project-root cwd.
- Native target selection uses config and Cargo artifact messages.
- Browser retains documented React/WASM conventions, Bun, pinned wasm-bindgen.
- Behavior tests use actual executables, independent Cargo projects, browser, or HTTP;
  they do not import private runtime details.
- SDK contracts remain shared; launcher changes stay in adapters.
- Mise PATH is checkout-relative and leaves the global launcher unchanged.
- Listener replacement follows successful builds and targets current-user listeners.

No FOLLOW_UP, ADVISORY, or DECISION findings.

Actually run: diff --check passed; all six Python CLI contracts passed both through
mise exec and directly; targeted timeout probe reproduced STD-1. Full bin/check,
including compiler/portability gates, native/CLI/release builds, SDK/protocol,
Chromium, listener lifecycle and PATH verification, was supplied, not rerun.
Probe used a stalled Cargo stand-in, not an actual compiler hang, and the existing
helper with only a shorter timeout. No product edits, development-port/global
launcher changes, commits, publication, or delegation.

## Spec report

Round 1 session: `ses_f2f6f87b7ffeEY7uoG1izpwS1l`.

### Complete report

**Axis: BLOCKED.** One confirmed launcher regression requires revision. No other
accepted-behavior violations confirmed. Source report:
`/tmp/opencode/snap-dev-spec-round1.md`.

Base `82ffd387b1f945caf02cbc0c4ada23400953ebdd`, implementation
`a4996d75ed289d55132f9b84ca0b25dc07f45ea3`, one commit `Add Rust snap dev with
project config and local PATH selection`. Reviewed three-dot diff, log, AGENTS,
README, TESTING, this record, changed CLI/config/build-driver/launcher/tests, and
necessary native host/reference TypeScript discovery/dev interactions. Coordinator
review-document edits were left intact; no product changes.

**SPEC-1 · BLOCKER · The checkout launcher loses mise's toolchain environment**

Location: `bin/dev:5–11`, especially the direct exec on line 11. Subsequent Cargo
lookup at `tools/cli/src/build.rs:23–24`.

Requirement: preserve the supported local launch flow. AGENTS directs development
through bin/dev; README retains that command with mise-managed Rust. Previously
the wrapper and browser-build helper ran Cargo through mise when installed.

Scenario: mise and Bun are on PATH, configured Rust is installed through mise,
but neither mise activation/shims nor rustup's Cargo is on PATH. The wrapper builds
the CLI through mise, then executes it in the original environment. Snap cannot
find Cargo. Reproduced from checkout root on an ephemeral port:

```sh
env PATH=/usr/bin:/bin:/home/cc444/.local/share/mise/installs/bun/latest/bin \
  SNAP_ADDR=127.0.0.1:0 TMPDIR=/tmp/opencode ./bin/dev
```

Initial Cargo build succeeded, Bun checked dependencies, then startup exited 1 with
`snap: Could not launch "cargo": No such file or directory (os error 2)`.

Bounded remedy: use `exec mise exec -- "$root/target/debug/snap" dev ...` when mise
is selected, retain direct execution otherwise, and verify on an ephemeral port
with Cargo available only through mise. No FOLLOW_UP, ADVISORY, or DECISION findings.

Coverage:

- Scope, declarative parsing, nearest-config discovery, invalid-config precedence,
  and config-relative paths match the slice.
- Preparation has ordered argv, project cwd, inherited environment/output, failure stop.
- Configured Cargo packages/targets and artifact paths work; independent fixture
  covers non-Healthy target and custom target directory.
- Browser remains Bun/pinned bindgen and documented React/WASM conventions; CLI
  uses std without changing portable application/client code.
- Groups cover hooks/builds/probes/host; shutdown and exit paths match contract;
  executed tests cover forced descendant cleanup and SIGINT forwarding.
- Builds precede replacement; fresh Build and override exercised; listener policy
  retains current-user/original-PID selection.
- Mise selects local Rust inside Healthy and global TypeScript outside checkout.

Actually run: commit/diff inspection and diff --check passed; six CLI contracts
passed in 6.217s; restricted-PATH probe reproduced SPEC-1; two independent fixture
runs gave distinct Build tokens; failed compilation with reviewer-owned ephemeral
listener returned 101 with compiler diagnostic and left listener usable; prep hook
received SIGINT and its exit 23 propagated; failed-compile diagnostic/source matched
direct Cargo; mise command selection was verified inside/outside checkout.

Full bin/check was supplied, not rerun, including compiler/portability, builds,
SDK/protocol/journey, two Chromium tests and listener lifecycle. Browser/live
replacement also rely on code inspection. Cold bindgen install was not exercised.
All probes used .tmp or /tmp/opencode and ephemeral ports. No port3846/global
launcher changes, edits, commits, publication, or delegation. SPEC-1 is the only
requested revision.

## Ledger and readiness

STD-1 accepted. Repair batch changes the test launcher to allow cooperative shutdown,
then cleans its dedicated Linux process session if forced shutdown is necessary.
A stalled-hook regression covers responsive and frozen CLI cases, including an
ephemeral listener proving descendants stop. Existing manual signal tests now use
the same cleanup fallback.

SPEC-1 accepted. The wrapper now uses mise for both compilation and CLI execution.
A regression starts the real wrapper with Cargo absent from PATH but mise/Bun
available, waits for an ephemeral listener, and verifies graceful shutdown.

All eight CLI tests pass after both fixes. Full `./bin/check` and `git diff --check`
also passed after the repair batch. The warmed gate now takes about 40 seconds due
to deliberate timeout/forced-cleanup checks. No production Rust code changed in
this batch; the only production delta is the wrapper's execution environment.

## Round 2: fix validation

Recorded before dispatch. Resume the same reviewers against the committed repair,
bounded to STD-1/SPEC-1 and regressions introduced by the launcher/fixture changes.
This is the sole permitted validation round. No broader review or additional
repair round is planned.

Validated repair: `c5bdea85e0d46c2fb17ec30c4e6acb9916f68d86`, commit
`Preserve mise launch environment and clean up timed-out CLI tests`.

### Standards validation report

Session: `ses_f2f6fdb93ffeWIsiT2mtLVM6A2`.

**Axis: CLEAR. STD-1 is resolved.** No fix-induced standards regression within this
bounded validation. Previous revision `a4996d75ed289d55132f9b84ca0b25dc07f45ea3`;
repair `c5bdea85e0d46c2fb17ec30c4e6acb9916f68d86`. Reviewed complete repair diff
and accepted ledger, covering STD-1, revised fixture cleanup, and SPEC-1 interaction.

STD-1 satisfies TESTING.md:45–49:

- `tests/cli/dev.py:64–80` explicitly launches Popen in a dedicated session, requests
  cleanup before re-raising timeout, closes streams and removes session survivors.
- `:31–39` sends TERM and allows eight seconds, exceeding the CLI's six-second grace,
  while draining output and waiting for the process.
- `:18–28` supplies Linux session-wide forced cleanup, including separate groups.
- `:184–202` verifies listener release with responsive and SIGSTOP-frozen CLI.
- Existing descendant-interruption fixture `:164–182` uses the same cleanup.

All cases passed. Both cooperative shutdown and fallback address the original
failure. SPEC-1 interaction also passed: bin/dev:6–11 executes through mise, and
`:204–235` verifies initial Cargo absence, real wrapper readiness and shutdown on
an ephemeral port. The test did not skip and no standards issue was found.

Actually run: repair diff --check passed; `mise exec -- python3 tests/cli/dev.py -v`
passed all eight tests without skips in 24.429s, including both timeout paths,
descendants, and restricted-PATH wrapper. Full post-repair bin/check was supplied,
not rerun, including compiler/portability, builds, SDK/types, protocol/journey,
Chromium, and listener lifecycle.

This sole fix-validation did not reopen the full review. No production Rust changed.
No further Standards revision requested. No product edits, commits, publication,
delegation, development-port use, or global-launcher changes occurred.

### Spec validation report

Session: `ses_f2f6f87b7ffeEY7uoG1izpwS1l`.

**Axis: CLEAR. SPEC-1 is resolved.** No concrete regressions in repair batch or
affected interactions. Previous `a4996d75ed289d55132f9b84ca0b25dc07f45ea3`; repair
`c5bdea85e0d46c2fb17ec30c4e6acb9916f68d86`. Inspected repair diff, log, reports,
ledger. Scope was SPEC-1 and wrapper/test-cleanup changes, with no production Rust.

SPEC-1 resolved: bin/dev:6–11 compiles and executes through mise when available;
the other branch retains direct execution. Cargo installed through mise remains
available to application builds. `tests/cli/dev.py:204–231` restricts PATH to system
tools plus mise/Bun, asserts initial Cargo absence, starts real wrapper, waits for
Healthy's ephemeral listener, terminates it and verifies exit 0. Passed without skip.

STD-1 interaction resolved in exercised cases: `:64–80` starts dedicated sessions
and requests shutdown on timeout; `:31–39` allows eight seconds vs CLI's six;
`:18–28` kills session survivors including separate groups. `:184–202` passed with
responsive/frozen CLI and verified stalled listener release in both cases. Existing
forced-descendant cleanup also passed.

No partial/unresolved findings or new BLOCKER/FOLLOW_UP/ADVISORY/DECISION findings.
Actually run: repair diff/log/ledger inspection; diff --check passed; direct
`python3 tests/cli/dev.py -v` passed all eight without skips in 24.467s. Coverage
included restricted PATH, both timeouts, descendants, help/version, discovery,
invalid config, literal hooks/failure ordering, real Cargo target/environment/exit.

Full post-repair bin/check was supplied, not rerun. Non-mise branch was inspected
rather than separately executed. Cold tool install was not exercised. Tests used
temporary projects and ephemeral ports; port3846/global launcher untouched. No
product edits, commits, publication, or delegation. No further Spec revision required.

## Final readiness

**READY** on code revision `c5bdea85e0d46c2fb17ec30c4e6acb9916f68d86`.
Both axes are CLEAR; STD-1 and SPEC-1 resolved. Required verification passed.
No remaining blockers, decisions, advisory findings, or independent follow-ups.
Two rounds used. This documentation-only update records their final results.
