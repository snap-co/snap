# The Snap module pattern

Use this guide when porting a TypeScript module, adding a capability, or changing
provider composition, carrier handling, or storage. The
[decision](../adr/0001-host-driven-capability-providers.md) records why this pattern
was chosen; [CONTEXT.md](../../CONTEXT.md) defines the names.

Status: accepted pattern, reference implementation pending. Authy's
[refactor plan](../plans/host-driven-module-pattern.md) tracks the transition.

## Assign ownership before choosing directories

| Concern | Owner |
| --- | --- |
| Operation and data contracts | The capability, such as Protocol or Identity |
| Domain rules and authorization | The selected provider, such as Passport |
| Explicitly published operations | Application composition and provider registration |
| HTTP, WebSocket, or binary envelopes | Carrier implementation selected by the host |
| Scheduling, IO, clocks, randomness, expensive external work | Host execution |
| Logical schema, indexes, constraints, schema evolution intent | The module owning the records |
| Query execution, physical naming, atomicity and durability mechanics | Store implementation |
| Record residency and freshness | The declared record-cache policy |
| Retry receipts and their lifetime | The operation-recovery contract |

Arrange contracts so an independent consumer can use them without depending on a
provider. Use Rust modules for ownership and crates for enforceable dependency or
portability constraints. Choose concrete package names when applying those rules;
the table does not require a package for each row or a trait for every helper.

## Porting procedure

### 1. Select observable behavior

Record the TypeScript reference revision, the chosen consumer operations, and
their observable failures, ordering, cancellation, recovery, and compatibility
promises. Read both the declarations and the implementing paths. Translate the
behavior rather than copying Effect layers, service tags, or the source file tree.

Done when each selected promise has an owner and a consumer-level verification
path. Existing SDK and wire contracts remain the default assertions.

### 2. Declare the capability and its exposure

Keep input/output schemas and capability contracts independent of providers.
Separate published operations from trusted server methods and server-only work.
For each published operation, identify the source of trusted caller context and
the module that enforces its authorization. For trusted methods, state whether
they enforce actor permissions or grant administrative authority.

Carrier bindings choose URLs, methods, framing, and delivery mechanics. One
operation may have several bindings. Register exposure explicitly; declaring a
table, importing a type, or adding a server method does not publish an operation.

Done when the contract can be consumed without its provider and each remotely
reachable operation has an explicit registration and authorization owner.

### 3. Implement portable behavior

Keep application and provider behavior `no_std` with `alloc`. Use synchronous
functions for immediate work and portable futures for workflows that suspend.
The host polls futures and executes external work; polling must not perform
blocking platform IO. End mutable borrows and release guards before suspension.

Use typed host-work interfaces owned by the concern that needs them. The common
executor must not grow Passport-, Snapshot-, or application-specific request
variants. Application composition connects selected providers to executors.

Document the acceptance point, cancellation before/after acceptance, result
correlation, late completions, capacity, shutdown, and uncertain write outcomes.
A future is an in-memory continuation, not a durable checkpoint. A dropped waiter
does not by itself undo accepted external work.

Done when a host can drive the behavior without domain-specific branches in its
general scheduling code, and the lifecycle promises are explicit.

### 4. Bind storage where required

Use the application's Store setup through module-owned namespaces. Describe
tables, indexed access, constraints, and atomic changes in terms the Store
contract supports. Keep backend-specific names and SQL in the Store implementation.
Module-specific repository helpers may express domain operations over Store.

Define the consistency required by each read and transaction. Check mutable
authority and dependent writes together where correctness requires it. A record
cache hit supplies a snapshot; prefetch changes scheduling, not required semantics.
Queries requiring a coherent snapshot must not silently mix unrelated cache hits.

For existing installations, specify schema migration and compatibility with
persisted records. When sharing a transaction across modules, use their owned
interfaces rather than writing another module's private tables.

Done when the storage guarantees and schema ownership are declared, and swapping
conforming backends does not change the provider's implementation or assertions.

### 5. Compose and verify a usable flow

Application composition chooses providers, carrier bindings, Store/cache setup,
and host executors. Exercise the selected flow through its real client or published
operation. A local browser Store is local persistence; remote data access and
synchronization require their own explicitly published behavior.

Apply [TESTING.md](../../TESTING.md). Reuse assertions across implementations, put
construction in adapters, and name the consumer promise before adding a lower-seam
contract. Use compiler/structural checks for dependency and portability rules.

Done when the selected consumer contract passes, required checks pass, and the
module's interface documents its ownership and lifecycle. Record translation
decisions beside the interface; update this guide only for a reusable rule.

## Port record

Each port's plan should record these facts, rather than introducing another
framework or copying the whole guide:

- Selected reference revision and consumer flow.
- Capability contract, selected provider, and application composition.
- Published operations, trusted methods, authorization owner, carrier bindings.
- Store namespace/schema and required transaction/query guarantees, if applicable.
- External work, acceptance/cancellation/recovery rules, and cache policy.
- Persistent-data compatibility and consumer verification commands.
