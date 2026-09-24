# Snap

Snap separates application capabilities from the providers that implement them.
Applications choose which capabilities and providers they use.

## Language

**Protocol**:
The contract for declaring and invoking operations and describing their inputs,
outputs, and failures.
_Avoid_: using Protocol as the name for all Snap capability contracts.

**Transport**:
A provider of Protocol's operation dispatch and delivery behavior.

**Identity**:
The contract for identifying callers and managing their authentication and sessions.

**Passport**:
A provider of Identity behavior.

**Access**:
The contract for deciding who may perform an action on a resource.

**Acl**:
A provider of Access behavior based on grants and resource relationships.

**Document**:
The contract for structured document state and its mutations, observations, and
synchronization.

**Snapshot**:
A provider of Document behavior.

**Blob**:
The contract for content stored and retrieved as binary objects.

**Bucket**:
A provider of Blob behavior.

**Store**:
The contract for namespaced records, queries, and transactional persistence used
by Snap providers and consuming applications.

**Storage namespace**:
A module-owned collection of storage definitions with an identity distinct from
other modules' definitions.

**Record cache**:
A reusable copy of stored data whose residency does not establish its freshness
or authority.

**Receipt cache**:
A bounded-lifetime record of an operation's outcome used for retry and recovery.

**Published operation**:
An operation explicitly exposed for invocation by remote clients.
_Avoid_: treating every server method or storage declaration as a published operation.
