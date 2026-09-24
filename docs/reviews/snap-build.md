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

Status: implementation ready to commit; round 1 reports pending.
