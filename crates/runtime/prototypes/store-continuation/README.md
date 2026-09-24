# Store continuation experiment

Follow-up: [accepted architecture](../../../../docs/adr/0001-host-driven-capability-providers.md)
and [Authy reference refactor](../../../../docs/plans/host-driven-module-pattern.md).

Throwaway design probe, not an adopted replacement for `Module::update` or Authy's
Passport. The question is whether a portable async workflow can read a resident
record immediately, suspend on a miss, and resume after host-owned IO, using the
same storage request vocabulary with multiple backends.

Run from the repository root:

```sh
mise exec -- python3 crates/runtime/prototypes/store-continuation/run.py
```

This compiles the portable code for `wasm32v1-none`, runs the native executable,
and writes a standalone, interactive trace viewer to
`/tmp/opencode/store-continuation-prototype/demo.html`. Open it in a browser. The
viewer navigates actual recorded Rust execution, rather than implementing a
second version of the storage/workflow logic. `trace.json` contains the same data.

## Ownership

- `core.rs`: portable Store contract, read cache, request/result channel, and async
  session-shaped workflow. It uses `core` and `alloc`. The workflow has no IO or
  executor dependency. A cached value is an owned versioned snapshot.
- `portable.rs`: standalone `no_std` compilation entry point. No new Cargo package.
- `platforms/native/examples/store-continuation-prototype.rs`: host owns polling,
  wake handling, request execution, memory authority, and SQLite authority. SQLite
  runs against disposable on-disk databases marked `PROTOTYPE-wipe-me` under
  `/tmp/opencode`, removed after each scenario.
- `MemoryCache` and `NoCache` are portable implementations selected by host
  construction. Memory management and cache policy are not inherently platform IO.

The backend sees generic keys, bytes, expected versions, and writes. It has no
credential/session methods. The example groups records by key prefixes, but does
not propose a schema, table layout, or query language for Snap.

## What the runs exercise

Each scenario runs against both Memory and SQLite:

1. **Cold lookup:** first poll queues a read and yields. The host runs an unrelated
   future, loads the record, wakes the caller, and resumes its retained continuation.
   The next await yields for the write.
2. **Completed prefetch:** the same read loads and caches the record before the
   workflow starts. One workflow poll crosses the read await and reaches the write
   await. Prefetch moved the load earlier; it did not remove the load.
3. **No cache:** the identical prefetch is followed by another load during the
   workflow. Caching is optional without changing the workflow or storage executor.
4. **Stale cache:** another writer changes the credential after prefetch. The cached
   read is immediate but its version is no longer authoritative. The conditional
   commit rejects it atomically and creates no session. There is no automatic retry.
5. **Cancellation before acceptance:** dropping the future removes its queued read.
6. **Cancellation after write acceptance:** the accepted write completes without a
   caller. Completion still invalidates cache entries and doesn't wake a dead task.

The runner checks those outcomes while producing the trace. It is an executable
experiment, not a new product behavior test suite.

### Observed result, 2026-09-24

All 12 runs completed with the expected outcomes. The portable module compiled
with warnings denied for `wasm32v1-none`. Formatting and
`cargo clippy -p snap-native --example store-continuation-prototype -- -D warnings`
passed. A Chromium walkthrough reached the final step of every recorded scenario
with no page errors. Native execution is demonstrated; bare-WASM execution is not.

The useful result is that `.await` does not necessarily suspend. A completed
prefetch lets the workflow reach its commit request in the first poll, while a
cold read returns control to the host before reaching that request. The same
compiler-generated continuation runs with both backends and both cache policies.

## Provisional semantics and limits

The Store contract here is deliberately small: lookup and atomic conditional batch.
For each batch, every version/absence condition must hold before any writes apply.
SQLite uses an immediate transaction. Memory executes the batch exclusively on the
owner thread. This tests shared semantics; it does not make Memory durable.

This is not a full sign-in implementation. The fixture contains an identity, not a
password hash. Password verification, hashing work, sessions/cookies, expiry and
real Passport policy remain in Authy. No measured performance claim is made.

The bridge is single-owner and uses `Rc<RefCell<_>>`. The host polls only initially
or after a real wake. It runs SQLite synchronously outside workflow polling for
clarity; this probe does not implement a worker pool or claim the host thread stays
nonblocking during the SQLite call. A worker-based host would send owned requests
out and return completions to the owner. No borrowed cache guard crosses an await.

Read snapshots can be stale. Every session insertion in this probe checks its
credential version against authority. All local commit results clear the cache,
including rejected or abandoned writes. An epoch prevents a read response issued
before an intervening commit completion from repopulating the cache. External
writers do not automatically invalidate it. Full coherence is a separate contract.

Missing records aren't cached. There are no deletes, eviction limits, load
coalescing, scans, indexes, migrations, general SQL, storage error recovery, or
durable continuation checkpoints. Rust futures retain state in live memory; they
are not serializable checkpoints. Redis/Postgres equivalence is not established.

Acceptance in this probe means the host has taken the storage request. That policy
is explicit for the experiment, not an amendment to Authy's accepted-write policy.

## Design implication to discuss

The experiment separates the consumer-facing Store contract, its portable
cache/request implementation, and the host's concrete storage executor. These are
distinct roles even if the final design groups them under one module. Credentials
and Sessions need not become table-specific platform interfaces. Domain modules
can use a shared Store, provided its transactions and constraints express the
atomic operations they require.

The experiment supports exploring compiler-generated continuations in place of
handwritten stages. It does not settle the final Store/Cache interface, provider
composition, schema ownership, freshness policy, or directory layout. Prototype
sources remain local for discussion; no new branch, worktree, or tracker was made.
