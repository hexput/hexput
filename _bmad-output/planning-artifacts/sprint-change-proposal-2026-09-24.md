---
title: 'Sprint Change Proposal — Registered Methods and Value Secrets'
date: '2026-09-24'
trigger: 'Story 3.1 planning (spec-3-1-call-a-registered-host-function-from-a-script.md)'
scope: 'Moderate — new FRs and stories inside the existing epic structure'
status: 'applied — decisions confirmed by Erdem 2026-09-24'
mode: 'batch'
---

# Sprint Change Proposal — Registered Methods and Value Secrets

## 1. Issue Summary

While deciding the wire shape of Story 3.1's outbound host call, Erdem asked for the call message to be generic rather than function-specific, because the Backend will also expose **methods bound to objects**, much like methods in other languages. That need is absent from the brief, PRD, Spine, epics and LANGUAGE-REFERENCE. Category: **new requirement from the stakeholder**.

Requirements as stated (2026-09-24):

- `registerMethod(objKey, fn)` binds a Backend function to an object key.
- The key lives in the object's hidden `__secret.key`. `__secret` may hold more Backend data, for example `ValueHolder { key: Option<String>, ref: String, value, rest }`.
- Every value that crosses to the Backend carries a reference id in `__secret.ref`: objects, strings, integers and anything else. The Backend may supply it; otherwise it is generated. This lets the Backend track the values it changes during a remote call and report those changes in the call's finish (reply) message.
- A method call sends the object itself.
- Argument and receiver nesting is capped by a runtime limit, default 12.
- `__secret` always travels to the Backend and may be edited there. Inside Hexput it is untouchable: reading yields `null`, a script-written `__secret` is silently ignored, and `for … in` skips it.

## 2. Impact Analysis

- **Epics.** Epic 3 grows by three stories (3.11–3.13) and two amended ones:
  - 3.1 gains its wire, error and depth criteria.
  - 3.7 gains the configurable argument depth.

  Epic 8's host-call stories (8.3, 8.6) gain a `registerMethod` criterion, and Epic 9's Story 9.4 gains the method-override diagnostic. No epic becomes obsolete. No order change is needed: Epic 3 still precedes Epic 8.
- **PRD.**
  - New FR-27 (Registered Methods) and FR-28 (Value Secret, Reference IDs, host-side modifications).
  - FR-6 gains the generic `Call` and `host`-error consequence.
  - Three glossary terms are added, the Config glossary names the depth limit, and MVP §6.1 lists both FRs.
  - The MVP stays achievable. Scope grows, but nothing is removed or invalidated.
- **LANGUAGE-REFERENCE.**
  - §3: the Value Secret and its invisibility rules.
  - §7: the `host` category, plus the new `depth` code.
  - §8: host-call semantics, Registered Methods, Reference IDs and modifications.
  - §11: methods are not classes or `this`.
  - No grammar change: a method call is an ordinary member call, so Epic 9's tree-sitter grammar is unaffected.
- **Architecture Spine.**
  - Edge `hexput-connection → hexput-rpc`.
  - A fixed wire contract for `Call` and the `{__secret, value}` holder.
  - Capability map rows, and `binds` gains FR-27 and FR-28.
  - No AD changes. AD-3 holds, because `hexput-rpc` never reaches `hexput-enforce`.
- **Technical.** The interpreter's value model gains hidden per-value metadata (Story 3.11). This is the largest code impact and touches every value path. Allocation counting (Story 3.6) must decide whether a Value Secret counts; that is left to Story 3.6.

## 3. Recommended Approach

**Direct adjustment** within the existing plan: add stories to Epic 3 and criteria to Epic 8. Rollback is not applicable, since nothing implemented is invalidated. An MVP review is not needed.

- **Effort:** medium to high, driven by Story 3.11's value-model change.
- **Risk:** medium. Hidden metadata on every value is a trust-boundary feature, and a leak of `__secret` into script view is a security defect.

**Sequencing:**
1. Story 3.1 (generic `Call`).
2. Story 3.11 (Value Secret).
3. Story 3.12 (methods, which need `key`).
4. Story 3.13 (modifications, which need `ref`).

## 4. Detailed Change Proposals (applied)

All edits below are applied in place. Each carries a dated `[Added 2026-09-24]` or amendment marker in its artifact.

| Artifact | Change |
| --- | --- |
| `prds/…/prd.md` | frontmatter `amended`; Glossary: Registered Method, Value Secret, Reference ID; Config glossary names the depth limit; FR-6 new consequence; FR-27; FR-28; §6.1 |
| `prds/…/.memlog.md` | course-correction entry |
| `language/LANGUAGE-REFERENCE.md` | frontmatter `amended`; §3 Value Secret decision; §7 `capability`/`host`/`depth` rows; §8 four decisions (host call, suspension and errors, Registered Methods, Reference IDs and modifications); §11 note |
| `architecture/…/ARCHITECTURE-SPINE.md` | frontmatter; `conn --> rpc` edge; two dated amendments (edge, wire contract); Capability map rows |
| `architecture/…/.memlog.md` | course-correction entry |
| `epics.md` | FR-27/FR-28 inventory and coverage; Epic 3 and 8 descriptions; Story 3.1 +3 ACs; Story 3.7 +1 AC; new Stories 3.11, 3.12, 3.13; Stories 8.3, 8.6 +1 AC |
| `implementation-artifacts/sprint-status.yaml` | `3-11`, `3-12`, `3-13` as `backlog` |
| `implementation-artifacts/deferred-work.md` | methods entry marked promoted |
| `AGENTS.md` | status paragraph, story count, vocabulary, next step |

**Decisions (Erdem, 2026-09-24), replacing the agent's first defaults:**

1. **Locations, not copies (A3).** A Reference ID names a location:
   - For an object or array, the collection itself, shared by identity.
   - For a string, number, bool or `null`, the variable, property or element it arrived in or was passed from.
   - A copy (`let m = n;`) or a computed value carries no Value Secret.
   - A Backend modification of a scalar ref replaces the value at that location only.
2. **Modifications flow both ways.** A script writing a referenced location is reported back in the execution's `Result` under `modifications`, once per ref, with its final value. Erdem's example: Backend-supplied `n = 8`, then `n = 9; return { ok: true };` gives `modifications: [{ ref, value: 9 }]`.
3. **Reference ID source and lifetime (B).**
   - The Backend supplies the ref on values it sends.
   - The Daemon generates one the first time a value without one crosses to the Backend.
   - A ref is stable within one execution.
   - Every value nested in a `Call` travels as a holder.
   - An execution result sends holders only for values that already carry a Value Secret.
4. **Report shape (C).**
   - `modifications: [{ref, value}]`, where each entry is a whole-value replacement.
   - Collections change in place.
   - An unknown ref is ignored.
   - A malformed list is `host.function_failed`.
5. **Method precedence and no override (D2).**
   - A Registered Method wins over an own property of the same name.
   - A script can never override one: a write is `capability.method_override`.
   - The static check reports the write, and so the language server shows it, when the environment declares a never-reassigned starting variable keyed. It stays silent otherwise, per the no-false-positive rule. Story 9.4 gained the matching criterion.

## 5. Implementation Handoff

- **Scope:** Moderate. The backlog grows inside Epic 3 and Epic 8, and there is no replan.
- **Developer (`bmad-build`).** Story 3.1 proceeds on its drafted spec, which already follows the generic `Call`, `host` and depth decisions. Stories 3.11–3.13 follow in order.
- **PM/Architect.** None required beyond confirming the four defaults above.
- **Success criteria:**
  - A script calls a keyed object's method.
  - The Backend sees and edits the Value Secret while the script never can.
  - Backend modifications are visible to the script after the call.
  - Each of these is pinned by tests in `hexput-tests`.

## 6. Addendum — second change (2026-09-24): installer, default ports, local playground

**Trigger.** Erdem asked for a one-line README install that sets Hexput up as a system service, fixed default ports, and a local browser playground with demo host functions and language-server help.

**Decisions (Erdem).** Ports: 7476 TCP+TLS, 7478 WebSocket, 7477 playground. The playground is its own package (`hexput-playground`), embedded in and served by the Daemon by default, off-switchable in System Config, and also a standalone executable.

**Applied.** PRD: FR-9 default-port bullet, new §4.10 with FR-29 and FR-30, MVP §6.1. Epics: FR inventory and coverage map (30 FRs), OQ-1 amendment (the playground is the one HTTP listener, loopback only), Story 5.1/5.2 default-port criteria, Story 7.6 installer criterion, new Epic 10 (Stories 10.1–10.4). Spine: `hexput-playground` crate and edges, the playground as an in-process Backend through `hexput-connection::serve`, `[playground]` System Config, default ports. Sprint status: `epic-10` and its four stories in the backlog.

**Scope.** Minor-to-moderate: additive; no existing story's intent changes. Epic 10 is sequenced after Epic 9 for its language-server story, but Stories 10.1–10.3 need only Epics 2–3.
