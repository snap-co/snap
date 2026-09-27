# Bend and Snap core

Evaluated on 2026-09-26. This is a source-based assessment, not a benchmark or an
implemented integration.

## Recommendation

Keep the core in Rust. Bend 2 is worth exploring for proofs of small, pure state
transitions or for balanced compute-heavy algorithms. Its most relevant potential
benefit is stating and checking laws, rather than accelerating Snap's current
calculator, transport or commit loop. A separate Bend model does not prove the
Rust implementation without a correspondence argument.

This review inspected Bend commit
`574b6d39a235b539eb19a5c532993a0abb3d11ad`, release 2.0.29. The site links to Bend 2,
the successor to HigherOrderCO/Bend. Its README explicitly says Bend 1 programs
and HVM do not carry over. Older descriptions of automatic HVM parallelism do not
describe this implementation. [1]

## What Bend 2 offers

- A pure functional language with Python-shaped syntax, algebraic data types,
  pattern matching, explicit types and first-class functions. [2]
- Dependent types, equality propositions and proofs written as definitions. A
  `law` states a proposition; a corresponding definition supplies its proof.
  Safe recursion must structurally decrease. [2]
- Affine ownership: values are consumed at most once by default; explicitly
  reusable `Data` values can be copied. Arrays and functions are non-copyable
  `Type` values. This differs from Rust's borrowing and lifetime system. [2]
- Explicit pure fork/join computation. `a b = f(x) g(y)` forks roughly balanced
  work. A `!` call selects GPU execution. The runtime has no work stealing, so
  unbalanced work is a poor fit. IO concurrency instead uses one event loop. [2][3]
- JavaScript, generated C and native executable outputs. The compiler/checker
  implementation is TypeScript. Native compilation uses clang; GPU backends are
  Metal and CUDA. JavaScript execution is sequential. [1][2][4]

Its runtime manages a heap, worker pool and event loop. Affine consumption and
reference counts reclaim values without a tracing garbage collector. These are
runtime mechanisms, not a verified memory-safety result from this review. [3]

## Concrete obstacles for Snap

| Requirement | Bend status at the inspected commit |
| --- | --- |
| Rust-owned runtime and callable library | Native output is a program with `main`. A native library target is planned but unscheduled. [5] |
| Portable core and `wasm32v1-none` | No documented or CLI-supported Wasm output target. Generated C alone does not establish freestanding compatibility. [2][4] |
| Checked signed 64-bit values | Built-in numbers are `Nat`, `U32` and `F32`. There is no built-in signed 64-bit type. Runtime `Nat` stops above `2^48 - 1`. [1][2][5] |
| Stable cross-language boundary | Custom effects can contain C/JS, but their internal ABI has no stability promise. They require rebuilding on compiler updates. [6] |
| Snap's commit and recovery guarantees | Purity and proofs do not supply admission, connection fencing, input retries, atomic publication or database durability. Those protocols still need implementation and verification. |

The website's C/CUDA speed comparisons are publisher claims and selected benchmark
results, not evidence of improvement in Snap. No Snap performance comparison was
run. [1][3]

Proofs establish the propositions written, subject to the checker and execution
assumptions. Bend explicitly permits unsafe/foreign-dependent checking to exit
successfully with a warning. Its Lean formalization says it does not fully match
the implementation and implementation bugs could cause inconsistencies. A passing
exit code or the existence of the Lean model does not establish that a complete
Snap application is correct. [5][7]

## What an experiment would look like

Start with a small state-transition model. Candidate laws include preservation of
committed state on failure and rejection of stale connection generations. Keep the
scope explicit: proving a handler law alone cannot prove the Rust executor's
publication behavior. Reuse concrete cases against Rust to check correspondence;
such comparisons remain tests rather than a proof of equivalence.

For a runtime experiment today, a separate native Bend executable is more realistic
than direct library embedding. The host would own that process and pass immutable
inputs/results. The Rust program and executor would still own validation and
publication. A subprocess is an integration proposal, not an existing Snap adapter.
The extra encoding, process and scheduling costs must be measured.

The official parallel-sum demo illustrates an actual Bend specification:

```text
law tree_is_seq:
  for +d: Nat
  for +i: Nat
  {Par.sum(d, i) == Par.seq(Par.pow2(d), i) : Nat}
```

Its separate proof uses induction to show that the parallel tree sum equals the
sequential specification. This is the useful pattern to investigate for Snap:
state the invariant, then check a proof about the function implementing it. The
law does not remove runtime numeric or memory limits. [8]

## Executed checks

The research agent cloned the official repository outside Snap and ran the pinned
compiler with Bun. The hello-world demo printed `Hello, world!`; the numeric and
parallel-sum proofs reported `All terms check.` Native compilation of the parallel
sum succeeded, and running with `--gpu off --threads 2` returned `2147450880`.
These check a functioning proof and native execution path. They do not measure
speedup, exercise a GPU, audit the kernel or verify a Rust bridge. [8]

## Snap's current requirements

The active consumer is Testy. Transport and execution are independent portable
capabilities. Core code is `no_std` with `alloc`, and structural checks compile
portable libraries for `wasm32v1-none`. Hosts own execution and external IO.
See [Architecture](../ARCHITECTURE.md).

The [`Program` interface](../crates/execution/src/program.rs) has synchronous
`admit` and `attempt` entry points. Programs retain no invocation state and perform
no IO. Admission returns Ready, Need or Reject. An attempt returns Need, a proposed
state/result, or failure. These are ordinary Rust calls, not a stable binary ABI
or a sandbox.

The [executor](../crates/execution/src/executor.rs) serializes operations through
one application-wide gate. It deep-copies the committed JSON state for an attempt,
validates the proposed output and state, then publishes the proposal. Need and
failure discard tentative edits. Internal parallel computation would not, by
itself, permit concurrent operation commits.

The [Testy implementation](../apps/testy/src/program.rs) uses checked signed
64-bit arithmetic. Its checked-add fixture deliberately requests a missing input
after editing private state, exercising rollback and retry. Any alternative
handler implementation must preserve these semantics.

[Resident Store](../crates/store/src/transaction.rs) has a separate contract.
A miss poisons the transaction and requires explicit loading and a later caller
invocation. It is not execution's automatic Need/retry mechanism. Its host backend
owns durability and distinguishes confirmed rejection from indeterminate commit.

The current execution flow is:

```text
Verified call -> FIFO application gate -> admission -> private attempt
                                                       |
                         +-----------------------------+----------+
                         |                             |          |
                        Need                          Fail      Commit
                         |                             |          |
                discard tentative state             discard    validate
                host supplies read input                       publish
                retry same invocation                          complete
```

The [local platform](../platforms/local/src/lib.rs) exposes `submit`, `step` and
input supply to hosts. The current [native fixture](../platforms/local/src/native.rs)
requires an immediate, nonblocking input resolver. A long-running external worker
would require a host that drives these interfaces asynchronously, rather than
blocking that resolver or performing IO inside `Program::attempt`.

## How to evaluate a language experiment

A useful experiment must name the improvement and compare against the existing
Rust implementation. Syntax alone does not establish a runtime benefit.

- Preserve checked `i64` behavior, including overflow and division edge cases.
- Preserve private-state rollback, input discovery, validation and commit ordering.
- Account for encoding, allocation and copying across any language boundary.
- Verify the actual native and Wasm targets required by the selected consumer.
- Measure a representative operation, including adapter overhead, against Rust.

There are already explicit copying costs in Rust: execution clones the committed
JSON record, Testy clones and deserializes that record into a calculator, and Store
clones resident records and indexes for each transaction. These are implementation
choices, not measured bottlenecks. A language comparison must not attribute a
benefit from changing these choices to the new language alone.

For a parallel-compute experiment, include a host-side Rust baseline using
[Rayon](https://github.com/rayon-rs/rayon). Its parallel iterators and `join` offer
data-parallel execution without a second application language. Keep the thread
pool in host code. Rayon's default WebAssembly behavior falls back to sequential
execution; browser threading requires an adapter and configuration. This baseline
would compare compute choices, not change Snap's serialized commit contract.

## Bend sources

Links pin the inspected revision where applicable.

1. [Official site](https://bend-lang.com/) and [README](https://github.com/bendlang/bend/blob/574b6d39a235b539eb19a5c532993a0abb3d11ad/README.md).
2. [Language guide](https://github.com/bendlang/bend/blob/574b6d39a235b539eb19a5c532993a0abb3d11ad/guide/GUIDE.md).
3. [BendRT paper](https://github.com/bendlang/bend/blob/574b6d39a235b539eb19a5c532993a0abb3d11ad/bend2/docs/BendRT/main.typ).
4. [Compiler CLI and output targets](https://github.com/bendlang/bend/blob/574b6d39a235b539eb19a5c532993a0abb3d11ad/bend2/main.ts).
5. [Unsupported behavior and planned work](https://github.com/bendlang/bend/blob/574b6d39a235b539eb19a5c532993a0abb3d11ad/WONTFIX.txt).
6. [Foreign effects and ABI](https://github.com/bendlang/bend/blob/574b6d39a235b539eb19a5c532993a0abb3d11ad/guide/EFFECTS.md).
7. [Lean formalization and implementation mismatch warning](https://github.com/bendlang/bend/blob/574b6d39a235b539eb19a5c532993a0abb3d11ad/bend2/bend.lean).
8. [Parallel sum implementation and proof](https://github.com/bendlang/bend/tree/574b6d39a235b539eb19a5c532993a0abb3d11ad/demos/pure_par_sum).
