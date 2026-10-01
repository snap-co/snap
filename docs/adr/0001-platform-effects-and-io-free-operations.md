# Platform-owned effects and IO-free operations

Transport invokes operations, operations read and stage changes through Store, and successful Store commits signal Controllers. External effects and nondeterministic inputs belong to platform-supplied capabilities rather than ambient IO inside operation code, so the same application behavior can run on real or controlled testing platforms.

For now, portable code, compiler checks, dependency rules and agent instructions enforce this architecture cooperatively; this is not a sandbox guarantee for arbitrary native code. Deterministic simulation is a long-term goal, not a strict current replay or same-seed guarantee, and mostly single-threaded execution is sufficient for present work.

Controller interfaces, concurrency, parallelism, distribution and notification-delivery guarantees remain undecided. MVCC, isolation options and Viewstamped Replication also remain undecided; this decision does not select them.
