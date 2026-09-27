# Resident Store

Store gives server-side modules synchronous resident reads and one transaction
across their writes. Portable code uses `snap_store`; SQLite IO lives in
`snap_sqlite`. This is the database-backed durability phase.

## Run the consumer

From the repository root:

```sh
./bin/snap migrate --database .snap/testy-store.sqlite --migrations apps/testy/migrations
mise exec -- cargo run -p testy-local --features store --bin testy-store-demo -- .snap/testy-store.sqlite 1
```

The demo submits a transport request to create an account. The first request returns
a Store miss. The host then explicitly preloads tables and configures the demo policy.
A second request commits an account, grant, document metadata and notification intent
together. The result comes through the ordinary transport response types. Run with
another account ID to create another account; repeating an existing ID is rejected.
This consumer runs in process; it does not install a new network server or add Store
to the calculator's execution state machine.

These are small fixtures demonstrating shared transactions, not replacements for
Identity, Access or Document. The notification intent is an outbox row; the demo
does not send email.

## Use a transaction

```rust,ignore
let committed = store.run("accounts.create", |tx| {
    let policy = tx.get("access.policy", &[policy_id.into()])?
        .ok_or(Error::NotFound)?;
    accounts::create(tx, &input, &policy)?;
    access::grant(tx, &input)?;
    documents::create_home(tx, &input)?;
    outbox::enqueue_welcome(tx, &input)?;
    Ok(account_id)
})?;
// The database and resident state now both contain the entire transaction.
respond_success(committed.value);
```

Helpers take the SAME `&mut Transaction`, not separate backend connections. Their
schemas can use different namespaces in the same catalog. Cross-database atomicity
is not supported. Every table has a module-qualified name such as `identity.accounts`.
The namespace prevents naming collisions; it is not an authorization mechanism.

Every read has three possible outcomes:

- `Ok(Some(row))` / nonempty index results: resident hit.
- `Ok(None)` / an empty complete index result: known absence, sometimes called NX.
- `Err(Error::Miss(lookup))`: insufficient resident knowledge; this attempt cannot commit.

Use `?` to return a miss. Even swallowing that error cannot salvage the transaction.
Later Store calls return its sticky error. Handler errors and panics before commit
also discard scratch state. Portable handlers must not perform external IO themselves;
Rust callbacks are not a security sandbox and Store cannot undo an arbitrary email call.

Store never retries a handler. The host can examine `misses()` or drain `take_misses()`
and choose a later `load(table)`. The count survives draining; the latest 128 records
include the operation label and lookup. Treat lookup values as potentially sensitive.
Failed loads do not erase previously valid resident state. Reopening creates a cold
Store with fresh in-process diagnostics.

## Indexes and writes

`get(table, primary_key)` requires the complete primary key. `find(table, index,
prefix)` accepts an ordered prefix of a declared index; `primary` is the built-in
primary-key index. Empty prefixes enumerate that index. Results have no implicit
limit and sort by index columns, then primary key. Compound primary and secondary
keys are supported. There are no arbitrary SQL predicates, joins or OLAP queries.

Complete inserts stage without loading a cold key first. Database constraints still
check cold duplicates at commit. Updates take an existing primary key plus changed
columns and require the target to be resident. Primary-key edits are disallowed;
use explicit delete/insert. Deletes require knowledge of presence or absence.

Reads see earlier staged writes. After commit, written records are immediate primary
key hits and loaded index results reflect inserts, updates and deletes. An insertion
into a cold table does not claim to know every other record matching a secondary key;
that complete-result query remains a miss until the table is loaded.

SQLite permits one owning Store at a time, including while idle. Close it before
migrating or opening another owner. `Committed` means disk commit and memory publication
completed. `Indeterminate` means the commit result is unknown and this Store is fenced;
reopen/recover and resolve the operation's outcome before retrying it. Transport IDs
are not durable idempotency keys.

## Migrations

```sh
./bin/snap migrate new accounts --migrations myapp/migrations
# Edit the generated TOML; the empty template deliberately cannot be applied.
./bin/snap migrate --database myapp/.snap/store.sqlite --migrations myapp/migrations
./bin/snap migrate --database myapp/.snap/store.sqlite --migrations myapp/migrations --status
```

Migration IDs must match filenames and sort in strictly increasing order. The
generator uses a timestamp prefix, or increments an existing numeric prefix at its
current width when a timestamp would sort before it. It refuses to generate an
out-of-order ID. Histories with nonnumeric prefixes or exhausted number widths can
be extended with an explicitly named, lexically later file. Keep the entire applied
history in source control.
The adapter records canonical definitions rather than whitespace-sensitive file bytes.
Editing, deleting or inserting an older applied migration is rejected; add a new one.
Status validates history and pending declarations without applying DDL. It requires
an initialized database.

A create-table migration:

```toml
id = "0001_accounts"

[[changes]]
action = "create_table"
[changes.table]
name = "identity.accounts"
columns = [{ name = "id", kind = "integer" }, { name = "email", kind = "text" }]
primary = ["id"]
indexes = [{ name = "email", columns = ["email"], unique = true }]
```

Foreign keys declare `columns`, `table` and `references`. They must reference a
complete primary key with matching types. Foreign keys are checked at commit, so
modules may stage related rows in either order. No implicit cascading deletion.

Supported changes:

| Action | Fields | Meaning |
| --- | --- | --- |
| `create_table` | `table` definition | Create a table, its keys and indexes |
| `drop_table` | `table` name | Explicitly remove the table and its rows |
| `add_column` | `table`, `column`, `fill` | Add a non-null column and fill existing rows |
| `rename_column` | `table`, `from`, `to` | Rename without discarding data |
| `create_index` | `table`, `index` definition | Add a secondary index, optionally unique |
| `drop_index` | `table`, `index` name | Remove a declared secondary index |

Column kinds are `text`, `integer`, and `bytes`. A fill value is a TOML string,
signed integer, or byte array. Adding columns keeps the fill as a database default;
Store inserts still require full rows. Column removal, type conversion, arbitrary
SQL/data migrations and down-migration automation are not yet exposed. Explicit
forward declarations are the phase-one scope; destructive reversal is not guessed.

Local columns must exist at the step that declares an index or key using them.
Foreign target tables may be declared later in the same migration. SQLite's legacy
interpretation of unknown quoted identifiers as string literals is disabled.

All pending migrations apply in one SQLite transaction, including history and DDL
shape recording. A constraint/DDL failure rolls the batch back. Startup detects
out-of-band DDL rather than serving resident data against an unrecognized schema.
Durability depends on SQLite and the filesystem honoring sync. Abrupt-process tests
exercise restart behavior, not every possible hardware or power-loss fault.

## Verification

```sh
mise exec -- cargo test -p snap-store -p snap-sqlite
mise exec -- cargo test -p snap-sqlite --test recovery -- --ignored
mise exec -- cargo test -p snap-cli --test migrate
mise exec -- cargo test -p testy-local --features store --test store
mise exec -- cargo test -p snap-core-properties --test store-properties
HEGEL_TEST_CASES=10000 HEGEL_SEED=42 HEGEL_DATABASE=disabled HEGEL_STATISTICS=1 \
  mise exec -- cargo test -p snap-core-properties --test store-properties -- --nocapture
```

The generated suite compares transactions and index results with a plain record
model, checks cold/NX behavior, and injects confirmed rollback or lost commit replies
at the backend interface. A separate generated migration property checks column/index
dependency ordering. Real SQLite tests cover constraints, migration rollback,
exclusive ownership and abrupt-process restart. The fast repository gate includes
ordinary Store tests; Hegel stays opt-in.

The initial campaign on 2026-09-26 passed 90,000 generated cases: 10,000 per
property at seeds `20260926`, `42` and `18446744073709551615`. Each three-property
run took approximately 92 seconds including Cargo. Three deliberately introduced
faults were caught and shrunk: allowing a swallowed miss to commit, skipping index
maintenance on update, and failing to fence an unknown commit outcome. The swallowed
miss's reproduction blob replayed the failure. All mutations were removed.
These counts describe tested histories, not a proof of correctness.

After fixing a review-discovered index-before-column migration defect, the new
migration-order property passed another 10,000 generated cases at seed `42`.
The populated-table regression also verifies rejection without changing schema or
history, followed by correctly ordered DDL that genuinely enforces uniqueness.
