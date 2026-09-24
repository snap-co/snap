# Authy as the reference for the host-driven module pattern

Status: implemented; final verification and review in progress. The architecture is
[accepted](../adr/0001-host-driven-capability-providers.md); the original experiment
is retained as [historical evidence](../../crates/runtime/prototypes/store-continuation/README.md).

## Implemented shape

- `snap-protocol`, `snap-identity`, and `snap-store` are independently consumable
  contract crates. Structural checking enforces the new `contract` role.
- `snap-runtime` groups Transport/Doctor/Passport providers. Protocol's `Provider`
  interface creates owned futures with capability-specific context and output.
- `snap-web` owns the selected web bindings, completion codecs, close-code mapping,
  and sequence framing. No Lane remains in operation declarations. Both shared
  client controllers consume normalized outcomes rather than wire strings.
- `snap-native::store` implements the shared Store for Memory and SQLite. Schemas
  register atomically with namespace collision checks and legacy-name migration.
  [Store semantics](../architecture/store.md) records the bounded interface.
- Authy composition selects Store, NoCache, crypto, and signed-cookie projection.
  Native crypto work has no storage queries. The general host polls provider
  continuations without Passport-specific actions or startup functions.
- Existing wire/SDK contracts remain. New Store, carrier/lifecycle, migration, and
  structural contracts cover the newly established promises.

## Goal

Refactor the existing Healthy/Authy slice into the reusable
[module pattern](../architecture/module-pattern.md) before porting another
capability. Authy must keep its working password/session flow while demonstrating
contract/provider separation, carrier-independent operations, host-driven
continuations, and application-selected shared storage.

This is a new internal-architecture change. The completed Authy review remains
historical evidence for its reviewed revision, not approval of this refactor.

## Compatibility baseline

Preserve the [Authy consumer contract](authy-password-sessions.md), its native and
browser SDK behavior, selected TypeScript wire interoperability, Healthy, and
development/standalone packaging behavior. Preserve existing accounts, password
hashes, sessions, and signing keys when moving storage into namespaces. A schema
change must migrate the current SQLite database without asking users to recreate
accounts or sign in again solely because of this refactor.

The old contract's synchronous-stage and Lane-based implementation choices are
superseded as design constraints by the ADR. Its observable behavior remains the
compatibility baseline. The prototype's host-takes-job acceptance point does not
automatically replace Authy's accepted-write semantics.

## Implementation sequence

### 1. Separate contracts, providers, and carrier bindings

- Give Protocol and Identity peer ownership. Place credential storage records,
  including password hashes, with their owning provider/storage model rather than
  the public Identity contract.
- Remove `Operation.lane` and Query/Submit/Message assumptions from portable
  dispatch, Passport declarations, and client workflow decisions. Keep the
  existing external HTTP/WebSocket mappings in carrier bindings.
- Expose an operation invocation and trusted caller context independently of its
  envelope. Keep connection correlation/admission behavior carrier-neutral where
  reusable, and carrier framing in the selected host implementation.
- Choose the smallest module/crate layout that enforces independent contract
  consumption and portability. Record that concrete layout before moving files.

Complete when contract-only consumers compile without providers, shared operation
definitions do not classify carriers, and existing wire/SDK behavior passes.

### 2. Establish the shared Store interface with real consumers in mind

- Define a bounded set of schema, lookup/query, and transaction operations needed
  by the current Passport flow. Include namespace registration, unique claims,
  indexed reads, bounded session/credential reads, and atomic conditional writes.
- Exercise coherent multi-table reads and an atomic related-record update shaped
  like Snapshot's requirements as a storage-contract probe. This establishes the
  required storage promise without porting Document or Snapshot yet.
- Supply Memory and SQLite implementations of the same interface. State the
  difference in durability. Use PostgreSQL schema naming as a design input, not
  an unverified claim that a Postgres backend exists.
- Define schema evolution ownership and migrate existing SQLite data. Logical
  namespaces must support future PostgreSQL schema mapping and current SQLite
  prefixing without provider-specific backend SQL.

Complete when the same named storage-contract promises hold through both backend
adapters, coherent reads/atomicity cannot be replaced with unrelated cache reads,
and existing SQLite data survives migration. Finalize the minimal interface here;
the prototype's key/bytes shape is evidence, not the required production design.

### 3. Make Passport the first host-driven continuation provider

- Express its external-work workflow through portable futures. Keep hashing,
  randomness, clocks, database IO, and cookie projection host-owned.
- Application composition supplies the shared Store and chosen cache policy.
  Passport's domain-specific helpers use that Store; backends do not implement a
  separate Passport storage interface.
- Replace Passport-specific types in the general execution interface and the
  general host's concrete Passport startup special case with composition-owned
  wiring. Preserve explicit authority and revocation delivery.
- Preserve admission capacity, deadlines, accepted writes, late-result fencing,
  and shutdown. Cache misses may suspend; hits/prefetch must preserve the same
  authoritative checks. Keep receipt caching separate from record residency.

Complete when the real Authy flow runs through the new composition, the host's
general scheduler has no Passport-specific branches, and lifecycle/authority
contracts pass through the consumer interfaces.

### 4. Close the reference refactor

- Run the existing shared native/browser identity journey, recovery cases, wire
  assertions, TypeScript interoperability, and Authy UI cases. Add focused coverage
  for persistent-data migration and any newly declared storage/execution promise
  at its owning consumer seam, following TESTING.md.
- Run `mise exec -- ./bin/check` and
  `mise exec -- ./bin/snap check apps/authy`. Run structural checks whenever a
  package/dependency changes as required by AGENTS.md.
- Update README's current architecture and the porting guide with actual code
  paths. Replace the throwaway prototype with the proven production interfaces
  and retain the experiment's conclusions as historical evidence.

Complete when checks pass and a scope-bounded review of this new refactor confirms
the architecture and preserved behavior. Start a new review record; do not reopen
or reuse the completed Authy implementation review budget.

## Scope

The result is one working reference composition and a repeatable porting method.
Postgres/Redis backends, a general SQL language, remote Store access, Document
replication, distributed cache coherence, and new authentication features require
their own consumer flows. Namespace-wide transactions, storage errors, migration
behavior, and cache freshness must be explicit wherever the selected flow needs
them; backend names alone do not establish those guarantees.

Future module ports follow the guide against their selected TypeScript revisions.
They should change shared interfaces only when a concrete consumer promise cannot
be expressed by the existing pattern.
