# Project check review

Contract: slice 03 in `docs/plans/project-tooling-and-authy.md`. Base
`334fd4fe8f50a6a2d7aab78b55b76b7f25f8aa6e`. Linux, trusted project config/source,
current main branch, no tracker/remote. Architecture policy and watch/HMR are later slices.

`snap check [directory]` shares discovery. `[check] rust` selects package manifests,
defaulting to the configured server and WASM packages. Each receives formatting,
Clippy with warnings denied, and all-target tests. Optional `build=true` runs the
shared debug builder first, so preparation may generate Rust inputs. Ordered literal
argv `commands` run at config-root cwd, with fresh artifact variables
`SNAP_CHECK_EXECUTABLE`, `SNAP_CHECK_PACKAGE`, and optional `SNAP_CHECK_WEB_DIR`.
Missing/inapplicable artifact variables are removed. Failures and interruption
propagate through the shared process-group runner; checks never silently skip.
The CLI itself neither launches a persistent dev server nor replaces listeners.

Healthy declares its Rust packages, TypeScript, WASM SDK, and packaged Chromium
scenario. Native SDK contracts are part of its native test target. Repository CLI,
release packaging, and reference-TypeScript contracts remain in `bin/check`.
Existing browser assertions are unchanged; its launcher accepts the check artifact.

Verification on 2026-09-23: full `mise exec -- ./bin/check` passed with 8 dev, 7 build,
and 4 check CLI tests, all previous SDK/protocol/journey/browser/lifecycle gates and
no skips. After moving build preparation ahead of Rust checks, the four check CLI
tests, real `snap check apps/healthy`, and CLI Clippy passed again. The check fixture
now generates its Rust source through prepare.build before checking. The original
check command test failed before implementation. `git diff --check` passed.

Round 1 recorded before dispatch. Two independent read-only Standards and Spec
reviews inherit Astra under the harness model policy. Two-round budget applies to
this milestone independently of earlier completed reviews.

Implementation: `ae7f22e6f214025c4c6a9e71808b3922a2ea685a`. Status: READY. Both axes
CLEAR in round 1, no findings or repairs. Standards session
`ses_f2f0cb815ffeRmfHmATQvzDZP7`; Spec session `ses_f2f0cb780ffe6osBIMioaK1L10`.

## Standards report

## Standards review: CLEAR

Reviewed `git diff BASE...HEAD` for:

- Base: `334fd4fe8f50a6a2d7aab78b55b76b7f25f8aa6e`
- HEAD: `ae7f22e6f214025c4c6a9e71808b3922a2ea685a`
- Commit: `ae7f22e Add selected project verification with snap check`

### Findings

None. No evidenced standards violations or introduced regressions found within the assigned scope.

### Coverage

Read `AGENTS.md`, `README.md`, `TESTING.md`, plan slice 03, and `docs/reviews/snap-check.md`. Reviewed all changed files and necessary interactions with discovery, shared builds, Cargo selection, process ownership, Healthy test launchers, and the repository gate.

- New concerns use CLI modules and preserve portable application/runtime/client boundaries.
- Check execution reuses the shared builder and process-group runner. Optional build preparation precedes Rust verification.
- Rust checks select packages by manifest. Project commands preserve literal arguments, config-root cwd, and fresh artifact variables.
- New tests exercise the real CLI through independent project fixtures. Existing browser assertions remain intact, with artifact selection added to launch setup.
- Healthy's configured SDK/browser checks have no reference-checkout dependency. Repository compatibility checks retain their separate entrypoint.
- The check execution path does not invoke development listener replacement or start a persistent development host.

### Verification and limits

**Independently checked:** fixed revisions and commit range, clean working tree, complete diff, necessary source interactions, and `git diff --check BASE...HEAD`, which passed.

**Supplied evidence:** full `mise exec -- ./bin/check` passed with 8 dev, 7 build, and 4 check CLI tests, no skips, and the SDK/protocol/journey/Chromium/lifecycle gates. After the ordering amendment, the four check fixtures, Healthy project check, and CLI Clippy passed again.

No runtime tests were repeated. This was one read-only Standards review under the accepted Linux and trusted-config assumptions. Later architecture tooling and HMR were excluded. No files were changed.

## Spec report

## Spec review: CLEAR

Reviewed the single commit `ae7f22e Add selected project verification with snap check` using the fixed diff:

- Base: `334fd4fe8f50a6a2d7aab78b55b76b7f25f8aa6e`
- HEAD: `ae7f22e6f214025c4c6a9e71808b3922a2ea685a`

### Findings

None. No evidenced accepted-requirement violation, introduced regression, or unrequested behavior warrants a SPEC-N finding.

### Coverage

Read `AGENTS.md`, `README.md`, `TESTING.md`, plan slice 03, and `docs/reviews/snap-check.md`. Reviewed all changed files and necessary interactions with discovery, the shared builder, process cleanup, Cargo selection, and Healthy's test launchers.

The implementation matches the accepted contract:

- Optional shared debug build runs before Rust checks.
- Selected package manifests receive formatting, warnings-denied Clippy, and all-target tests.
- Project commands run sequentially with literal arguments, config-root cwd, and refreshed artifact variables.
- Failures stop execution; interruption uses existing child-group cleanup.
- Check does not enter the development launcher or listener-replacement path.
- Healthy selects native/WASM SDK and packaged Chromium checks without invoking reference-TypeScript compatibility checks.
- Repository verification remains in `bin/check`.

The extracted Cargo helpers preserve baseline build behavior. The browser change preserves existing assertions and accepts the selected check executable.

### Independent verification

Passed:

- `mise exec -- cargo build -p snap-cli`
- Real-CLI temporary probes confirming:
  - An explicit package passes despite an unformatted, uncompilable sibling in the same Cargo workspace.
  - All three stale `SNAP_CHECK_*` variables disappear from commands when building is disabled.
  - Commands run in order at config-root cwd.
  - An existing listener remains available.
  - A virtual-workspace manifest fails with an explicit package-selection diagnostic.
- Fixed-revision `git diff --check`.

Probe script: `/tmp/opencode/snap-check-spec-probe.py`. Temporary fixture directories were removed. Product files remain unchanged; working tree is clean.

### Supplied evidence and limits

Relied on the supplied successful full repository gate and post-amendment check fixtures, Healthy project check, and CLI Clippy results. Did not repeat those suites. Healthy's reference independence was assessed through its declared commands and their imports, not by independently rerunning Healthy without the reference checkout.

Review is limited to Linux, trusted project configuration/source, and slice 03. Later architecture tooling and HMR are excluded.
