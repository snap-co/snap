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
Status: awaiting committed revision and reports.
