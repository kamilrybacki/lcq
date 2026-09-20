# Instructions for coding agents

## Start here

This repository is `lcq`, the reusable protocol project, NOT the Morsik application.
Read in order:

1. `docs/HANDOFF.md` — current state and next action.
2. `docs/superpowers/specs/2026-09-18-lcq-design.md` — approved product decisions and proposed technical refinements.
3. `docs/IMPLEMENTATION-ROADMAP.md` — milestone boundaries and acceptance gates.
4. The implementation plan named in `docs/HANDOFF.md`.

The design discussion was in Polish; code and identifiers should be English. No access to the original chat is required. If these documents conflict, explain the conflict and ask before changing security semantics.

## Scope

- `lcq` owns protocol, signatures/encryption, quorum, durable protocol state, radio queues/forwarding, transport/clock interfaces and simulation.
- `morsik-lora` is a separate application integration repository. It owns local MQTT/HTTP wiring and mapping Morsik data onto protocol contracts.
- All actual model inference belongs to `morsik-analysis`. `morsik-dashboard` is presentation only.
- Do not modify sibling repositories, rename Morsik modules, install a broker, deploy containers, call live models, or access operational maritime sources unless that milestone is explicitly authorized.
- Use synthetic/public approved fixtures. Never commit secrets, production keys, operational captures or credentials. Fixed test keys, if needed later, must be conspicuously labeled insecure fixtures.

## Work discipline

- Inspect git status and preserve user changes. A dirty worktree is not permission to reset it.
- Execute one milestone at a time with failing tests first, minimal implementation, focused tests, then full available checks.
- Do not silently convert proposed engineering choices into user-approved requirements. Confirm the technical profile before crypto/wire-format implementation.
- Do not use packet loss alone as a claim of LoRa fidelity. Record the limitations of PHY/collision models.
- Do not call passing tests a formal security proof, certify model truth, or claim progress under 40% Byzantine participation.
- No automatic fleet membership changes, threshold reduction on disconnection, self-assigned competence (D24: a member's standing comes from the manifest, never from the member), or recovery by resetting nonce counters under an existing key.
- A positive-only endorsement protocol is not a blockchain and is not a complete generic BFT consensus algorithm.
- A local broker is not the durable safety journal. A group encryption secret is not sender authentication.
- Before commits, run checks and record exact commands/results. Never fabricate PASS results or operator approval.
- Update `docs/HANDOFF.md` at each milestone with verified status, changed files, commands, failures and the next bounded action. Keep assumptions and limitations visible.
- Local commits may group one completed milestone. Do not push, open PRs or deploy unless requested.

## Environment and tools

There is no established build/test environment at handoff. Proposed starting profile is Python 3.13+, `uv`, `pytest`, `hypothesis`, and `ruff`; it becomes real only in M1. Commands in plans are future instructions, not evidence they have run. Do not install unrelated global tools.

Respect the operator's tool policy. The original environment preferred Cellarette MCP and jCodemunch; if tools are missing, ask before bypassing a policy. In a different Claude Code environment use available equivalents without assuming the original tools exist.
