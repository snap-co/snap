# Desired-state residency and explicit retry

This decision remains accepted. MISS-driven residency resolution, explicit fresh attempts after LOAD, and admission without operation-specific residency preflight are open implementation requirements.

Every read MISS ends the current attempt, discards its staged application writes and updates a residency manifest to request the missing record. The Store driver resolves that desired state through loading outside application execution. If the requested record does not exist, resolution terminates the operation with NX without re-entering the handler. Multiple connections reference one shared resident copy of each record; operations receive their own copies rather than a connection owning a duplicate resident data set.

Residency requests are platform control state, separate from the application transaction that failed. Loading does not resume the failed stack or automatically rerun the operation; execution requires an explicit fresh attempt. A failed fetch cannot establish NX.

Store may represent each addressed primary key with a resident slot containing loading status and an optional reference to its data. Desired residency and actual loaded state are separate facts. A complete in-memory primary-key index and compact slot metadata are possible representations, not required optimizations; raw pointers, fixed-width layouts and copy-on-write are not selected here, and operations still receive owned values.

Store combines logical residency references so releasing one owner's requirement cannot unload data still needed by another. When the last reference is released, a replaceable eviction policy decides whether data is unloaded immediately or retained until memory is needed. Start with immediate unloading; eviction algorithms and metadata representation can be changed later. These references are an explicit residency contract, not a requirement to use Rust's built-in reference counting.

In-flight loads obey the latest residency target selected from references and eviction policy. A late completion cannot undo an unload decision, even if the physical fetch could not be cancelled. Unloading releases memory, not the stored record.

Logical connections own their connection-associated references, not physical sockets. Creating a logical connection begins populating its desired residency manifest using Access eligibility and the owning modules' data declarations. Temporary socket loss preserves references through the configured reconnect lifetime; explicit close or expiry releases them. Access informs this process but does not own it, as described in [Access eligibility and module policy](0005-access-eligibility-and-module-policy.md).

Admission does not inspect operation-specific Store requirements or wait for a residency preflight. Prefetch should make read misses uncommon; handlers discover their own reads and any MISS follows the bounce-and-resolve mechanism. Mechanical readiness checks and optimizations based on miss rates are deferred.
