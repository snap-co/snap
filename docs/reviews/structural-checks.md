# Structural checks review

Contract: slice 04 of `docs/plans/project-tooling-and-authy.md`. Base
`c7f8cc5f3f7a2358cebefe8fe5790f945a050014`. Linux host plus browser/bare WASM,
trusted config/source, main branch. Prior milestones' review rounds are complete.

Package metadata explicitly declares core/application/platform/binding/composition/tool.
Core/application libraries must compile on wasm32v1-none with all features. Normal
local dependency directions follow README's table. Development/build edges may use
host code, but shared roles cannot select application/composition under any kind.
Cargo-resolved host and wasm32-unknown-unknown graphs use all features. Local path
packages in the selected closure require roles; registry/git packages do not.

`check.architecture=true` enables structural checking during a project check.
`--structure-only` runs only graph/portability checks and `--workspace` expands that
mode to every workspace package. The repository gate uses workspace mode, replacing
the hardcoded Healthy/client portability list. Ordinary checks add warnings-denied
Rustdoc; workspace docs are also checked. Healthy's binary has doc=false to avoid
overwriting portable Healthy's library docs at the same Cargo target name.

Pinned cargo-machete 0.9.2 and cargo-deny 0.20.2 are installed by mise. Repository
`bin/check-deps` checks versions against those pins, unused dependencies, and minimal
source/wildcard policy. Duplicate versions and private path dependencies are allowed.
Documented machete exceptions retain wasm-bindgen-futures used by macro expansions.
`--audit` opts into network advisory checks, not part of ordinary verification. No
license policy is inferred. Added brief interface-comment and structural-check
guidance in AGENTS.md. HMR/watch remain later milestones.

Verification on 2026-09-23: mise tool installation succeeded; full `mise exec --
./bin/check` passed with 8 dev, 7 build, 4 check, 3 architecture CLI tests, compiler,
automatic portability, docs, dependency tools, and all SDK/protocol/journey/browser/
lifecycle gates. No skips. Workspace Rustdoc passed again after disabling binary
docs to remove the output collision. Initial architecture test failed before command
support existed. `git diff --check` passed. Optional network audit was not run.

Round 1 recorded before dispatch, two independent read-only Standards and Spec
reviewers inheriting Astra under harness policy. Two-round limit for this milestone.
Round 1 revision: `64715c08fcf4c6625b135b99dac15c915f4665e6`.
Standards session: `ses_f2f00814effeluRusXnB9vgOt5`.
Spec session: `ses_f2f008111ffeuahkeZ8vVssygu`.

## Finding ledger

| Finding | Disposition | Repair |
| --- | --- | --- |
| STD-1 / SPEC-2 | Accepted BLOCKER, same finding | Recognize Cargo's ordinary library kinds, including explicit rlib. CLI regression first reproduced rejection of a valid no_std library. |
| SPEC-1 | Accepted BLOCKER | Queue each discovered portable manifest for its own all-feature graph inspection before compilation. Follow newly discovered packages. Include bare-WASM metadata as well as host/browser so graph validation matches compilation. CLI regression first reproduced a false success across separate path workspaces; also cover bare-target-only optional edges. |

One batch of repairs is complete. Both new regression tests failed before repair.
Full `mise exec -- ./bin/check` passes after repair, now including five architecture
CLI tests and all existing gates without skips. The separate-workspace regression
also covers an optional bare-target-only edge. `git diff --check` passed.
Round 2 is recorded before dispatch and will validate these fixes and affected
interactions only; no third review or additional repair batch is authorized.

## Round 1 Standards report

## Standards axis: BLOCKED

Reviewed base `c7f8cc5f3f7a2358cebefe8fe5790f945a050014` → head `64715c08fcf4c6625b135b99dac15c915f4665e6`, commit `64715c0`, against AGENTS.md, README.md, TESTING.md, plan slice 04, and `docs/reviews/structural-checks.md`.

### STD-1 — BLOCKER, medium: valid portable `rlib` libraries are rejected

**Location:** `tools/cli/src/architecture.rs:137–145`, specifically the `"lib"` comparison at line 144.

**Requirement:** README.md:184–186 and the review contract require reachable core/application libraries to compile for `wasm32v1-none` with all features. They do not restrict libraries to Cargo's default crate type. Slice 04 also requires accurate diagnostics and a bounded remedy.

**Trigger:** A portable application declares:

```toml
[package.metadata.snap]
role = "application"

[lib]
crate-type = ["rlib"]
```

Cargo reports this library's target kind as `["rlib"]`. The checker accepts only `"lib"`, so it rejects the package before portability compilation and incorrectly recommends moving host binaries into composition packages.

**Independent evidence:** Fixture at `/tmp/opencode/slice04-standards-rlib`:

- `mise exec -- cargo check --lib --all-features --target wasm32v1-none --manifest-path /tmp/opencode/slice04-standards-rlib/Cargo.toml` passed.
- Cargo metadata confirmed the library target has `kind: ["rlib"]`.
- `mise exec -- ./target/debug/snap check /tmp/opencode/slice04-standards-rlib --structure-only` exited 1 with `portable packages need a library target`.

**Bounded remedy:** Recognize Cargo's library target kinds, including `rlib`, and retain the actual `cargo check --lib` portability check. Add a CLI consumer regression proving that an explicitly declared portable `rlib` passes.

### Coverage and checks

Inspected role declarations and direction enforcement, dependency-kind exceptions, host/WASM graph traversal, selected versus workspace scope, portable compilation, Rustdoc integration, pinned dependency gates, policy configuration, macro exceptions, CLI fixtures, and supporting documentation. No application behavior changes were found.

Independent checks:
- Built the reviewed CLI successfully.
- Ran the targeted reproduction above.
- `git diff --check` passed.
- Working tree remained clean.

Supplied evidence accepted: full `mise exec -- ./bin/check` passed with 8 dev, 7 build, 4 check, and 3 architecture CLI tests, plus structural, documentation, dependency-tool, SDK, protocol, journey, browser, and lifecycle gates with no skips. The post-collision-fix workspace Rustdoc rerun passed.

The full suite was not repeated. Optional network advisory checking was not run. No product edits, commits, publication, or delegated review occurred. No additional findings.

## Round 1 Spec report

## Spec axis: BLOCKED

Reviewed the fixed diff from `c7f8cc5f3f7a2358cebefe8fe5790f945a050014` to `64715c08fcf4c6625b135b99dac15c915f4665e6`, commit `64715c0`. Two confirmed accepted-requirement violations.

### SPEC-1 · BLOCKER · Medium
**All-feature portability compilation can introduce dependencies that escape architecture validation.**

- **Location:** `tools/cli/src/architecture.rs:75–90,184–189`
- **Requirement:** README structural rules and review contract require allowed dependency directions across all-feature graphs, including reachable local packages.
- **Trigger:** A selected application depends on a core package in a separate local workspace. That core package has a nondefault feature enabling an optional platform dependency.
- **Observed:** The real `snap check --structure-only` exited **0**, printing `Structural checks passed`. Its subsequent all-feature core compilation actually compiled the forbidden platform dependency. Cargo metadata's `--all-features` on the application manifest had not enabled the separate dependency package's nondefault feature. Selecting the core manifest directly caused the same CLI to exit **1** and report the forbidden normal edge.
- **Impact:** A forbidden dependency can pass the structural gate merely because its portable owner is reached through a path dependency rather than selected directly.
- **Bounded remedy:** Validate host/WASM metadata for each portable package under the same all-feature selection used by its portability compilation. Include newly reachable local packages in role/direction validation before reporting success.
- **Evidence:** `/tmp/opencode/slice04-spec-c9bhkppw/{selected,core-selected}.log`; reproduction script `/tmp/opencode/slice04-spec-probes.py`.

### SPEC-2 · BLOCKER · Medium
**A valid explicitly declared Rust library is rejected as lacking a library target.**

- **Location:** `tools/cli/src/architecture.rs:137–145`
- **Requirement:** Core/application libraries receive bare-WASM checks. The contract does not restrict them to an implicit Cargo library crate type.
- **Trigger:** A portable application declares `[lib] crate-type = ["rlib"]`.
- **Observed:** `cargo check --lib --all-features --target wasm32v1-none` exited **0**. The real `snap check --structure-only` exited **1**, claiming “portable packages need a library target.” Removing only the explicit crate-type declaration made the CLI pass. Cargo represents this valid library with target kind `rlib`, while the predicate accepts only `lib`.
- **Bounded remedy:** Recognize Cargo's library target kinds, including `rlib`, or let Cargo's `--lib` selection validate library availability. Retain rejection of packages that genuinely lack a library.
- **Evidence:** `/tmp/opencode/slice04-library-j6xutwmi/{bare-rlib,cli-rlib}.log`; reproduction script `/tmp/opencode/slice04-spec-library.py`.

## Coverage and verification

Read AGENTS, README, TESTING, slice 04, the review contract/evidence, and the entire fixed diff. Inspected interactions with Cargo package selection, configuration, process ownership, existing CLI fixtures, and repository wrappers. Coverage included role directions and dependency kinds, selected/workspace scope, portability, Rustdoc, pinned dependency gates, macro exceptions, and optional audit scope. No application behavior changes were found.

**Independent checks**
- Confirmed immutable revisions, single-commit scope, and clean working tree.
- `mise exec -- cargo build -p snap-cli` passed.
- Both targeted probes ran through the actual CLI under `mise exec`.
- Fixed-diff `git diff --check` passed.
- Compared baseline check code. Both findings concern newly introduced structural validation; neither is presented as a demonstrated failure of the baseline CLI.

**Supplied evidence accepted**
- Full `mise exec -- ./bin/check` passed without skips: 8 dev, 7 build, 4 check, 3 architecture CLI tests, structural/compiler/doc/tool checks, and SDK/protocol/journey/browser/lifecycle gates.
- Rustdoc rerun passed after the binary documentation collision fix.

The full suite and optional network audit were not rerun. No product edits, commits, publication, or delegation occurred. No additional follow-ups, advisories, or decisions.
