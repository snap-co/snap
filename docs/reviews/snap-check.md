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
this milestone independently of earlier completed reviews. Status: pending commit
and round 1 results.
