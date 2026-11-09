# Boundless patches

Patches on `boundless`, each on upstream history. Base: upstream `rust-v0.61.0`
(643cb242).

## Refuse a compile or check that runs out of stack, and release without overflowing it (Boundless #731)

**Reason.** At 0.61.0 a schema an application accepts from a user can end the
process: compiling, checking and releasing recurse once per reference hop, and
a stack overflow aborts.

- Compile: `compile_pending_property_validators`
  (`keywords/unevaluated_properties.rs`) and `compile_pending_items_validators`
  (`keywords/unevaluated_items.rs`) follow `$ref`, `$dynamicRef` and
  `$recursiveRef` on the call stack, outside the worklist
  `compile_with_internal` drains. A chain of definitions
  `{"allOf": [{"$ref": next}], "unevaluatedProperties": false}` overflows a
  2 MiB thread at about 400 hops; with `unevaluatedProperties: false` only at
  the root, at about 700.
- Check: a chain `{"allOf": [{"$ref": next}]}` compiles (the worklist) and then
  overflows `iter_errors` and `validate` at about 5,000 hops on a 2 MiB thread,
  for a valid and an invalid instance alike; with `anyOf`, `is_valid` overflows
  at about 26,000 hops.
- Release: a `$ref` whose target is already compiled owns it
  (`Context::reuse`, `TargetNode::Owned`). Definitions that each refer to the
  one before them, listed in that order, compile without recursion and leave
  the last one owning the whole chain; dropping it recurses once per hop
  through the compiler-generated drop of `SchemaNodeInner`. 20,000 hops
  (a 1.2 MB document) overflow a 2 MiB thread when the finished validator is
  dropped, and when a build that fails after compiling them drops what it
  built.

**What upstream does there.** It aborts the process. Upstream's fixes c1936557
(worklist for `$ref` targets at compile) and 7eb61284 (restoring pending
compile entries across `$ref` cycles under `unevaluated*`) cover none of these
shapes, and upstream has no `Drop` for its nodes: `tests/ref_chain.rs` releases
a 1,000-hop forward chain, whose targets the validator holds flat. The patch
adds an error where there was none; it changes no cancellation, timeout, close
or retry path, and no answer of a compile or check that finishes.

**The patch.** `stack::low()` compares `stacker::remaining_stack()` with a
256 KiB red zone (`stack::RED_ZONE`); on `wasm32` it is always `false`.

- Compile: `compiler::compile_with_internal` and the two `unevaluated*`
  compilers above test it first and return
  `ValidationErrorKind::StackExhausted` at the place they stopped.
- Check: `ValidationContext::stack_low()` tests it and sets
  `ValidationContext::stack_exhausted`. `NodeValidators::is_valid`,
  `SchemaNode::validate`, `collect_errors` and `evaluate_at`, the
  `PendingSchemaNode` methods and `mark_evaluated_properties_impl` /
  `mark_evaluated_indexes_impl` test it first and return their cheapest answer.
  `Validator::is_valid`, `validate`, `iter_errors` and `evaluate`, and
  `valid_under_every_outcome` / `errors_under_every_outcome`, read the flag
  after the walk: `iter_errors` yields exactly one `StackExhausted` error,
  `validate` returns it, `evaluate` reports it as its only error and `is_valid`
  returns `false`, so nothing computed after the flag was set becomes a verdict.

- Release: `Drop for SchemaNodeInner` (`node.rs`), the one type every owning
  edge between nodes passes, tests it; when low it moves the node's validators
  out and drops them inside `stacker::grow` on a new 1 MiB segment
  (`stack::RELEASE_SEGMENT`), as rustc's `ensure_sufficient_stack` does for its
  recursive passes. A release cannot be refused, so it continues instead; the
  segments live only while the release runs. Release memory grows with
  ownership depth: a recursive release holds several segments at once, also
  when it cleans up after a failed build. A review measured 6 and 18 segments
  beyond the caller's stack at 20,000 and 50,000 hops (an instrumented copy,
  so measurements, not bounds). Nothing here reserves or caps these mappings.
  A stack `stacker` cannot measure (a caller's own fiber) is not protected:
  release there from inside `stacker::grow`.

A thread whose whole stack is at most the red zone can no longer compile or
check anything. `tests/ref_chain.rs` built on a 256 KiB thread; its thread is
now 512 KiB, the red zone and the same 256 KiB to work in.

Not covered, with no abort observed: recursion bounded by the JSON nesting of
the schema or the value (the `single_validator()` fast paths, value equality
and hashing) and `unevaluated_properties`' `collect_static`.

`ValidationErrorKind` gains a variant. The binding crates (`jsonschema-py`,
`jsonschema-rb`, `jsonschema-wasm`) match it exhaustively, are not updated and
do not compile on this branch: Boundless builds only `jsonschema` and the
crates it depends on. Handling the variant in the three bindings is work an
upstream offer has to include.

**Regression evidence.** `crates/jsonschema/tests/stack.rs`, on 2 MiB threads:
a 5,000-hop and a 3,000-hop `unevaluatedProperties` chain are refused at
build; `validate`, `iter_errors` and `evaluate` of a 50,000-hop `allOf` chain
report `StackExhausted` for a valid and an invalid instance; `is_valid` of a
50,000-hop `anyOf` chain is `false`; the same chains at 20 hops answer as
before. With `stack::low()` returning `false` each of the first three ends the
test process with SIGABRT (macOS arm64, 2026-11-10). A 20,000-hop chain of
targets built first is released after a finished build and after a failed one,
and `validate`, `iter_errors` and `evaluate` through it report
`StackExhausted`; without the `Drop` each of the three ends the test process
with SIGABRT (macOS arm64, 2026-11-10). `cargo test -p jsonschema`
passes on macOS arm64, upstream's suites included.

**Boundless code that relies on it.** `crates/json/src/engine.rs`, which runs
every compile and check on a 2 MiB segment and turns `StackExhausted` into
`input_too_large` (`schema_stack_bytes`). It releases compiled schemas on
native thread stacks, where the release above continues on further segments;
no Boundless limit reserves or caps those mappings, and they are not counted
on any account.

**Upstream.** Not reported yet. The patch retires when a release stops these
shapes and every case of `tests/stack.rs`.

## Translate capturing groups as non-capturing (Boundless #660)

**Reason.** `pattern` and `patternProperties` ask only whether a match exists,
but the ECMA-262 translator preserves capturing groups. A pattern consisting
of `[01]*1[01]{20}[^01]` and 1,000 `(a?)` groups over 100,000 pseudo-random
bits allocated about 132.7 MB in one search. Non-capturing groups use about
4.1 MB (about 2.2 MB under Boundless's 1 MiB pattern limits).

**Patch.** The translator replaces ordinary and named capturing openers with
`(?:`. Its offsets track bytes removed as well as added. Unsupported
lookaround and backreferences return before the visitor, as upstream does;
the patch changes no cancellation, timeout, close, retry or error path.

**Regression evidence.** Seven unit cases in `crates/jsonschema-regex/src/lib.rs`
cover ordinary, repeated, nested and named groups, unchanged flag and
non-capturing groups, class escapes and parentheses that are not groups.
Boundless's content-memory tests also check match answers and the capture
case's allocation. Implementation verification is recorded with the pinning
change; the earlier design probe passed all 321 translator tests.

**Boundless code that relies on it.** `configured()` and `pattern_matches` in
`crates/json/src/validation.rs`.

**Upstream.** PR drafted, not submitted. Retire this patch and its pin when an
upstream release translates match-only captures as non-capturing. Upstream
submission waits until the Boundless implementations are verified.
