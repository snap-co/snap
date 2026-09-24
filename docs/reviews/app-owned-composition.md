# App-owned Healthy composition review

## Contract and scope

Slice 01 of `docs/plans/project-tooling-and-authy.md`, approved for implementation
on 2026-09-23. Move Healthy's composition and outputs into app ownership while
preserving portable dependencies, browser host reuse, and consumer behavior.

Base: `ec2b34bd7bcc2bacb377da89943dcc0fcdda86b8`.
Implementation revision: recorded below after commit.

Accepted implementation decisions:

- `healthy-native` owns the server binary, native journey runner, and existing SDK
  test target. The `healthy` application library retains its original dependencies.
- `healthy-wasm` selects the resident application and links shared query exports.
  `snap-client-wasm` is reusable library support without an application dependency.
- `apps/healthy/client.ts` selects generated bindings and decodes Healthy snapshots.
  Shared TypeScript support owns initialization caching, query shutdown, and
  observation subscriptions without importing application code or generated files.
- Bindings live under app-local `.snap/bindings`; the existing public WASM asset
  filename and release `dist/healthy` package layout stay compatible with the host.
- Existing assertions remain intact. Launchers, imports, and Cargo target ownership
  change to follow the new composition. No behavior change or new runtime framework
  is part of this slice.
- Build/check commands and watching/HMR remain subsequent slices. This repository
  has no remote, tracker, or publication workflow; work is committed on main.

## Verification

On 2026-09-23:

- `./bin/build-client`, native all-target compilation, TypeScript, WASM SDK contracts,
  and Rust formatting passed during iteration.
- `mise exec -- ./bin/check` passed all required gates, with eight CLI tests and no
  skips, four native SDK tests, three WASM SDK tests, three-client shared assertions,
  protocol checks, native journey, two Chromium dev/release tests, and dev lifecycle.
- TypeScript checking passed after the final declaration-only relocation from the
  shared React host to the application's web directory.
- Cargo metadata confirmed shared packages have no Healthy dependencies, including
  development/build dependencies. Portable Healthy's dependencies are unchanged.
- `git diff --check` passed.

## Review rounds

Round 1 is the initial independent Standards and Spec review of the committed
implementation. Maximum budget is two rounds for this milestone. The previous
`snap dev` review is complete and concerns a different milestone.

Status: awaiting commit and round 1 dispatch. No findings yet.
