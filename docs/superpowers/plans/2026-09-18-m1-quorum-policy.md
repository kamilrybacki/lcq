# M1 Quorum Policy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. If that skill is not installed, follow the same test-first steps and review gates directly. Steps use checkbox (`- [ ]`) syntax for tracking. Do not start later milestones automatically.

**Goal:** Create a pure, tested calculator of the two quorum thresholds for a fixed known fleet.

**Architecture:** `Policy` is immutable and contains approved integer member weights. `evaluate` accepts trusted signer IDs, counts each once, and reports count and weight outcomes separately. Later protocol layers must authenticate and scope messages BEFORE passing signers into this function; M1 itself is not a secure protocol.

**Tech Stack:** Proposed starting profile: Python 3.13+, uv, hatchling, pytest, hypothesis, ruff. No runtime third-party dependencies in M1.

## Global Constraints

- Work only in `lorai`. Do not modify `morsik-lora` or `Baltic_Hackaton_26`.
- No MQTT, HTTP server, Docker, LLM inference, cryptography or hardware in this milestone.
- Positive-only endorsement; no “all clear” output.
- Default Byzantine budget: 4000 basis points, i.e. 40% of members, rounded down to an integer number of members.
- Count threshold: `floor((N+floor(0.4*N))/2)+1` by default.
- Weight threshold: STRICTLY greater than 2/3, expressed with integer arithmetic.
- Max:min weight ratio ≤3:1 by default; positive integer weights only.
- Membership and weights are fixed during evaluation, regardless of reachability.
- Existing product defaults are approved; Python/toolchain and the exact API below are engineering proposals. Confirm M1 execution scope before creating code.

## Intended files and public interface

Create `pyproject.toml`, `.python-version`, `.gitignore`, `uv.lock` (generated), `src/lorai/__init__.py`, `src/lorai/policy.py`, `tests/test_policy.py`, and `tests/test_policy_properties.py`. Modify `README.md` and `docs/HANDOFF.md` only to record actual usage/results.

`Policy(weights: Mapping[str, int], byzantine_bps: int = 4000, max_weight_ratio: int = 3)` exposes read-only weights, `size`, `max_faulty`, `min_signers`, `total_weight`.

`evaluate(policy: Policy, signers: Iterable[str]) -> QuorumResult` produces immutable `signer_count`, `support_weight`, `count_met`, `weight_met`, and computed `approved`.

## Task 1 — Package and policy validation

**Files:** Create the package/configuration and `tests/test_policy.py`.

- [ ] Check `git status --short --branch`, `python3 --version`, and `uv --version`. Do not install global tools silently. If unavailable, report the missing prerequisite.
- [ ] Create the package metadata below, `.python-version` containing `3.13`, empty `src/lorai/__init__.py`, and `.gitignore` as shown. Do not create `policy.py` yet.

```toml
[project]
name = "lorai"
version = "0.1.0"
description = "Known-membership endorsement protocol for constrained radio networks"
requires-python = ">=3.13"
dependencies = []

[dependency-groups]
dev = ["pytest", "hypothesis", "ruff"]

[build-system]
requires = ["hatchling"]
build-backend = "hatchling.build"

[tool.hatch.build.targets.wheel]
packages = ["src/lorai"]

[tool.pytest.ini_options]
testpaths = ["tests"]

[tool.ruff]
target-version = "py313"
line-length = 100

[tool.ruff.lint]
select = ["E", "F", "I", "UP", "B"]
```

```gitignore
.venv/
__pycache__/
*.py[cod]
.pytest_cache/
.hypothesis/
.ruff_cache/
dist/
.env
.env.*
!.env.example
*.db
*.db-wal
*.db-shm
```

- [ ] Run `uv sync --group dev`; inspect and retain the generated `uv.lock`. There is no claim that untested latest dependency versions are compatible: the subsequent checks establish the selected lockfile's compatibility.
- [ ] Write these tests in `tests/test_policy.py`:

```python
import pytest

from lorai.policy import Policy


@pytest.mark.parametrize("n,f,q", [(5, 2, 4), (10, 4, 8), (20, 8, 15), (100, 40, 71)])
def test_default_thresholds(n, f, q):
    policy = Policy({str(i): 1 for i in range(n)})
    assert (policy.size, policy.max_faulty, policy.min_signers) == (n, f, q)
    assert policy.total_weight == n


@pytest.mark.parametrize("weights", [{}, {"a": 0}, {"a": -1}, {"a": True},
                                      {"a": 1.0}, {"": 1}, {"a": 4, "b": 1}])
def test_invalid_weights_rejected(weights):
    with pytest.raises(ValueError):
        Policy(weights)


@pytest.mark.parametrize("budget", [-1, 10000, True, 4000.0])
def test_invalid_fault_budget_rejected(budget):
    with pytest.raises(ValueError):
        Policy({"a": 1}, byzantine_bps=budget)


@pytest.mark.parametrize("ratio", [0, -1, True, 3.0])
def test_invalid_ratio_rejected(ratio):
    with pytest.raises(ValueError):
        Policy({"a": 1}, max_weight_ratio=ratio)


def test_weights_are_copied_and_read_only():
    original = {"a": 1, "b": 2}
    policy = Policy(original)
    original["a"] = 99
    assert policy.weights["a"] == 1
    with pytest.raises(TypeError):
        policy.weights["a"] = 99
```

- [ ] Run `uv run pytest tests/test_policy.py -q`. Expected red: missing `lorai.policy`, not an unrelated dependency error.
- [ ] Create `src/lorai/policy.py` with the following minimal implementation:

```python
from collections.abc import Mapping
from dataclasses import dataclass
from types import MappingProxyType


@dataclass(frozen=True)
class Policy:
    weights: Mapping[str, int]
    byzantine_bps: int = 4000
    max_weight_ratio: int = 3

    def __post_init__(self) -> None:
        weights = dict(self.weights)
        if not weights:
            raise ValueError("fleet must not be empty")
        if any(not isinstance(k, str) or not k for k in weights):
            raise ValueError("member IDs must be nonempty strings")
        if any(type(v) is not int or v <= 0 for v in weights.values()):
            raise ValueError("weights must be positive integers")
        if type(self.byzantine_bps) is not int or not 0 <= self.byzantine_bps < 10000:
            raise ValueError("fault budget must be integer basis points in [0, 10000)")
        if type(self.max_weight_ratio) is not int or self.max_weight_ratio < 1:
            raise ValueError("weight ratio must be a positive integer")
        if max(weights.values()) > self.max_weight_ratio * min(weights.values()):
            raise ValueError("weight ratio exceeds policy cap")
        object.__setattr__(self, "weights", MappingProxyType(weights))

    @property
    def size(self) -> int:
        return len(self.weights)

    @property
    def max_faulty(self) -> int:
        return self.byzantine_bps * self.size // 10000

    @property
    def min_signers(self) -> int:
        return (self.size + self.max_faulty) // 2 + 1

    @property
    def total_weight(self) -> int:
        return sum(self.weights.values())
```

- [ ] Run `uv run pytest tests/test_policy.py -q`, `uv run ruff check .`, `uv run ruff format --check .`. Apply formatting if needed and rerun. Expected: tests and checks pass.
- [ ] Commit only these milestone files and the generated lockfile with message `feat: add immutable fleet quorum policy` after verifying the diff contains no unrelated changes.

## Task 2 — Count and weight decision, separately visible

**Files:** Modify `src/lorai/policy.py`, `tests/test_policy.py`.

**Consumes:** `Policy` from Task 1. **Produces:** `QuorumResult` and `evaluate` defined above.

- [ ] Add `evaluate` to the import in `tests/test_policy.py` and append:

```python
def test_equal_weight_five_needs_four():
    p = Policy({str(i): 1 for i in range(5)})
    assert not evaluate(p, ["0", "1", "2"]).approved
    assert evaluate(p, ["0", "1", "2", "3"]).approved


def test_exactly_two_thirds_is_not_enough():
    p = Policy({"a": 1, "b": 1, "c": 1}, byzantine_bps=0)
    result = evaluate(p, ["a", "b"])
    assert result.count_met
    assert not result.weight_met
    assert not result.approved


def test_heavy_member_can_block_despite_count():
    p = Policy({"a": 3, "b": 1, "c": 1, "d": 1, "e": 1})
    result = evaluate(p, ["b", "c", "d", "e"])
    assert (result.signer_count, result.support_weight) == (4, 4)
    assert result.count_met and not result.weight_met
    assert not result.approved


def test_weight_cannot_replace_required_member_count():
    p = Policy({"a": 3, "b": 3, "c": 3, "d": 1, "e": 1})
    result = evaluate(p, ["a", "b", "c"])
    assert result.weight_met and not result.count_met
    assert not result.approved


def test_duplicate_signers_do_not_increase_support():
    p = Policy({str(i): 1 for i in range(5)})
    assert evaluate(p, ["0"] * 100) == evaluate(p, ["0"])


def test_unknown_member_is_rejected_not_added_to_denominator():
    p = Policy({"a": 1, "b": 1})
    with pytest.raises(ValueError):
        evaluate(p, ["a", "intruder"])
    assert p.total_weight == 2


def test_empty_support_is_not_approved():
    result = evaluate(Policy({"a": 1}), [])
    assert not result.approved
    assert result.signer_count == result.support_weight == 0
```

- [ ] Run `uv run pytest tests/test_policy.py -q`; verify failure is the missing `evaluate` API.
- [ ] Add `Iterable` to the collections import and append this implementation to `policy.py`:

```python
@dataclass(frozen=True)
class QuorumResult:
    signer_count: int
    support_weight: int
    count_met: bool
    weight_met: bool

    @property
    def approved(self) -> bool:
        return self.count_met and self.weight_met


def evaluate(policy: Policy, signers: Iterable[str]) -> QuorumResult:
    unique = frozenset(signers)
    if not unique.issubset(policy.weights):
        raise ValueError("unknown signer")
    weight = sum(policy.weights[s] for s in unique)
    return QuorumResult(
        signer_count=len(unique),
        support_weight=weight,
        count_met=len(unique) >= policy.min_signers,
        weight_met=3 * weight > 2 * policy.total_weight,
    )
```

- [ ] Run `uv run pytest tests/test_policy.py -q` and both ruff checks. Expected: all tests pass. In review explicitly state that `evaluate` cannot verify message identity, scope or cryptography.
- [ ] Commit scoped files with message `feat: evaluate count and weight endorsement thresholds`.

## Task 3 — Property tests, intersection sanity and handoff

**Files:** Create `tests/test_policy_properties.py`; update README and handoff with measured results.

- [ ] Add the following tests. They are an independent oracle/check of arithmetic properties; unlike Tasks 1–2 they may already pass. Do not invent a red result.

```python
from itertools import combinations

from hypothesis import given, strategies as st

from lorai.policy import Policy, evaluate


def test_all_small_count_quorums_intersect_in_more_than_fault_budget():
    for n in range(1, 9):
        policy = Policy({str(i): 1 for i in range(n)})
        quorums = [set(c) for c in combinations(policy.weights, policy.min_signers)]
        for a in quorums:
            for b in quorums:
                assert len(a & b) > policy.max_faulty


@given(st.lists(st.integers(min_value=1, max_value=3), min_size=1, max_size=30),
       st.integers(min_value=0, max_value=100))
def test_integer_oracle_and_duplicate_invariance(weights, prefix):
    policy = Policy({str(i): w for i, w in enumerate(weights)})
    signers = list(policy.weights)[:prefix]
    support = sum(policy.weights[s] for s in signers)
    result = evaluate(policy, signers)
    expected_count = len(signers) >= (len(weights) + (2 * len(weights)) // 5) // 2 + 1
    assert result.approved == (expected_count and 3 * support > 2 * sum(weights))
    assert result == evaluate(policy, list(reversed(signers)) + signers)


@given(st.integers(min_value=1, max_value=100), st.integers(min_value=0, max_value=9999))
def test_threshold_is_feasible_and_intersection_bound_holds(n, bps):
    policy = Policy({str(i): 1 for i in range(n)}, byzantine_bps=bps)
    assert 1 <= policy.min_signers <= n
    assert 2 * policy.min_signers - n > policy.max_faulty
    assert evaluate(policy, policy.weights).approved
```

- [ ] Run `uv run pytest -q`, `uv run ruff check .`, `uv run ruff format --check .`, and `uv build`. Expected: tests/checks pass and a wheel/sdist are produced. Build artifacts must remain ignored.
- [ ] Review code manually: no float threshold, no mutation of weights, no reachable-member denominator, no binding of policy to local arrival order. Check that a one-member manifest is merely mathematically permitted, not advertised as fault-tolerant.
- [ ] Add this usage example to README with an explicit “trusted signer IDs only; no authentication in M1” warning:

```python
from lorai.policy import Policy, evaluate

policy = Policy({"a": 1, "b": 1, "c": 1, "d": 1, "e": 1})
result = evaluate(policy, ["a", "b", "c", "d"])
print(result.approved)  # True
```

- [ ] Update `docs/HANDOFF.md`: M1 implemented / awaiting user review; exact test count and commands; selected locked dependencies; commit IDs and next gate M2. Keep crypto and radio explicitly unimplemented.
- [ ] Commit the tests and documentation with message `test: verify quorum invariants and document M1`.
- [ ] Stop for review. Do not begin M2 or claim formal Byzantine safety from arithmetic tests.

## M1 acceptance summary

The milestone is complete only when installation, policy tests, property tests, lint/format and build have actually succeeded, source/API scope has been reviewed, and the handoff reflects reality. Manual approval belongs to the user. This milestone does not compute model-quality coefficients, authenticate manifests, parse packets, implement consultations or simulate LoRa.
