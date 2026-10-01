# Snap

Snap provides shared interfaces, reusable domain modules and interchangeable platform drivers. Applications compose these capabilities into their own behavior.

## Language

**Platform**:
The environment that supplies drivers, external effects and nondeterministic inputs to an application. A platform may be entirely in memory.
_Avoid_: Native platform as a synonym for every platform.

**Snap interface**:
A shared contract connecting application and module behavior to platform drivers, including the guarantees callers can rely on. Transport and Store are Snap interfaces.

**Snap module**:
A reusable domain capability built on Snap interfaces rather than specific to an application. Document, Identity, Access and OIDC are Snap modules.

**Access**:
Snap's framework-level ACL module for granting identities roles on registered resources and relating resources through access links. It determines eligibility without owning residency or client synchronization; framework and module code can use that eligibility for guards, prefetching, fetches or subscriptions.

**Access eligibility**:
An identity's effective permission to read or invoke guarded operations on a resource. Eligibility can inform a residency manifest but does not itself load or transmit the resource.

**ACL grant**:
A framework-managed assignment of a role to an identity for a resource. A qualifying grant permits an operation's Access guard, not every possible domain mutation.

**Access link**:
A framework-managed parent-child relationship between resources used by Access. It is not limited to Document resources or relational storage.

**Application**:
Application-specific behavior and policy composed from Snap interfaces and modules, independent of concrete platform drivers. Its choice of platform is separate from that behavior.

**Composition**:
The host assembly that selects platform capabilities and connects them to application behavior through shared interfaces. It can depend on both platform implementations and application code without making application behavior depend on those implementations.

**Runtime host**:
Execution integration for a particular environment that connects application behavior to selected platform drivers. It does not imply dynamic code loading or require every available driver.

**Operation**:
Application or module behavior invoked through Transport that reads and stages changes through Store without performing external IO.

**Attempt**:
One execution of an operation against private transactional state. A failed attempt discards its staged writes rather than retaining a suspended application stack.

**Controller**:
Behavior that responds to committed Store changes and can perform external effects through platform-supplied capabilities.

**Store residency**:
Store's knowledge of which committed data is available for operations without external IO. Residency is shared across stored domains, not specific to Document.

**Store declaration**:
A module-supplied description of its stored data and generic handling requirements. It expresses storage needs without teaching Store the module's domain behavior.

**Residency manifest**:
The desired state of which Store records should be available in memory, accounting for residency references and the selected eviction policy. It drives loading and unloading through the selected Store driver, separately from application data changes.

**Residency reference**:
A logical owner's requirement that Store data remain resident, including references held by logical connections rather than physical sockets. Multiple connections can reference the same shared resident record without creating separate resident copies.
_Avoid_: Rust reference count as a synonym for the residency contract.

**Store eviction policy**:
A replaceable policy deciding whether dereferenced Store data is unloaded or retained for reuse. Referenced data remains needed; dereferencing makes it eligible for policy-controlled unloading.

**Resident slot**:
Store's internal metadata and optional loaded value for a primary key. The slot's loading state is distinct from whether residency is desired.

**Client holdings manifest**:
A client's declaration of the resources and versions it already holds, used by a module to identify missing or stale client data. It is distinct from the server's residency manifest.

**HIT**:
A Store lookup whose value is available without external IO, including the operation's own staged writes.

**MISS**:
A Store read that cannot supply the requested value from resident data and invalidates the current attempt. Resolution may load the value or establish NX; MISS alone does not prove nonexistence.

**LOAD**:
Resolution of a read MISS that makes the requested value resident and permits an explicit fresh attempt. It does not resume or automatically rerun the failed handler.

**NX**:
A terminal operation failure when resolution of a requested read establishes that the record does not exist. It is not a handler-visible absence value, a record's residency status or a failed IO request.
_Avoid_: Missing as a synonym for both unresolved MISS and confirmed NX.

**Driver**:
A platform-selected implementation at a Snap interface. Memory and network drivers are alternatives for Transport; memory and database drivers are alternatives for Store.

**Transport carrier**:
The physical communication and wire encoding used to exchange Transport commands and responses. Carrier handling is separate from operation execution, application policy and logical-connection bookkeeping.

**Memory driver**:
A driver whose operations use process memory without external IO. It is a driver choice, not an application mode.

**Test cartridge**:
Supplied application behavior that exercises a platform's guarantees, including its handling of invalid instructions and failures.

**Testing platform**:
A controlled environment that loads application behavior and supplies its drivers, external inputs and failures.
