---
title: 'Story 3.13: Keep referenced values in step between Backend and script'
type: 'feature'
created: '2026-09-28'
status: 'in-progress'
baseline_commit: '0b877cded512947314912ff852462b10d97cc098'
route: 'dispatch'
review_loop_iteration: 0
context:
  - '{project-root}/_bmad-output/implementation-artifacts/epic-3-context.md'
  - '{project-root}/_bmad-output/implementation-artifacts/spec-3-11-carry-hidden-metadata-on-values-the-script-cannot-touch.md'
---

<frozen-after-approval reason="human-owned intent — do not modify unless human renegotiates">

## Intent

**Problem:** Story 3.11 gives values Reference IDs, but nothing flows by them. A Backend that changes an object while handling a `Call` cannot tell the Script. A Script that writes a Backend-supplied variable or collection never tells the Backend. Either side must re-fetch its state with a second round trip (FR-28, LANGUAGE-REFERENCE §8 "Reference IDs and host-side modifications").

**Approach:** Modifications flow both ways, always by Reference ID and always as the whole new value.
- **Inbound.** A `Call` reply may carry `modifications: [{ref, value}]`. The Daemon applies each one before the Script resumes.
  - A referenced collection changes in place and keeps its identity.
  - A referenced scalar location is replaced there and only there.
- **Outbound.** The execution tracks every referenced location the Script writes. Its successful `Result` lists each written location once, with its final value, under `modifications`.

## Boundaries & Constraints

**Always:**
- LANGUAGE-REFERENCE §3/§8 are normative. A Reference ID names a location, never a copy.
- A Script never observes a secret. Applying a modification changes only values the Script could already see change: the same collection, or the same location.
- A Backend's modification is not a Script write and is never reported back.
- Values without modifications travel exactly as before. A reply or result without the key is unchanged.

**Decisions (agent, 2026-09-28 — Erdem delegated: "kendin tahmin et", no approvals):**
1. **Reply shape.** A `Call` reply is `Result {value}` or `Result {value, modifications}`. `modifications` is an array of maps with exactly the keys `ref` (a non-empty string) and `value` (a wire value, which may contain holders).
   - Anything else in `modifications` fails the call with `host.function_failed` (a malformed reply), before any modification is applied. This covers a non-array, an entry that is not such a map, a bad `ref`, and a malformed holder.
   - An `Authorize` answer never carries modifications; a `modifications` key there makes the answer invalid (`handler_invalid`).
2. **Applying.** All modifications in one reply are applied in order, and a later entry for the same `ref` wins.
   - **Unknown refs.** A `ref` the execution does not hold (never seen, or its location or collection is gone) is ignored.
   - **Collections.** A `ref` naming a collection replaces that collection's contents with the new value's contents, keeping its identity and its own secret.
     - Nested values in the new contents are decoded fresh, as a reply's value is: nested holders set their secrets, and nested collections are new collections.
     - A kind mismatch fails the call with `host.function_failed`: an array ref given a non-array, or an object ref given a non-object.
   - **Scalar locations.** A `ref` naming a scalar location replaces the value at that one place, and the place keeps its secret.
     - If the new value is a collection, the place now holds that new collection, and the place's secret stays.
     - A holder around the new value is refused as malformed. A modification carries its value; the `ref` names the place.
3. **Ref index and duplicates.** The execution keeps one index from Reference ID to its location or collection.
   - The first registration of a `ref` wins. A later value arriving with a `ref` already indexed to a different live location keeps that secret on its own location for the wire, but it is not indexed, so modifications naming that `ref` reach the first.
   - An `hx:` ref that the execution did not generate is treated like any Backend ref. This resolves both deferred Story 3.11 entries on ref collisions.
4. **Script writes.** A write is recorded when the Script:
   - assigns to a variable, property or element whose place has a secret;
   - stores into, or appends to, an array or object that has its own secret, recorded by the collection's `ref`.
   
   Generated refs count, because the Backend saw them in a `Call`. Every write between the first and the end is folded, so each `ref` is listed once, in the order of its first write, with its value when the execution ends. Writes by the Backend's modifications are never recorded.
5. **Result shape.** On success the result is `{value, modifications?, findings?}`, with `modifications` present only when non-empty. Each entry is `{ref, value}`. The value is encoded as a `Call` argument would be, with every nested secret emitted (generated ones included) and no new refs generated.
   - A failed execution (`Error`) carries no modifications: the Script's state is discarded.
   - The output-size budget and the frame check measure the whole payload, `modifications` included. `findings` stays outside the budget (Story 3.10).
   - A secret-unaware Backend whose Script writes a place it passed to a host call now sees `modifications`. Everything else stays byte-identical.
6. **Budgets.** Applying modifications charges memory like a reply's value and counts no allocations. The time spent decoding and applying them is charged to CPU like reply conversion. Tracking writes is not charged.

**Never:**
- no rollback of any effect;
- no modifications on `ExecutionStart` itself or on `Error`;
- no Script-visible notification that a modification happened;
- no new crate edge;
- no `unsafe`.

## I/O & Edge-Case Matrix

| Scenario | Input / State | Expected Output / Behavior |
|---|---|---|
| Collection in place | `let a = {n: 1}; let b = a; f(a); return b.n;`; the reply modifies `a`'s generated ref to `{n: 2}` | `2`; `b` observes it (same identity) |
| Scalar place | starting `n` held as `8` with `r1`; `let m = n; f(); return [n, m];`; the reply modifies `r1` to `9` | `[9, 8]` |
| Unknown ref | reply modifies `"nope"` | ignored; the Script continues |
| Malformed | `modifications: 1`, an entry missing `ref`, `{ref: "r1", value: {__secret: {}, value: 1}}`, an array ref given `5` | `host.function_failed` on the call; nothing applied |
| Script writes scalar | starting `n` held `8`, `r1`; `n = 9; return {ok: true};` | `{value: {ok: true}, modifications: [{ref: "r1", value: 9}]}` |
| Script writes collection | starting `u` held with `r2`; `u.x = 1; u.x = 2; return 0;` | `modifications: [{ref: "r2", value: {…, x: 2}}]` (once, with the final value) |
| Unwritten | a held `n` only read | no `modifications` key |
| Generated ref written | `f(i); i = 5; return 0;` | `modifications` lists `i`'s `hx:` ref with `5` |
| Backend write not echoed | a reply modifies `r1`; the Script does not write `n` | no `modifications` |
| Duplicate ref | starting variables `a` and `b` both held with `ref` "r9"; a reply modifies "r9" | only `a` (first registered) changes |
| Error | the Script writes `n`, then fails | `Error` with no modifications |
| Output budget | `modifications` push the payload past `output_size_bytes` | `budget.output_size_exceeded` |

</frozen-after-approval>

## Code Map

- `crates/hexput-interpreter`:
  - add a ref index (ref → collection id or place) that is maintained when secrets attach or are generated, honouring first-wins;
  - apply modifications through `HostCall::resume_held` or a new `HostCall::resume_with(held, modifications)`, taking decoded `(ref, Held)` pairs; return a kind-mismatch error to the driver;
  - add a written-ref set that records the Script's writes at the variable, property and element store sites and at the collection store and append sites (decision 4);
  - `Outcome::Finished` exposes the final `(ref, Held)` list for the written refs (for example `Finished { result, modifications }`).
- `crates/hexput-exec/src/wire.rs`: decode a reply's `modifications` (decision 1), using the existing holder decoding for values; encode result modifications (decision 5); `payload_size` over `{value, modifications}`.
- `crates/hexput-exec/src/lib.rs`:
  - reply handling: validate everything first, then apply;
  - an `Authorize` answer carrying `modifications` is `handler_invalid`;
  - the result returns the modifications;
  - the output charge covers them.
- `crates/hexput-script`: emit `modifications` in `Result` (with `findings` after it, per Story 3.10); run frame checks over the whole payload.
- Tests (`tests/interpreter.rs`, `tests/exec.rs`, `tests/connection.rs`): every matrix row. Update the Story 3.11 byte-identical test if its Script writes a sent place.
- Docs:
  - LANGUAGE-REFERENCE §8: a dated note recording decisions 2–5;
  - the Spine: a dated FR-28 line on the reply and result `modifications` shapes;
  - AGENTS.md: the Epic 3 paragraph, the test count, and that Epic 3 is code-complete (next step: Epic 3 review, then Epic 4);
  - `deferred-work.md`: resolve the two Story 3.11 ref entries;
  - `sprint-status.yaml` is left to the workflow.

## Tasks & Acceptance

**Execution:**
- [ ] Interpreter: ref index, apply, write tracking, finished modifications.
- [ ] Exec: decode and validate, apply, encode, budget.
- [ ] Script: result shape.
- [ ] Tests for every matrix row.
- [ ] Docs.

**Acceptance Criteria:**
- Given a reply with valid modifications, when the Script resumes, then every named live location or collection holds its new value, collections keep their identity, and copies are unchanged.
- Given a Script that writes referenced locations, when it succeeds, then `modifications` lists exactly those refs, once each, with their final values.
- Given no reply modifications and no referenced writes, when a Script runs, then its wire output is byte-identical to before this story.

## Spec Change Log

## Verification

**Commands:**
- `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings`
- `python3 scripts/check-crate-graph.py`
- `cargo test --workspace --locked`
