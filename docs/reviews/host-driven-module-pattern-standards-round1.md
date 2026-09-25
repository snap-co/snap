# Host-driven module refactor: Standards review, round 1

## Verdict

**BLOCKED.** Two findings need coordinator disposition before this refactor satisfies its accepted architecture and Store contract. The main ownership split is sound: contracts are independent of providers, Authy selects its implementations, and native executors perform database and expensive crypto work outside portable future polling. The remaining issues are SQLite namespace aliasing and web sequence framing retained in the portable client.

This is one independent, read-only Standards review in round 1 of the new two-total-round budget. I did not perform another review round, delegate, change product files, edit the review ledger, commit, or publish.

## Revisions and scope

- Immutable base: `ee98a70afac25a3caee8a0306db633efb1ab18c1`.
- Reviewed HEAD: `a47a4018c7755c548eeb4d3bac55edc7ca72afe6`.
- Sole commit: `Implement host-driven providers, shared Store and carrier bindings`.
- Review range: `git diff ee98a70afac25a3caee8a0306db633efb1ab18c1...a47a4018c7755c548eeb4d3bac55edc7ca72afe6`.
- Captured diff: `/tmp/opencode/module-pattern-round1.diff`. It is byte-for-byte identical to the Git diff. SHA-256: `65235eac1b454ed55791a4ac4b047b8f9de8710e9145276c10b6300b3fe2fb87`.
- The initial working-tree check showed only the permitted coordinator-owned modification to `docs/reviews/host-driven-module-pattern.md`.
- Architecture authority: `AGENTS.md`, `README.md`, `TESTING.md`, accepted ADR `docs/adr/0001-host-driven-capability-providers.md`, `docs/plans/host-driven-module-pattern.md`, `docs/architecture/module-pattern.md`, and `docs/architecture/store.md`.
- Observable compatibility authority: `docs/plans/authy-password-sessions.md`. Its old synchronous stages and Lane choices were not treated as requirements.

## Coverage

I inspected the changed contract and provider interfaces, their manifests and structural-role enforcement, portable client changes, both client carrier adapters, native scheduling and web delivery, Authy composition, Store implementations and registration, crypto and cookie ownership, new test contracts and adapters, verification command changes, and architecture documentation. I compared affected lifecycle code with the base where needed to distinguish retained behavior from changes.

Specific paths covered include:

- `crates/protocol`, `crates/identity`, and `crates/store`.
- `crates/runtime/src/{lib,transport,doctor,passport}.rs`.
- `crates/client/src/{lib,application,identity}.rs`.
- `platforms/web/src/lib.rs`.
- `platforms/native/src/{lib,store,passport,cookie,websocket,client}.rs` and `platforms/native/src/client/identity.rs`.
- `platforms/browser/src/{lib,identity}.rs`.
- `apps/authy/src/lib.rs`, `apps/authy/native/src/main.rs`, and related composition configuration.
- `tools/cli/src/architecture.rs`, new Store/carrier/migration/structural tests and their adapters, `bin/check`, and the archived prototype's replacement README.

I also inspected the TypeScript carrier's operation-ID construction at the selected compatibility revision `9689a8ed3108f58233721c2000d2b9ea96259fe7`, using `git show`. The reference checkout's current HEAD is `188d32bfe720a85e336f31dded36bcd8f06f1069`; I did not assume those revisions were identical.

### Standards satisfied in the inspected paths

- The three contract packages have the `contract` role and no provider dependencies. The CLI includes that role in portable checks and rejects normal dependencies from contracts to portable providers.
- Application, runtime, and client core code remains `no_std` with `alloc`. Store transactions and costly crypto calls delegate to native blocking workers. The general scheduler handles provider futures and composition-produced effects without Passport work variants.
- Passport owns identity policy and checks protected reads and revocation in guarded transactions. Its logical schemas and legacy names stay with Passport; backend SQL stays in native Store.
- Authy selects the Store, crypto executor, NoCache policy, bindings, and cookie/result projection. Table registration does not publish remote access.
- Accepted continuations retain their semaphore permit while running, even when their response observer leaves. Private response channels isolate equal caller-selected IDs. Native Store documents the lifetime of already-started blocking transactions.
- New Store assertions name promises that Identity's SDK cannot express. They reuse one contract through Memory and SQLite adapters. Carrier and migration assertions exercise wire operations. I found no new direct `Provider::invoke` behavior tests replacing consumer contracts.

## Findings

### STD-1: Reject SQLite identifier aliases during schema registration

- Classification: **BLOCKER**.
- Severity: **Medium / P2**. This silently violates namespace isolation and backend substitution for accepted schemas. Authy's current lowercase declarations do not trigger it.
- Primary location: `platforms/native/src/store.rs:452-468`, particularly the case-sensitive `names.contains(&physical)` collision check at line 457.
- Related locations: identifier acceptance at `platforms/native/src/store.rs:440-445`; registry lookup and registration at lines 61-79 and 200-204; physical table creation at lines 130-135; Memory's table map at lines 24-29.
- Contract: `docs/architecture/store.md:15-16` promises SQLite physical-name collision rejection. `docs/architecture/module-pattern.md:90-105` requires module-owned namespaces and conforming backend substitution. `docs/plans/host-driven-module-pattern.md:77-85` requires Memory and SQLite to implement the same interface and named guarantees.

**Trigger and evidence.** Register two schemas with identical columns and primary keys, no indexes or foreign keys, and tables `Table { namespace: "Alpha", name: "records" }` and `Table { namespace: "alpha", name: "records" }`. Both names pass `valid`, which permits ASCII uppercase. Their physical strings differ, so `validate_schemas` accepts both. Memory creates two distinct `BTreeMap` entries.

SQLite identifiers remain case-insensitive when quoted. The second `CREATE TABLE IF NOT EXISTS` therefore reuses the first table. Both `PRAGMA table_info` checks succeed because the declared shapes match, and the registry accepts two distinct case-sensitive text keys. Reads and writes through the two logical namespaces address one physical table. A write through `Alpha.records` becomes visible through `alpha.records`; matching primary keys can also cause a constraint failure that Memory would not produce.

I confirmed the SQLite part with an isolated Python `sqlite3` in-memory probe using the emitted registration DDL. Its output showed:

```text
registry: [('Alpha_records', 'Alpha.records'), ('alpha_records', 'alpha.records')]
read via alpha: [('owned-by-Alpha',)]
tables: [('snap_store_schemas',), ('Alpha_records',)]
```

This probe exercised SQLite's identifier behavior, not the Rust Store end to end. The accepted-input and backend divergence paths above are established directly by the Rust source.

**Bounded remedy.** Define one supported identifier policy and enforce it in shared schema validation. For example, reject uppercase identifiers consistently in both backends, or reject collisions using SQLite's identifier comparison rules. Apply the same policy to the reserved registry name and relevant legacy names. Keep reopen ownership checks consistent with that policy. Add a schema-registration case through the existing Store adapters that proves accepted namespaces remain distinct or that colliding declarations fail consistently.

### STD-2: Finish moving connection ID framing out of the portable Identity client

- Classification: **BLOCKER**.
- Severity: **Medium / P2**. Current web behavior works, but the new client/carrier boundary still depends on the selected web encoding.
- Primary location: `crates/client/src/identity.rs:167-184`, particularly `format!("{epoch}:{}", self.message_sequence)` at line 169.
- Changed interface path: `crates/client/src/identity.rs:339-344` retains the raw carrier epoch in response to the new `ConnectionEvent::Attached`; `crates/protocol/src/lib.rs:57-60` defines the normalized connection events.
- Related locations: `platforms/web/src/lib.rs:156-159` parses the same web sequence encoding; `platforms/native/src/client/identity.rs:159-166` and `platforms/browser/src/identity.rs:213-220` serialize the already-framed portable invocation unchanged.
- Contract: `docs/plans/host-driven-module-pattern.md:57-62` requires removing Query/Submit/Message assumptions from client workflow decisions and leaving carrier framing with the selected host implementation. Its implemented-shape claim at lines 13-15 specifically assigns sequence framing to `snap-web`. `docs/architecture/module-pattern.md:29-31,62-64` assigns envelopes and framing to carrier implementations.

**Trigger and evidence.** Every successful connection attachment supplies an epoch to the portable Identity controller. Its collection-read path increments `message_sequence`, constructs the exact `epoch:sequence` wire string, uses it for pending/deadline correlation, and sends it as `Invocation.operation_id`. Both web adapters serialize that value directly. Moving parsing into `snap-web` has therefore moved only one side of the framing boundary. An adapter using a different operation-ID encoding must translate IDs and reverse that translation for completions around a web-formatted value already owned by the core.

The selected TypeScript reference also constructs this encoding in the browser transport, at `packages/browser/src/transport.ts:493-496` in revision `9689a8ed3108f58233721c2000d2b9ea96259fe7`. Preserving `epoch:sequence` on the web wire does not require the Identity controller to construct it.

This is a retained omission in the refactor's expressly targeted client/carrier split, not a newly observed wire regression. The new normalized-event seam and the documentation claim that sequence framing now belongs to `snap-web` make it relevant to this completion review.

**Bounded remedy.** Let the portable controller correlate reads with carrier-neutral identifiers or structured logical identity. Put web operation-ID encoding and corresponding completion normalization in `snap-web` and its host adapters. Preserve the existing web strings, read coalescing, generation fencing, deadlines, and recovery behavior. Existing shared native/browser SDK and wire contracts should remain the verification seam.

## Verification

### Checks I actually ran

| Check | Result |
| --- | --- |
| `git rev-parse HEAD`, `git status --short`, and review-range commit listing | Expected HEAD, sole commit, and permitted ledger-only modification |
| Byte comparison of Git diff with `/tmp/opencode/module-pattern-round1.diff` | Identical; SHA-256 recorded above |
| `git diff --check BASE...HEAD` with the full revisions above | Passed |
| Existing binary `target/debug/deps/store_contract-e50395d7e3e1236b --nocapture` | 2 passed: Memory and SQLite Store contracts |
| `mise exec -- bun test tests/protocol/carriers.test.ts` | 1 passed, 5 assertions; independent fixture on an ephemeral port |
| Isolated in-memory Python SQLite identifier probe | Confirmed case-only physical names alias despite distinct registry rows |

I used the existing Store test executable named in the coordinator's gate log. I launched no compilation, build, full suite, development watcher, or command that publishes `.snap` artifacts. The Store test owned its temporary databases; the SQLite probe used only `:memory:`. I did not access the real Authy database.

### Supplied and observed gate evidence

The assignment supplied successful workspace all-target compilation, warnings-denied Clippy, initial bare-WASM checks, Store/carrier/lifecycle contracts, Authy wire and legacy migration cases, eight native/browser recovery cases, and TypeScript checks. Those are coordinator evidence, not independently rerun checks except where listed above.

I read the first 100 lines of `/tmp/opencode/module-pattern-full-check.log`. That snapshot explicitly showed the final-role structural check passing for Healthy with workspace portable packages, including Protocol, Identity, and Store. It also showed Rustdoc completion, dependency checks, CLI check results, Healthy native SDK results, and the two Store test names passing. I do not claim that the whole `mise exec -- ./bin/check` run or the required Authy project gate completed successfully from that partial log.

## Limits and disposition

- This was the Standards axis, focused on the changed architecture and affected interactions. It was not a fresh repository-wide design or security review.
- I did not rerun Authy browser/UI, migration, recovery, HMR, packaging, or TypeScript interoperability suites. Their supplied results remain coordinator evidence.
- The passing existing Store suite does not cover case-folded schema names. The SQLite probe supports STD-1 but is not a substitute for the proposed shared Store regression case.
- STD-2 is an architecture-completion finding supported by the accepted plan and direct data flow. It does not assert a failure in the current web contract tests.
- No additional findings require a broader redesign or a new requirement. No tracker was created. The coordinator owns disposition, fixes, gate completion, and any round-2 validation.

Final Standards verdict: **BLOCKED** pending disposition of STD-1 and STD-2.
