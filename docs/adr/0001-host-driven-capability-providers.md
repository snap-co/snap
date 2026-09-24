---
status: accepted
---

# Host-driven capability providers and shared storage

Snap retains the separation between capability contracts and their providers,
while applications select statically linked Rust implementations and hosts own
execution and IO. A shared, local Store supports provider-owned and application-owned
data; remote operations are published explicitly. This preserves replaceability
without reproducing Effect's runtime service construction or making Document the
required persistence model.

Agreed on 2026-09-24. This records the target architecture. The existing Authy
implementation is the first migration target, not evidence that migration is done.

## Reasons

- Protocol/Transport, Identity/Passport, Access/Acl, Document/Snapshot, and
  Blob/Bucket are contract/provider relationships. Rust compilation changes how
  applications construct providers, not whether consumers can depend on contracts
  independently of providers.
- The current `Lane` declaration mixes operation definitions with HTTP/WebSocket
  bindings. Compatibility requires preserving selected wire behavior, not those
  internal classifications. Hosts unwrap carrier envelopes before dispatch.
- Handwritten input/action stages preserve host control, but compiler-generated
  continuations can express sequential workflows with less manual state. The
  [experiment](../../crates/runtime/prototypes/store-continuation/README.md) proves
  IO-free futures can yield host work, resume on completion, and avoid a read
  suspension after a completed prefetch. Host-owned IO remains the constraint.
- Store has independent consumers: Passport, Snapshot, and application-owned data.
  Sharing its backend avoids repeating persistence mechanics for every provider.
  Document's tree, mutation, and synchronization behavior remains useful where
  that model fits the data.

## Consequences

Capability contracts are peers. Protocol is not an umbrella for Identity schemas.
Application composition selects providers and supplies one Store setup initially.
Each module owns a storage namespace and its schema; backend naming maps logical
names to PostgreSQL schemas or SQLite prefixes. Namespace separation need not
prevent transactions spanning participating namespaces in the same Store.

Store is local to its host. Storage declarations do not expose remote operations.
Providers can expose published client/server operations, trusted server interfaces,
or server-only workflows. Owning modules enforce domain authorization; Transport
dispatches with trusted caller context without acquiring domain-specific policy.

Portable futures may replace manual workflow stages. They do not prescribe an
executor, serialize continuations, or define cancellation and accepted-write
semantics automatically. Record residency and operation receipts have distinct
cache contracts even when they share a backend.

The exact Store query/transaction interface, schema evolution machinery, cache
freshness policy, and crate layout remain design work. Backend substitution means
satisfying the same declared guarantees, not assuming every database is equivalent.

Follow the [module porting guide](../architecture/module-pattern.md). The
[reference refactor plan](../plans/host-driven-module-pattern.md) defines the next
implementation and its completion criteria.
