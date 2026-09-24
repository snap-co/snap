# Rust watching reviewer reports

Unabridged reports retained with the ledger in `rust-watch.md`.

## Standards, round 1

# Slice 06 Standards review, round 1

Result: **BLOCKED**. One confirmed violation of the documented watch contract.

## Revisions and scope

- Repository: `/home/cc444/code/snapco/snap`.
- Base: `579f1c3a630571bfbbb06e0c24354d8ac306af4b`.
- Head: `f195eae50a78c1d02e79b04328a0dcc49a1ec205`.
- Commit: `f195eae Watch Rust dependencies and activate recoverable dev generations`.
- Reviewed `git diff 579f1c3a630571bfbbb06e0c24354d8ac306af4b...f195eae50a78c1d02e79b04328a0dcc49a1ec205`, the changed implementations and their necessary configuration/process interactions.
- Read `AGENTS.md`, complete `README.md` and `TESTING.md`, the accepted plan including slice 06, and complete `docs/reviews/rust-watch.md`.
- Standards axis only, using the assigned Astra reviewer. Linux, trusted local projects, Healthy HTTP-only. The closed slice-05 readiness/port amendment was treated as an existing contract, not reopened.

The review covered dependency placement and portability boundaries, watch discovery and exclusions, hook ordering and process ownership, generation publication/recovery, static build compatibility, and consumer-interface test ownership. IO remains in the tool crate; `notify` is added to the existing tool-role package. The added behavioral assertions exercise CLI/browser interfaces rather than Rust internals. No architectural boundary or test-seam violation was found in this delta.

## Findings

### STD-1: Newly created ancestor Cargo configuration is never watched

Disposition: **BLOCKER**. Severity: medium.

Location: `tools/cli/src/watch.rs:145-155`, with event classification at `tools/cli/src/watch.rs:197-220`.

Accepted rule: `README.md:173-178` promises watching ancestor Cargo/toolchain configuration and refreshing the watch graph for new directories. The accepted plan's slice-06 criterion at `docs/plans/project-tooling-and-authy.md:254-255` requires watching relevant configuration. The review contract also explicitly includes ancestor Cargo configuration. No startup-only existence restriction is documented.

Supported trigger: Start `snap dev` with a selected package under an ancestor that has no `.cargo` directory. Create `<ancestor>/.cargo/config.toml` while development is running, for example to set `build.rustflags`.

Evidence:

1. Watch setup only registers `.cargo` and its config paths when `cargo.is_dir()` is already true. Ancestors themselves are watched nonrecursively.
2. The creation event for an ancestor's `.cargo` directory is neither a registered manifest/config path nor a path under a selected package root. Classification ignores it. No refresh discovers the new config directory, so subsequent file edits there are also missed until another watched edit happens.
3. An independent real-CLI probe reproduced this on an isolated server-only Cargo project under `/tmp/opencode`, using port zero. Creating a valid ancestor `.cargo/config.toml` produced no rebuild diagnostic over three seconds and left HTTP Build identity unchanged:
   - Initial and after creation: `standards-probe-1790224331855961264`.
   - `Rebuilding Rust:` count after creation: `0`.
4. A source edit then changed Build to `standards-probe-1790224336071485938`. After that graph refresh, editing the same ancestor config changed Build to `standards-probe-1790224337532397761`. These positive controls establish that the CLI was responsive and that the missed event depends on the directory being absent at startup.

Impact: Development silently retains a generation compiled with the old Cargo configuration. Configuration changes alone do not recover it. This is a gap in the newly introduced watcher, not a reopening of prior readiness or port behavior.

Bounded remedy: Register expected ancestor config paths regardless of initial existence, and classify creation/replacement of their `.cargo` directory as a graph-refresh event so the directory gets its own watch. Cover the initially absent ancestor-directory case through a real CLI contract that observes successful rebuild/Build identity after creating valid configuration.

Probe script: `/tmp/opencode/rust-watch-standards-probe.py`.
Probe CLI log: `/tmp/opencode/rust-watch-standards-probe.log`.

No independent FOLLOW_UP, ADVISORY, or DECISION findings are recorded.

## Verification and limits

Supplied evidence, not rerun here:

- Full `mise exec -- ./bin/check` passed with no skips, about 95 seconds. Reported coverage includes 11 dev, 7 build, 4 check, 5 architecture CLI tests; four Chromium scenarios; SDK/protocol/journey, portability, Rustdoc, Clippy, dependency tools, and listener lifecycle checks.
- Targeted TypeScript/Clippy and workspace structural checks passed. The added Rust-edit browser scenario reportedly takes about 23 seconds.

Independent checks:

- Resolved both full commit IDs and confirmed HEAD equals the reviewed head. Working tree was clean before and after inspection.
- `git diff --check <base>...<head>` passed.
- `mise exec -- cargo build -p snap-cli` passed and reported the current dev build up to date.
- Ran the isolated real-CLI probe twice, including once after that build verification. Both reproduced STD-1 with source/config positive controls. The probe owned its temporary project and process session, requested an OS-assigned port, and terminated its CLI in cleanup. It did not replace a default-port listener.

No product files were edited, no tests were added to the repository, and no commit, publication, tracker action, or delegation was performed. The full suite and browser scenarios were not repeated; their supplied success does not cover the initially absent ancestor-config case. This report is bounded to the pinned delta and documented Standards contracts, not an exhaustive Spec verdict.

## Spec, round 1

# Slice 06 Spec review, round 1

Result: **BLOCKED**. Two confirmed watch-discovery gaps violate the accepted input-watching contract.

## Revision and scope

- Base: `579f1c3a630571bfbbb06e0c24354d8ac306af4b`.
- Head: `f195eae50a78c1d02e79b04328a0dcc49a1ec205`.
- One commit: `f195eae Watch Rust dependencies and activate recoverable dev generations`.
- Reviewed `git diff <base>...<head>`. HEAD matches the supplied head and the working tree is clean.
- Read `AGENTS.md`, `README.md`, `TESTING.md`, the accepted plan including slice 06, and the complete `docs/reviews/rust-watch.md` contract and verification record.
- Reviewed dependency discovery, event classification/debounce, initial and watched builds, generation ownership, replacement/restoration, Build identity, browser controls/acknowledgement, process supervision, static-build interactions, and the new browser contract/adapters.
- Scope is Linux trusted local projects and Healthy's HTTP behavior. The closed slice-05 HTTP-readiness/prescribed-port amendment is accepted. This review does not reopen it or expand protocol/application scope.
- Performed the assigned Spec axis only, without delegation or product-file edits.

## Findings

### SPEC-1 · BLOCKER · Medium: path dependencies' inherited workspace manifests are not watched

**Location:** `tools/cli/src/watch.rs:81-87,101-116,140-147`.

**Accepted rule:** Plan slice 06, lines 254-255, requires watching relevant shared/path dependencies and manifests. `README.md:173-178` and the review contract also promise Cargo-metadata-based manifest watching and graph refresh.

**Trigger:** A selected package uses a path dependency that belongs to a separate Cargo workspace. The dependency inherits a setting from that workspace's root `Cargo.toml`, and that root is outside the selected project's ancestor chain and outside the dependency package directory.

**Evidence:** `Sources::new` records `workspace_root/Cargo.toml` only for the selected native/WASM metadata invocations. For each local dependency it records the dependency's own manifest and recursively watches that package directory. It never discovers or watches the dependency's separate workspace root manifest.

The isolated actual-CLI probe used `project/` with a path dependency at `depworkspace/member/`. That dependency inherited `version.workspace=true` and exposed `env!("CARGO_PKG_VERSION")` through the fixture's HTTP response. Editing `depworkspace/Cargo.toml` from version `0.1.0` to `0.2.0` produced no rebuild in the three-second observation window. HTTP still returned the original Build and dependency version `0.1.0`. A subsequent edit to the selected package's Rust source triggered a rebuild and returned `0.2.0`. The unchanged Build and zero rebuild diagnostics before the positive control match the missing watch registration in the code.

**Impact:** Valid manifest edits that change the selected application's dependency configuration leave the old application serving indefinitely until another watched input changes. Inherited dependencies, features, and package settings can be affected.

**Bounded remedy:** Include workspace manifests that contribute inherited configuration to reachable local/path packages, including separate workspaces. Watch their containing directories so replacement saves are covered. Add a CLI consumer regression that edits only the dependency workspace manifest and observes the resulting application change without a source touch.

### SPEC-2 · BLOCKER · Medium: creating an ancestor `.cargo` directory is invisible to the watcher

**Location:** `tools/cli/src/watch.rs:149-160,195-220`.

**Accepted rule:** Plan slice 06, lines 254-255, requires config watching. `README.md:173-178` and `docs/reviews/rust-watch.md:11-14` explicitly include ancestor Cargo configuration.

**Trigger:** While `snap dev` is running, create `.cargo/config.toml` in an ancestor of the project where `.cargo` did not exist at startup. The ancestor is outside the discovered package roots. This is a normal way to introduce shared Cargo configuration for a workspace or group of projects.

**Evidence:** Ancestor directories receive nonrecursive watches, but `.cargo` and its config paths are registered only if `.cargo` already exists. The later directory-creation event is neither a known manifest/config path nor inside a package root, so `classify` discards it. No watcher is installed inside the new directory.

In the same isolated actual-CLI probe, creating the project's parent `.cargo/config.toml` with `[env] SPEC_PROBE_CONFIG="new"` caused no rebuild in the three-second observation window. The fixture used `option_env!` and continued returning `config: "old"` with the same Build. Editing the selected Rust source then caused a rebuild and HTTP returned `config: "new"`, proving that Cargo uses the new configuration but the watcher missed its creation.

**Impact:** New ancestor Cargo configuration does not apply automatically, and subsequent edits inside that newly created directory remain unwatched until some other input causes discovery to run again.

**Bounded remedy:** Treat creation/replacement of an ancestor `.cargo` directory as a configuration invalidation and refresh its watches. Track the expected config paths even before they exist. Add a CLI regression covering creation of an initially absent ancestor `.cargo/config.toml` and automatic application of its build configuration.

## Verification and evidence

Supplied implementation evidence, accepted as supplied rather than independently rerun:

- `mise exec -- ./bin/check` passed in about 95 seconds, including 11 dev, 7 build, 4 check, 5 architecture CLI cases, four Chromium cases, SDK/protocol/journey, structural, Rustdoc, Clippy, dependency-tool and lifecycle checks, with no skips.
- Targeted TypeScript and Clippy passed; the new browser edit contract takes about 23 seconds.
- The existing contract covers native/WASM/shared edits, failed-compilation retention, superseded builds, replacement-startup rollback, config correction and interruption during a manifest-triggered hook.

Independent checks:

- Resolved both full revisions, confirmed the one-commit range and clean working tree.
- `git diff --check <base>...<head>` passed.
- `mise exec -- cargo build -p snap-cli` passed in 0.03 seconds with no compilation needed, confirming the probe used the current built CLI.
- Ran `mise exec -- python3 /tmp/opencode/rust-watch-spec-probe.py` against the actual CLI. Both missed-input failures reproduced; both source-edit positive controls rebuilt and exposed the changed values over HTTP.
- Probe script: `/tmp/opencode/rust-watch-spec-probe.py`.
- Owned fixture and CLI log: `/tmp/opencode/rust-watch-spec-u9ns6z8t/cli.log`.
- The fixture used an isolated Cargo target directory and requested port zero. Snap selected `127.0.0.1:45443`; the probe never invoked default-port replacement. Cleanup terminated the CLI with status 143 and released its owned process session.

## Limits and disposition

The full suite and Chromium contracts were not repeated. The independent probes target input-discovery gaps absent from the supplied scenarios; code-path analysis establishes why those inputs cannot produce events at the required paths. No broader platform, security, protocol or state-preservation guarantees were inferred.

No FOLLOW_UP, ADVISORY or DECISION findings. SPEC-1 and SPEC-2 are introduced missing slice-06 behavior with bounded remedies, not pre-existing milestone-05 issues. No tracker action was taken. No product files were changed, and nothing was committed or published.

## Standards, round 2

# Slice 06 Standards review, final fix validation

Result: **CLEAR**. STD-1 is resolved. No repair-induced Standards blockers were found in the bounded review.

## Revisions and scope

- Repository: `/home/cc444/code/snapco/snap`.
- Prior reviewed revision: `f195eae50a78c1d02e79b04328a0dcc49a1ec205`.
- Repair revision: `e9f64499daad0b9e1790e38a6a3d4bedcf84076c`.
- Repair commit: `e9f6449 Watch inherited workspaces and newly created Cargo configuration`.
- Original milestone base: `579f1c3a630571bfbbb06e0c24354d8ac306af4b`.

Reviewed the complete repair diff, the updated ledger in `docs/reviews/rust-watch.md`, and the affected watcher registration/classification and CLI contract. The Standards rules and accepted slice-06 contract read in round 1 remain applicable. This final round validates STD-1 and repair-induced interactions only. It does not reopen closed slice-05 behavior or audit unaffected milestone code.

## Finding disposition

### STD-1: Newly created ancestor Cargo configuration is never watched

Status: **RESOLVED**. Cross-reference: SPEC-2 in the shared ledger.

The repair at `tools/cli/src/watch.rs:165-179` records each ancestor's `.cargo` directory and both expected configuration file paths before checking whether the directory exists. The ancestor itself receives a nonrecursive watch. Creation or replacement of `.cargo` therefore matches the registered configuration paths in `Sources::classify` at lines 221-229 and invalidates the configuration. The subsequent watch-graph rebuild registers the new directory, allowing later edits inside it to trigger rebuilds.

Independent consumer verification passed through `DevContract.test_dependency_workspace_and_new_ancestor_config_rebuild`. With no Rust source edits after startup, the test creates a previously absent ancestor `.cargo/config.toml`, observes its compile-time environment value change to `new` over HTTP, edits that configuration again, and observes `updated`. This directly covers the missing creation event and continued watching, rather than relying on an unrelated source edit to recover.

## Affected interactions

- The same regression first replacement-saves a path dependency's separate workspace manifest and observes the inherited package version change from `0.1.0` to `0.2.0` over HTTP, with a new Build identity. This exercised the adjacent SPEC-1 repair in the same watcher lifecycle.
- `tools/cli/src/watch.rs:131-164` asks Cargo for each reachable local package's workspace manifest through the existing process runner, then watches the manifest's containing directory. It preserves process ownership/cancellation and handles replacement saves through directory watches.
- The added test uses a real CLI, an isolated project, OS-assigned ports, HTTP-visible compiled values, bounded polling, and process cleanup. It conforms to `TESTING.md`'s consumer-interface and fixture-ownership rules. No new crate, runtime dependency, or portable-code boundary change is introduced by the repair.
- No new BLOCKER, FOLLOW_UP, ADVISORY, or DECISION findings are recorded. The Spec review owns its separate final disposition of SPEC-1.

## Verification and limits

Independent checks:

- Confirmed the full prior/repair commit IDs, HEAD at the repair revision, and a clean working tree before and after validation.
- `git diff --check f195eae50a78c1d02e79b04328a0dcc49a1ec205...e9f64499daad0b9e1790e38a6a3d4bedcf84076c` passed.
- `mise exec -- cargo build -p snap-cli` passed and reported the current dev build up to date.
- `mise exec -- python3 tests/cli/dev.py DevContract.test_dependency_workspace_and_new_ancestor_config_rebuild -v` passed: one test, 5.054 seconds. All three compiled-value transitions completed without source edits.

Supplied evidence, not rerun here: the post-repair full `mise exec -- ./bin/check` passed with no skips in about 100 seconds, including 12 dev, 7 build, 4 check, 5 architecture CLI tests; four Chromium scenarios; SDK/protocol/journey, structural, Rustdoc, Clippy, dependency-tool, and lifecycle checks.

The full suite and browser scenarios were not repeated. Directory replacement handling was inspected; the independently run contract exercises initial directory creation and subsequent config-file editing. No product files were edited, no commits or publication actions were performed, and no review was delegated. This concludes the final authorized Standards validation round.

## Spec, round 2

# Slice 06 Spec review, final fix validation

Result: **CLEAR**. SPEC-1 and SPEC-2 are resolved. No repair-induced blocker or unresolved decision was found in the affected interactions.

## Revisions and scope

- Prior reviewed revision: `f195eae50a78c1d02e79b04328a0dcc49a1ec205`.
- Repair revision: `e9f64499daad0b9e1790e38a6a3d4bedcf84076c`.
- One repair commit: `e9f6449 Watch inherited workspaces and newly created Cargo configuration`.
- HEAD matches the repair revision. The working tree was clean before and after validation.
- Reviewed `git diff f195eae50a78c1d02e79b04328a0dcc49a1ec205...e9f64499daad0b9e1790e38a6a3d4bedcf84076c`, the complete review ledger in `docs/reviews/rust-watch.md`, and the prior Spec report at `/tmp/opencode/rust-watch-spec-r1.md`.
- This final round validates the assigned findings, their regression coverage, and repair-induced interactions with watch classification, discovery, rebuilding and process ownership. The accepted Linux/trusted-project scope and closed slice-05 amendment remain the contract. Unaffected implementation areas were not re-audited.

## Finding dispositions

### SPEC-1: resolved

Prior finding: separate path-dependency workspace manifests were absent from the watch graph, leaving inherited-setting edits unapplied until another watched input changed.

The repair at `tools/cli/src/watch.rs:131-150` asks Cargo to locate the workspace manifest for every reachable local package using `cargo locate-project --workspace --manifest-path`. It adds the returned manifest to the tracked set. Lines 157-164 register its containing directory, covering replacement saves rather than attaching a watch only to the old file inode.

The new actual-CLI contract replacement-saves a separate dependency workspace's manifest, changes its inherited package version from `0.1.0` to `0.2.0`, and observes the changed compiled value and a fresh Build over HTTP without touching Rust sources. I independently ran this contract successfully.

The added Cargo commands use the existing owned command runner and project cwd. Discovery still processes the deduplicated reachable local-package set. Its errors continue through the existing discovery-failure path. No repair-induced blocker was found in these interactions.

### SPEC-2: resolved

Prior finding: creating an initially absent ancestor `.cargo` directory was ignored, preventing both initial application of its config and subsequent config watching.

The repair at `tools/cli/src/watch.rs:165-179` records each ancestor's expected `.cargo`, `.cargo/config`, and `.cargo/config.toml` paths regardless of existence. Ancestor directories remain nonrecursively watched. The existing classification at lines 221-229 now recognizes directory creation/replacement as configuration invalidation. Rediscovery installs the directory watch when it exists; nonexistent directories are not passed to `notify.watch`.

The same independently run CLI contract creates the previously absent ancestor configuration, observes its compile-time environment value over HTTP, then edits the config again and observes the second value. Both transitions require no Rust source edit. This verifies initial invalidation and the refreshed config-file watch.

The ledger's STD-1 is the duplicate of SPEC-2. This report resolves SPEC-2 on the Spec axis; it does not replace the separate Standards verdict.

## Verification

Independent checks at the repair revision:

- Resolved both full revisions and confirmed the one-commit repair range.
- `mise exec -- cargo build -p snap-cli` passed in 0.03 seconds with no compilation needed.
- `mise exec -- python3 tests/cli/dev.py DevContract.test_dependency_workspace_and_new_ancestor_config_rebuild -v` passed, one test in **4.952 seconds**, no skip.
- The test owns its temporary project, dependency workspace, Cargo outputs and process session, uses configured port zero, and cleans up through the fixture launcher. It does not use default-port replacement.
- `git diff --check f195eae50a78c1d02e79b04328a0dcc49a1ec205...e9f64499daad0b9e1790e38a6a3d4bedcf84076c` passed.
- Final `git status --short` was empty.

Supplied evidence, distinguished from independent checks:

- The post-repair `mise exec -- ./bin/check` passed in about 100 seconds with 12 dev, 7 build, 4 check and 5 architecture CLI cases, four Chromium cases, and all SDK/protocol/journey, structural, documentation, Clippy, dependency-tool and lifecycle checks. No skips were reported.
- The implementer's targeted regression and diff check also passed.

## Limits and final disposition

I did not repeat the full repository suite or Chromium scenarios. The independent targeted test exercises both assigned failures through the real CLI and HTTP, including replacement-saving the inherited workspace manifest and subsequent edits to the newly watched Cargo config. Ancestor `.cargo` directory replacement was checked through the repaired registration/classification path, not a separate runtime probe.

No new BLOCKER, FOLLOW_UP, ADVISORY or DECISION findings. Both assigned findings are resolved, with no partial or unresolved item. This concludes the final authorized Spec validation round.

No product files were edited. No commits, publication, delegation or further repair work were performed.
