# App-owned Healthy composition review

## Contract and scope

Slice 01 of `docs/plans/project-tooling-and-authy.md`, approved for implementation
on 2026-09-23. Move Healthy's composition and outputs into app ownership while
preserving portable dependencies, browser host reuse, and consumer behavior.

Base: `ec2b34bd7bcc2bacb377da89943dcc0fcdda86b8`.
Implementation revision: `8b8654fbdae3bfff25bef570c661264cf6342ea2`.

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

Round 1 recorded before dispatch. Two independent read-only general agents review
the fixed base and implementation revision. Astra availability was confirmed through
model discovery. The harness permits an explicit model/variant override only on a
user request, so these agents inherit the session model rather than overriding its
variant. This is a composition relocation with existing lifecycle behavior, not a
new concurrency or security design.

Status: READY. Standards CLEAR and Spec CLEAR at
`8b8654fbdae3bfff25bef570c661264cf6342ea2`. One review round used; no repair or
second round needed. Required verification passed. STD-1 is optional and no
confirmed independent follow-ups require tracking. Subsequent changes to this
record and the progress/handoff documents record the outcome only.

- Standards session: `ses_f2f2afdadffe9Q1eIavYU4Ly3a`.
- Spec session: `ses_f2f2afd77ffemIOL6tNLusST2U`.

### Finding ledger

| ID | Disposition | Coordinator decision |
| --- | --- | --- |
| STD-1 | ADVISORY, low | Accepted as optional, pre-existing wording cleanup. No repair required for slice 01. Revisit when another app uses the browser host. |

### Standards report

## Standards result: CLEAR

No in-scope blocker, documented-standard violation, or unresolved decision found. One optional advisory concerns existing wording in the reusable React entrypoint.

### Reviewed revisions

- Base: `ec2b34bd7bcc2bacb377da89943dcc0fcdda86b8`
- Head: `8b8654fbdae3bfff25bef570c661264cf6342ea2`
- Only commit: `8b8654f Move Healthy composition and bindings into app ownership`
- Reviewed the fixed three-dot diff. The sole uncommitted file, `docs/reviews/app-owned-composition.md`, contains coordination metadata and was excluded from product-change assessment.

### Coverage and conformance

Read `AGENTS.md`, `README.md`, `TESTING.md`, the accepted slice 01 plan, and the review assumptions. Inspected all changed product files and relevant browser-runtime, React-host, build-driver, CLI-build, and consumer-adapter interactions.

- **Portability and dependency ownership:** Portable Healthy's source and dependencies remain unchanged. Native and WASM composition now occupy separate app-owned packages with distinct platform dependencies. This follows `AGENTS.md:8–13` and the plan's ownership rules at `docs/plans/project-tooling-and-authy.md:69–76`.
- **Shared support:** Shared Rust bindings no longer depend on Healthy. Shared TypeScript support imports neither Healthy nor generated bindings. Initialization caching, query lifetime, and observation subscriptions remain reusable; `apps/healthy/client.ts` owns generated-module selection and immutable snapshot decoding.
- **Browser composition and outputs:** The application still enters through the shared React host. Its ambient application declaration and generated bindings now live under Healthy. Config-relative `.snap/bindings` replaces the shared generated-output directory.
- **Consumer contracts:** Native SDK assertions retain their original file and move only their Cargo target ownership. Other contract changes are imports, artifact paths, and launch wiring. No changed assertions or runtime redesign were found. This conforms to `TESTING.md:3–5` and `AGENTS.md:19–20`.
- **Scope:** Changes remain within composition and interface ownership. No new CLI build/check commands, watcher, HMR implementation, or plugin framework was introduced.

### Finding

#### STD-1 — ADVISORY, low severity: reusable entrypoint retains Healthy-specific failure text

- **Location:** `clients/react/main.tsx:10`
- **Contract reference:** Reusable browser entrypoint requirement, `docs/plans/project-tooling-and-authy.md:74–75,130`. This is an optional cleanup, not an evidenced violation of slice 01.
- **Trigger:** Another application selects this reusable host, startup rejects, and the root element exists.
- **Evidence:** The shared catch handler renders `"Healthy could not start. Reload to retry."` regardless of the selected application. Comparing the fixed baseline confirms the same text existed before this change. Healthy's current behavior is unaffected; impact on another application was not exercised.
- **Bounded remedy:** When extending host reuse to another application, replace the literal with application-neutral wording. No new configuration or lifecycle interface is needed.

### Checks and evidence

**Independently performed:**

- Resolved both full commit IDs and confirmed the single-commit range.
- Ran `git diff --check` against the reviewed range successfully.
- Ran `mise exec -- cargo metadata --offline --locked --no-deps --format-version=1`.
  - Confirmed shared workspace packages have no Healthy dependency in any declared dependency kind.
  - Confirmed the native binary, journey example, and SDK test target belong to `healthy-native`.
  - Confirmed `healthy-wasm` owns the WASM composition target and shared bindings are a library.
- Compared the portable Healthy manifest and consumer assertion files against baseline.
- Inspected reference Doctor's health result in `~/code/bod/snap` read-only.
- Confirmed no product working-tree changes.

**Supplied by the coordinator, not rerun:**

- Complete `mise exec -- ./bin/check` pass, including eight CLI tests without skips, portability/compiler/lint gates, four native SDK tests, three WASM SDK tests, shared health assertions through all three client adapters, protocol checks, native journey, Chromium dev/release checks, and listener lifecycle.
- TypeScript recheck after the final declaration relocation.

### Limits

This was one read-only Standards review of slice 01 and its necessary interactions. Behavioral execution relies on the supplied verification; I did not repeat the full suite. Assessment uses the accepted Linux development, compatible-host release, and trusted-config/source assumptions. No required evidence was missing, and no files were edited.

### Spec report

## Spec review result: CLEAR

No in-scope blockers, decisions, or confirmed follow-ups found.

### Reviewed revisions

- Base: `ec2b34bd7bcc2bacb377da89943dcc0fcdda86b8`
- HEAD: `8b8654fbdae3bfff25bef570c661264cf6342ea2`
- Commit: `8b8654f Move Healthy composition and bindings into app ownership`
- Reviewed the fixed triple-dot diff. The only working-tree change was coordination metadata in `docs/reviews/app-owned-composition.md`.

Read `AGENTS.md`, `README.md`, `TESTING.md`, the accepted plan, and the implementation assumptions. Scope was slice 01 and its required interactions, under the accepted Linux development, compatible-host release, and trusted-source/configuration assumptions.

### Coverage and conclusions

| Accepted requirement | Review evidence |
|---|---|
| Portable Healthy remains IO-free and `no_std` with `alloc` | Healthy’s manifest and application sources are unchanged from baseline. Cargo metadata shows its original dependencies only. Native and WASM dependencies belong to the new composition packages. |
| Application owns native and WASM selection | `apps/healthy/native/src/main.rs:3` selects the server application; `apps/healthy/wasm/src/lib.rs:13` selects the resident client. Shared native and WASM packages no longer depend on Healthy, including development dependencies. |
| Shared binding and TypeScript support remain reusable | `bindings/wasm/src/lib.rs` retains shared query marshalling. `clients/typescript/src/index.ts` accepts bindings and a snapshot decoder rather than importing the app or generated files. A standalone browser bundle loaded only that shared TypeScript source. |
| App facade owns snapshot schema | `apps/healthy/client.ts:16` owns the snapshot type, and line 38 owns decoding and deep freezing. Shared observation support preserves lifecycle and snapshot caching. |
| Existing query and Healthy exports survive relocation | Directly inspected and instantiated the generated WASM module. Both classes’ exports were present. The generated `Client` completed a real HTTP query, and generated `Healthy` returned serialized loading and successful observations. |
| Reusable React host remains | The app definition still flows through `snap:application` into the existing host. The ambient application declaration moved into app ownership. Host startup and cleanup are unchanged. |
| Bindings have application-owned output locations | Healthy’s config and build scripts now generate under `apps/healthy/.snap/bindings`. The app facade imports that location. Dev and release packaging retain the existing public WASM asset filename. |
| Consumer behavior and assertions remain intact | Compared the moved Rust composition and extracted TypeScript lifecycle code against baseline. Test changes are target/import/path adjustments; existing consumer assertions are preserved. Native journey executable naming remains compatible with its launcher. |

The two new packages match the accepted compilation and dependency constraints. The implementation introduces no runtime redesign, plugin framework, new CLI build/check commands, or HMR.

### Checks performed independently

- Resolved both full revisions and inspected commit history, status, and diff.
- Ran `mise exec -- cargo metadata --no-deps --format-version 1`; checked dependency kinds and target ownership.
- Ran `git diff --check` against the fixed revisions.
- Confirmed no changes to portable Healthy, client/browser runtime behavior, native SDK assertions, journey assertions, or protocol assertions.
- Ran the direct generated-WASM probe described above against an ephemeral HTTP listener, with cleanup.
- Bundled shared TypeScript support independently, recording actual loaded inputs.
- Inspected the reference checkout’s Health API and Doctor response behavior.

The first exploratory WASM probe incorrectly expected the first notification to be successful. It received `loading`, which the unchanged browser runtime emits before completing HTTP work. Correcting the probe to await the first non-loading observation passed. This was a probe assumption error, not a regression.

### Supplied verification relied upon

The coordinator reported a passing `mise exec -- ./bin/check`, including all eight CLI tests without skips, portable compilation, four native SDK tests, three WASM SDK tests, the shared three-client health contract, protocol checks, native journey, two Chromium dev/release tests, and listener lifecycle. The coordinator also reported a subsequent passing TypeScript check after declaration relocation.

I did not repeat the full suite or independently rebuild release/browser artifacts. The boundary probe used the generated artifacts from that verification.

### Findings and limits

**Findings: none.** No `SPEC-N` entries are required.

This was one read-only Spec review of slice 01. It does not assess later slices, hostile configurations, or platforms beyond the accepted support scope. No product files were edited, and no commits, tracker changes, or delegated reviews were created.
