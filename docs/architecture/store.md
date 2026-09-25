# Local Store reference

`snap-store` is a local application-facing contract. `snap-native::store::Store`
supplies Memory and SQLite implementations. App composition registers all schemas
once and passes clones of that Store to its selected providers. Namespace handles
organize ownership, not end-user permissions; published operations own authorization.

## Schema and naming

A `Table` is a namespace/name pair. `Schema` declares non-null Text, Integer or
Bytes columns, a primary key, indexes, unique constraints, and foreign keys to
declared primary keys. `Row` values are checked against those declarations before
execution. This is a bounded relational interface, not a SQL parser or ORM.

SQLite maps a table to `namespace_name`, quotes identifiers, and rejects physical
name collisions. Both backends require lowercase ASCII letters, digits and underscores
in namespaces, tables, columns and legacy names. This excludes case-only aliases
under SQLite's identifier comparison, including on a later registration.
Schema registration runs in one transaction. A module can declare
a legacy physical name for a one-time rename preserving rows and references.
Passport uses this to migrate the old `credentials`, `sessions`, and `settings`
tables into `snap_identity`. The persisted signing key is reused.

The registry records logical ownership and a schema fingerprint. Reopening checks
columns, primary keys, indexes and foreign keys; incompatible declarations fail
without partial migration. Original SQLite primary-key columns may lack an explicit
NOT NULL clause; Store's typed writes still require those fields. Subsequent schema
changes need an explicit migration design rather than silently altering data.
PostgreSQL's future mapping is a schema/table pair; no Postgres backend ships here.

## Transactions and reads

`Store::transaction` checks all guards, then executes its statements serially in a
single serializable transaction. All selects share its snapshot and observe earlier
statements. Constraint or guard failures roll back all writes. Results correspond
to statements; write results are empty row sets.

Queries support conjunctions of typed equality/inequality/range predicates, ordering,
and a row limit. Primary-key ordering breaks ties. Inserts supply complete rows;
updates supply non-primary columns; deletes select rows by predicates. Foreign-key
checks are immediate with no cascading actions. There are no joins, nullable fields,
aggregate expressions, general upserts, or network access in this interface.

Memory executes exclusively against a candidate state and publishes it only after
success. SQLite uses immediate transactions, including for coherent reads. This
favors explicit serializable behavior over read concurrency in the initial backend.
Memory offers atomicity, not durability. SQLite acknowledges after commit.

## Host ownership and cancellation

Native Store clones share an executor handle. `transaction().await` submits work to
a blocking host worker; portable futures never lock or query the database themselves.
Dropping a waiter cannot roll back an already-started transaction. The web host
keeps an admitted Passport continuation alive after its observer disconnects, so
later workflow steps still finish and revocation effects still get delivered.
Process termination is not durable workflow recovery and no mutation is replayed.

## Cache policy

`snapshot` is an explicit advisory read using the supplied `Cache`; transactional
reads always use Store authority. `NoCache` always loads. Native `MemoryCache` has
a bounded entry count and returns owned row snapshots. Entries have no freshness
guarantee or automatic coherence. A completed prefetch can avoid a read suspension,
but cannot substitute for a transactional check of current authority.

Authy selects NoCache. Passport can consume a snapshot cache for credential lookup:
it checks the credential/hash again inside session creation and reloads authority
before rejecting cached absence or a password against a cached hash. Receipt caching has separate
lifetime/recovery semantics and is not implemented by this record cache.

Contract verification: `cargo test -p snap-native --test store-contract`. Authy
migration is verified through wire operations in `tests/protocol/migration.test.ts`.
