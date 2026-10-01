# Store owns application state and residency

Store owns transactional application state, including ephemeral state supplied by a memory driver, rather than Transport maintaining a second application-state transaction system. Transport retains its invocation, connection and execution bookkeeping; Testy's calculator becomes a Document-backed application object rather than executor-held domain state.

Modules declaratively describe their stored data and translate domain requests into generic Store requests. Store owns shared storage and residency machinery; drivers translate loading and commit requests into their chosen storage operations, without Store understanding Document-specific semantics.

The initial interface design centers on basic CRUD and resident primary-key lookup. A nonresident lookup fails without performing hidden IO; richer gathering interfaces and residency declaration details are not selected by this decision. Lookup outcomes and returned-value ownership are defined in [Owned values and read-miss resolution](0003-owned-values-and-read-miss-resolution.md).
