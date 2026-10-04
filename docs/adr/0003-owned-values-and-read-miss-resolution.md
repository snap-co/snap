# Owned values and read-miss resolution

This decision remains accepted. Terminal NX without handler-visible absence is an open implementation requirement.

Store initially returns owned copies rather than mutable references or opaque access handles, keeping application code independent of borrowed-record lifetimes. Changing a returned copy does not change Store; writes use explicit Store operations. Copy-on-write and other return-value optimizations are deferred until needed.

Operation-facing primary-key reads return an owned value on HIT or invalidate the current attempt on MISS. Read resolution distinguishes LOAD, which makes the requested value resident and permits an explicit fresh attempt, from NX, which terminates the whole operation without re-entering the handler. NX means the requested record does not exist; it is not a status attached to a nonexistent record or a handler-visible value that application code must branch on. A loading failure is never evidence of NX. MISS handling is defined in [Desired-state residency and explicit retry](0004-desired-state-residency-and-explicit-retry.md).

A complete primary-key index is an acceptable way to establish absence, and substantial memory use is acceptable when it improves access. This decision does not require a complete index or choose its representation; an authoritative key lookup can also resolve a read to NX.

There is no generic residency preflight in Transport and no required-existing versus creates-new declaration for admission to interpret. Handlers discover their own reads. Creating a record does not require reading that record first; duplicate-key validation belongs to insertion and commit rather than an application pre-read. Any read performed by a creation operation follows the same MISS resolution rules as another read.
