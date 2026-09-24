#!/usr/bin/env python3
"""Assert the AD-enforcing edges of the workspace crate graph.

The Architecture Spine states several rules of the shape "only X may reach Y
directly". A crate boundary only enforces such a rule once some code actually
imports the crate: a forbidden edge present in a manifest but not yet used
compiles and lints clean. While a crate is still a stub, nothing imports its dependency, so an invalid edge
would surface much later, in an unrelated change, reading as pre-existing.

This script closes that gap by asserting the edges directly against the
resolved dependency graph from `cargo metadata`, so the Spine's rules fail CI
rather than a future code review.

Run: python3 scripts/check-crate-graph.py
"""

from __future__ import annotations

import json
import subprocess
import sys

# (crate, forbidden dependency, architecture decision, why it matters)
FORBIDDEN_EDGES = [
    ("hexput-check", "hexput-interpreter", "AD-8", "the check pass must not be able to execute a script"),
    ("hexput-check", "hexput-rpc", "AD-8", "the check pass must not be able to reach the host"),
    ("hexput-check", "hexput-enforce", "AD-8", "the check pass must not perform capability/budget checks"),
    ("hexput-globalvar", "hexput-plugin", "AD-4", "the Global Variable store must outlive the Plugin actor"),
    ("hexput-globalvar", "hexput-rpc", "AD-4", "the Global Variable store must not be reachable through RPC"),
    ("hexput-session", "hexput-config", "AD-5", "per-backend Config and System Config must stay separate surfaces"),
    ("hexput-config", "hexput-session", "AD-5", "per-backend Config and System Config must stay separate surfaces"),
]

# Edges an Architecture Decision requires to exist. AD-4 names `hexput-session` as the sole
# caller of `hexput_globalvar::teardown(plugin_id)`, which it cannot be without this edge —
# it was absent when the graph was first derived from the module tree, making the rule
# unimplementable. Asserted so it cannot silently disappear again.
REQUIRED_EDGES = [
    ("hexput-session", "hexput-globalvar", "AD-4",
     "session is the sole caller of hexput_globalvar::teardown(plugin_id)"),
]

# crate -> the exact set of workspace crates allowed to depend on it
SOLE_DEPENDENTS = [
    ("hexput-enforce", {"hexput-exec"}, "AD-3",
     "capability and budget enforcement must be reachable only through the one shared Executor"),
    ("hexput-transport", {"hexput-daemon"}, "AD-1",
     "no crate but the wiring root may branch on a transport type"),
]


# Pure language crates must retain exactly these production boundaries (Spine graph).
EXACT_DEPENDENCIES = {
    "hexput-ast": {"hexput-shared"},
    "hexput-lexer": {"hexput-shared"},
    "hexput-parser": {"hexput-lexer", "hexput-ast"},
    # Story 1.10: AD-8 says `hexput-ast` alone, and `hexput-ast` re-exports the diagnostics
    # shape, so the `hexput-shared` edge the crate was scaffolded with is gone. When Epic 3
    # needs `hexput-shared::budget` here, adding it back is a deliberate amendment.
    "hexput-check": {"hexput-ast"},
    # Story 1.9: the CLI reaches diagnostics through a re-export from the parser and the
    # interpreter. A direct `hexput-shared` or `hexput-ast` edge is not in the Spine's graph and
    # would otherwise only be caught by a reviewer's eye. `hexput-check` is listed because
    # Story 1.10's check command needs it. Story 2.6 moved `is_identifier` into the parser, which
    # took the last use of `hexput-lexer` with it, so that edge is gone.
    "hexput-cli-core": {
        "hexput-parser",
        "hexput-interpreter",
        "hexput-check",
    },
    # Story 2.1: System Config reaches `hexput-shared` alone — never `hexput-session` (AD-5,
    # asserted above too) nor anything that could let it reload, watch or write itself.
    "hexput-config": {"hexput-shared"},
    # Story 2.2: the wire codec is transport-agnostic and sans-IO (AD-1). It reaches the shared
    # envelope and diagnostics and nothing else — no transport, no session, no executor.
    "hexput-port": {"hexput-shared"},
    # Story 2.3: an adapter turns bytes into the Port's envelopes and reaches nothing else — no
    # Session, no executor — so no transport can make a decision the core should make (AD-1).
    "hexput-transport": {"hexput-port"},
    # Story 2.4: the Session registry reaches the wire value it decodes `Init` from, the shared
    # ids, and the Global Variable store it alone tears down (AD-4) — never System Config (AD-5)
    # or anything that executes.
    "hexput-session": {"hexput-port", "hexput-shared", "hexput-globalvar"},
    # Story 2.3: the connection actor drives a Port (the `conn --> port` edge is the Spine's
    # 2026-09-23 amendment) and attaches to a Session; it never reaches a transport (AD-1).
    # Story 2.6: it routes `ExecutionStart` to `hexput-script` (`conn --> script`, amended
    # 2026-09-23) and executes nothing itself. Story 3.1: it holds one `hexput-rpc` call table,
    # writing its executions' host calls and routing their replies (`conn --> rpc`, amended
    # 2026-09-24) — still never `hexput-enforce` (AD-3).
    "hexput-connection": {"hexput-session", "hexput-port", "hexput-script", "hexput-rpc"},
    # Story 3.1: host-call correlation reaches the wire codec alone — never `hexput-enforce`
    # (AD-3), `hexput-exec` (which depends on it, not the reverse) or `hexput-globalvar` (AD-4).
    "hexput-rpc": {"hexput-port"},
    # Story 3.1: capability (and later budget) enforcement reaches the shared vocabulary alone.
    "hexput-enforce": {"hexput-shared"},
    # Story 2.6: Direct Execution reaches the parser, the interpreter's value types, the static
    # check (unused until Epic 3's check mode), the one Executor, and the wire value and error
    # shape (`script --> port`, amended 2026-09-23) — never a transport, never `hexput-enforce`
    # except through `hexput-exec` (AD-3).
    "hexput-script": {
        "hexput-parser",
        "hexput-interpreter",
        "hexput-check",
        "hexput-exec",
        "hexput-port",
    },
    # Story 2.6: the one Executor (AD-3). It alone reaches enforcement; `hexput-rpc` and
    # `hexput-globalvar` join it for Epic 3's Registered Functions and Epic 6's Global Variables.
    "hexput-exec": {
        "hexput-enforce",
        "hexput-interpreter",
        "hexput-rpc",
        "hexput-globalvar",
    },
    # Story 2.3: the wiring root composes every daemon crate, and it alone reaches a transport.
    "hexput-daemon": {
        "hexput-transport",
        "hexput-session",
        "hexput-connection",
        "hexput-script",
        "hexput-plugin",
        "hexput-config",
        "hexput-exec",
    },
    # Story 2.1: the binaries are thin hand-offs. `hexput-daemon` re-exports what its `main` needs,
    # so `hexput-bin` never reaches `hexput-config` (or anything else) directly.
    "hexput-bin": {"hexput-daemon", "hexput-cli-core", "hexput-lsp-core"},
}


def workspace_graph() -> dict[str, set[str]]:
    """Map each workspace crate to its workspace-internal *normal* dependencies.

    Dev-dependencies are excluded deliberately. Every rule here is about what production
    code can reach — "only the Executor may reach enforcement", "only the wiring root may
    reach a transport". A dev-dependency compiles into tests and nothing else, so it grants
    no such reach. This is what lets `hexput-tests` test any crate in the workspace without
    either weakening these rules or being exempted by name.

    Build-dependencies are still counted: a build script runs as part of producing the crate.
    """
    result = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked", "--no-deps"],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        # Most often a manifest changed without regenerating Cargo.lock. Say so plainly
        # instead of surfacing a Python traceback over cargo's own message.
        raise SystemExit(
            "could not read the crate graph: `cargo metadata --locked` failed.\n"
            "If you just edited a Cargo.toml, run `cargo generate-lockfile`.\n\n"
            f"{result.stderr.strip()}"
        )
    meta = json.loads(result.stdout)

    members = {pkg["name"] for pkg in meta["packages"]}
    return {
        pkg["name"]: {
            d["name"]
            for d in pkg["dependencies"]
            if d["name"] in members and d.get("kind") != "dev"
        }
        for pkg in meta["packages"]
    }


def main() -> int:
    graph = workspace_graph()
    failures: list[str] = []

    for crate, expected in EXACT_DEPENDENCIES.items():
        actual = graph.get(crate)
        if actual != expected:
            failures.append(
                f"Spine: `{crate}` must depend on exactly {sorted(expected)}; "
                f"found {sorted(actual) if actual is not None else 'missing crate'}"
            )

    for crate, forbidden, ad, why in FORBIDDEN_EDGES:
        if crate not in graph:
            failures.append(f"{ad}: crate `{crate}` is missing from the workspace")
        elif forbidden in graph[crate]:
            failures.append(
                f"{ad}: `{crate}` must not depend on `{forbidden}` — {why}"
            )

    for crate, required, ad, why in REQUIRED_EDGES:
        if crate not in graph:
            failures.append(f"{ad}: crate `{crate}` is missing from the workspace")
        elif required not in graph[crate]:
            failures.append(
                f"{ad}: `{crate}` must depend on `{required}` — {why}"
            )

    for crate, allowed, ad, why in SOLE_DEPENDENTS:
        actual = {c for c, deps in graph.items() if crate in deps}
        if actual != allowed:
            unexpected = actual - allowed
            missing = allowed - actual
            detail = []
            if unexpected:
                detail.append(f"unexpected dependents {sorted(unexpected)}")
            if missing:
                detail.append(f"expected dependents {sorted(missing)} absent")
            failures.append(
                f"{ad}: `{crate}` must be depended on by exactly {sorted(allowed)} — "
                f"{'; '.join(detail)} — {why}"
            )

    if failures:
        print("Crate graph violates the Architecture Spine:\n", file=sys.stderr)
        for f in failures:
            print(f"  - {f}", file=sys.stderr)
        print(
            "\nThese edges are architectural invariants, not style preferences.\n"
            "See the Spine's 'Crate dependency graph' section before changing one.",
            file=sys.stderr,
        )
        return 1

    checked = (len(FORBIDDEN_EDGES) + len(REQUIRED_EDGES)
               + len(SOLE_DEPENDENTS) + len(EXACT_DEPENDENCIES))
    print(f"Crate graph OK — {checked} Architecture Decision edges asserted.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
