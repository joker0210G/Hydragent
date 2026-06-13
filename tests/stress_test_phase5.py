#!/usr/bin/env python3
"""
Hydragent Phase 5 — Infrastructure Stress Test
==============================================

This is NOT a runtime stress test in the same shape as
``stress_test_phase1.py`` / ``phase2.py`` / ``phase3.py``. Those talk
to a *running* ``hydragent.exe`` over the JSON-RPC bus. Phase 5's
new surfaces (DAG engine, self-healing replanner, wiki, ASCII
printer, status CLI) are all pure Rust modules that don't need a
live LLM or a live bus to exercise. So this script is an
**infrastructure smoke test** instead:

  S1  cargo build -p hydragent-planner        (clean compile)
  S2  cargo build -p hydragent-swarm          (clean compile)
  S3  cargo test  -p hydragent-planner        (all 61 tests pass)
  S4  cargo test  -p hydragent-swarm          (all 77 tests pass)
  S5  swarm_status --help                     (binary alive, prints usage)
  S6  swarm_status --from-spec <sample>       (renders ASCII for a real spec)
  S7  swarm_status --from-report <sample>     (renders ASCII for a real report)
  S8  swarm_status --one-line --from-report    (one-line log-friendly mode)
  S9  swarm_status --stdin-spec               (stdin path works)
  S10 swarm_status --no-header --from-spec    (--no-header strips the header)

The Rust side has its own doctest + lib + integration coverage, so
this script's job is to catch the two things that ``cargo test``
alone won't:

  - Binary not present (release process forgot to build)
  - CLI flags regressed (flag renamed, conflict rule wrong, etc.)

It does **not** verify end-to-end agent behaviour. For that, run
``stress_test_phase3.py`` against a live bus.

Usage
-----

    # Pre-requisites (one-time, in a persistent terminal):
    #   set "CARGO_BUILD_JOBS=1"     (Windows)
    #   cargo build -p hydragent-planner -p hydragent-swarm
    #
    # Then, in any terminal:
    #   python tests/stress_test_phase5.py
    #
    # Optional flags:
    #   --skip-build    Don't re-run cargo build (use cached binaries)
    #   --keep-artifacts Keep the sample spec/report JSONs in ./scratch/

Exit code: 0 on full success, 1 on any failure.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import List, Optional

REPO = Path(__file__).resolve().parent.parent
SCRATCH = REPO / "scratch"
TARGET_DEBUG = REPO / "target" / "debug"
SWARM_STATUS = TARGET_DEBUG / "swarm_status.exe"

# We always run cargo with a single job on Windows to keep RAM usage down.
CARGO_ENV = {**os.environ, "CARGO_BUILD_JOBS": "1"}


# ---------------------------------------------------------------------------
# Pretty printers
# ---------------------------------------------------------------------------


class C:
    """Tiny ANSI color helper. Disabled if stdout isn't a TTY."""

    def __init__(self) -> None:
        self.on = sys.stdout.isatty()

    def wrap(self, code: str, text: str) -> str:
        if not self.on:
            return text
        return f"\033[{code}m{text}\033[0m"

    def green(self, t: str) -> str:
        return self.wrap("32", t)

    def red(self, t: str) -> str:
        return self.wrap("31", t)

    def yellow(self, t: str) -> str:
        return self.wrap("33", t)

    def dim(self, t: str) -> str:
        return self.wrap("2", t)

    def bold(self, t: str) -> str:
        return self.wrap("1", t)


C = C()


def header(title: str) -> None:
    print()
    print(C.bold(f"== {title} =="))
    print()


# ---------------------------------------------------------------------------
# Runner
# ---------------------------------------------------------------------------


@dataclass
class Step:
    """One assertion in the test plan."""

    name: str
    cmd: list
    cwd: Optional[Path] = None
    env: Optional[dict] = None
    timeout: float = 300.0
    expect_exit: int = 0
    expect_stdout_contains: List[str] = field(default_factory=list)
    expect_stdout_excludes: List[str] = field(default_factory=list)
    stdin_payload: Optional[str] = None
    skip: bool = False
    skip_reason: str = ""


@dataclass
class Result:
    name: str
    ok: bool
    duration_s: float
    stdout_tail: str = ""
    stderr_tail: str = ""
    skip_reason: str = ""


def run_step(step: Step) -> Result:
    """Run one shell command and assert on its output."""
    if step.skip:
        print(C.yellow(f"  - {step.name}: SKIP ({step.skip_reason})"))
        return Result(name=step.name, ok=True, duration_s=0.0, skip_reason=step.skip_reason)

    print(C.dim(f"  $ {' '.join(step.cmd)}"))
    t0 = time.monotonic()
    try:
        proc = subprocess.run(
            step.cmd,
            cwd=str(step.cwd) if step.cwd else str(REPO),
            env=step.env if step.env is not None else CARGO_ENV,
            capture_output=True,
            text=True,
            timeout=step.timeout,
            input=step.stdin_payload,
            shell=False,
        )
    except subprocess.TimeoutExpired as e:
        dt = time.monotonic() - t0
        print(C.red(f"  x {step.name}: TIMEOUT after {dt:.1f}s"))
        return Result(
            name=step.name, ok=False, duration_s=dt,
            stderr_tail=(e.stderr or "")[-2000:],
        )
    except FileNotFoundError as e:
        dt = time.monotonic() - t0
        print(C.red(f"  x {step.name}: command not found: {e}"))
        return Result(name=step.name, ok=False, duration_s=dt)

    dt = time.monotonic() - t0
    ok = proc.returncode == step.expect_exit

    # Check stdout expectations.
    for needle in step.expect_stdout_contains:
        if needle not in proc.stdout:
            ok = False
            print(C.red(f"  x {step.name}: expected '{needle}' in stdout, not found"))
    for needle in step.expect_stdout_excludes:
        if needle in proc.stdout:
            ok = False
            print(C.red(f"  x {step.name}: did NOT expect '{needle}' in stdout, but found it"))

    if ok:
        print(C.green(f"  + {step.name}: ok ({dt:.1f}s)"))
    else:
        print(C.red(f"  x {step.name}: FAIL (exit={proc.returncode}, {dt:.1f}s)"))
        print(C.dim("    --- stdout tail ---"))
        print(proc.stdout[-2000:])
        print(C.dim("    --- stderr tail ---"))
        print(proc.stderr[-2000:])

    return Result(
        name=step.name,
        ok=ok,
        duration_s=dt,
        stdout_tail=proc.stdout[-2000:],
        stderr_tail=proc.stderr[-2000:],
    )


# ---------------------------------------------------------------------------
# Sample artifacts
# ---------------------------------------------------------------------------


SAMPLE_DAG_SPEC = {
    "swarm_id": "stress-test-phase5",
    "page_id": "stress-page-1",
    "original_task": "Render a sample DAG to ASCII for the stress test.",
    "nodes": [
        {
            "id": "A",
            "name": "root",
            "description": "Plan the work",
            "task_type": "planning",
            "allowed_tools": [],
            "model_hint": None,
            "token_budget": 1000,
            "timeout_ms": 10000,
            "retry_count": 0,
            "max_retries": 2,
            "status": "completed",
            "result": None,
        },
        {
            "id": "B",
            "name": "left branch",
            "description": "Do research",
            "task_type": "research",
            "allowed_tools": ["web_search"],
            "model_hint": None,
            "token_budget": 1000,
            "timeout_ms": 10000,
            "retry_count": 0,
            "max_retries": 2,
            "status": "completed",
            "result": None,
        },
        {
            "id": "C",
            "name": "right branch",
            "description": "Do analysis",
            "task_type": "reasoning",
            "allowed_tools": [],
            "model_hint": None,
            "token_budget": 1000,
            "timeout_ms": 10000,
            "retry_count": 0,
            "max_retries": 2,
            "status": "failed",
            "result": None,
        },
        {
            "id": "D",
            "name": "join",
            "description": "Combine and summarise",
            "task_type": "summarization",
            "allowed_tools": [],
            "model_hint": None,
            "token_budget": 1000,
            "timeout_ms": 10000,
            "retry_count": 0,
            "max_retries": 2,
            "status": "skipped",
            "result": None,
        },
    ],
    "edges": [
        {"from": "A", "to": "B", "label": None},
        {"from": "A", "to": "C", "label": None},
        {"from": "B", "to": "D", "label": None},
        {"from": "C", "to": "D", "label": None},
    ],
    "created_at": 0,
}

SAMPLE_REPORT = {
    "swarm_id": "stress-test-phase5",
    "page_id": "stress-page-1",
    "original_task": "Render a sample report to ASCII for the stress test.",
    "started_at_ms": 0,
    "finished_at_ms": 1234,
    "total_execution_ms": 1234,
    "completed": 2,
    "failed": 1,
    "cancelled": 0,
    "skipped": 1,
    "node_results": {},
    "final_spec": SAMPLE_DAG_SPEC,
}


def write_sample_artifacts(keep: bool) -> tuple[Path, Path]:
    """Write the sample spec and report JSONs to scratch/."""
    SCRATCH.mkdir(exist_ok=True)
    spec_path = SCRATCH / "phase5_sample_dag.json"
    report_path = SCRATCH / "phase5_sample_report.json"
    spec_path.write_text(json.dumps(SAMPLE_DAG_SPEC, indent=2))
    report_path.write_text(json.dumps(SAMPLE_REPORT, indent=2))
    if not keep:
        # Schedule cleanup at exit by registering a try/finally in main.
        pass
    return spec_path, report_path


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument("--skip-build", action="store_true",
                        help="Don't re-run cargo build (use cached binaries)")
    parser.add_argument("--keep-artifacts", action="store_true",
                        help="Keep the sample spec/report JSONs in ./scratch/")
    parser.add_argument("--no-color", action="store_true",
                        help="Disable ANSI colors in output")
    args = parser.parse_args(argv)

    if args.no_color:
        C.on = False

    # -----------------------------------------------------------------------
    # Pre-flight
    # -----------------------------------------------------------------------
    header("Pre-flight")
    if not shutil.which("cargo"):
        print(C.red("cargo not on PATH. Install Rust or activate the toolchain."))
        return 1
    if not args.skip_build and not SWARM_STATUS.exists():
        print(C.yellow(f"{SWARM_STATUS.name} not built yet; the build step will produce it."))

    # -----------------------------------------------------------------------
    # Sample artifacts
    # -----------------------------------------------------------------------
    header("Sample artifacts")
    spec_path, report_path = write_sample_artifacts(args.keep_artifacts)
    print(C.dim(f"  sample spec   : {spec_path}"))
    print(C.dim(f"  sample report : {report_path}"))

    # -----------------------------------------------------------------------
    # Plan
    # -----------------------------------------------------------------------
    steps: List[Step] = []

    # Build steps (optional, slow on Windows)
    if not args.skip_build:
        steps += [
            Step(
                name="S1 cargo build hydragent-planner",
                cmd=["cargo", "build", "-p", "hydragent-planner"],
                timeout=600.0,
            ),
            Step(
                name="S2 cargo build hydragent-swarm",
                cmd=["cargo", "build", "-p", "hydragent-swarm"],
                timeout=600.0,
            ),
        ]
    else:
        print(C.dim("  - skipping build (--skip-build)"))

    # Test steps
    steps += [
        Step(
            name="S3 cargo test hydragent-planner",
            cmd=["cargo", "test", "-p", "hydragent-planner"],
            timeout=600.0,
            expect_stdout_contains=["test result: ok"],
        ),
        Step(
            name="S4 cargo test hydragent-swarm",
            cmd=["cargo", "test", "-p", "hydragent-swarm"],
            timeout=600.0,
            expect_stdout_contains=["test result: ok"],
        ),
    ]

    # CLI smoke tests (require the binary)
    if SWARM_STATUS.exists() or not args.skip_build:
        steps += [
            Step(
                name="S5 swarm_status --help",
                cmd=[str(SWARM_STATUS), "--help"],
                timeout=10.0,
                expect_stdout_contains=["--from-spec", "--from-report"],
            ),
            Step(
                name="S6 swarm_status --from-spec <sample>",
                cmd=[str(SWARM_STATUS), "--from-spec", str(spec_path)],
                timeout=10.0,
                expect_stdout_contains=["root", "left branch", "right branch"],
            ),
            Step(
                name="S7 swarm_status --from-report <sample>",
                cmd=[str(SWARM_STATUS), "--from-report", str(report_path)],
                timeout=10.0,
                expect_stdout_contains=["totals", "wall"],
            ),
            Step(
                name="S8 swarm_status --one-line --from-report <sample>",
                cmd=[str(SWARM_STATUS), "--one-line", "--from-report", str(report_path)],
                timeout=10.0,
                expect_stdout_contains=["status=FAIL", "completed=2", "failed=1"],
            ),
            Step(
                name="S9 swarm_status --stdin-spec",
                cmd=[str(SWARM_STATUS), "--stdin-spec"],
                timeout=10.0,
                stdin_payload=spec_path.read_text(),
                expect_stdout_contains=["root", "left branch"],
            ),
            Step(
                name="S10 swarm_status --no-header --from-spec <sample>",
                cmd=[str(SWARM_STATUS), "--no-header", "--from-spec", str(spec_path)],
                timeout=10.0,
                expect_stdout_excludes=["swarm_id :"],
                expect_stdout_contains=["left branch"],
            ),
        ]
    else:
        print(C.yellow(f"  - {SWARM_STATUS.name} missing and --skip-build; CLI steps will be skipped"))

    # -----------------------------------------------------------------------
    # Run
    # -----------------------------------------------------------------------
    header("Running")
    results: List[Result] = []
    for step in steps:
        results.append(run_step(step))

    # -----------------------------------------------------------------------
    # Summary
    # -----------------------------------------------------------------------
    header("Summary")
    passed = sum(1 for r in results if r.ok)
    failed = sum(1 for r in results if not r.ok and not r.skip_reason)
    skipped = sum(1 for r in results if r.skip_reason)
    total = len(results)
    total_time = sum(r.duration_s for r in results)

    print(f"  steps : {total}  passed={passed}  failed={failed}  skipped={skipped}")
    print(f"  time  : {total_time:.1f}s")
    print()

    if failed:
        print(C.red("FAIL — the following steps did not pass:"))
        for r in results:
            if not r.ok and not r.skip_reason:
                print(C.red(f"  - {r.name}"))
        print()
        return 1

    print(C.green("PASS — all infrastructure checks succeeded."))
    return 0


if __name__ == "__main__":
    try:
        rc = main()
    finally:
        # Best-effort cleanup of the sample artifacts unless the user
        # asked to keep them. The script writes to scratch/ which is
        # already in .gitignore.
        if "--keep-artifacts" not in sys.argv:
            for p in (SCRATCH / "phase5_sample_dag.json",
                      SCRATCH / "phase5_sample_report.json"):
                try:
                    if p.exists():
                        p.unlink()
                except OSError:
                    pass
    sys.exit(rc)
