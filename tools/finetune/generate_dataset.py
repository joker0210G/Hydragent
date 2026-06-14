"""generate_dataset.py — turn agent trajectories into supervised fine-tuning data.

Reads a JSONL of agent trajectories and emits a JSONL of chat-format
training samples. Each output row has:

    {
      "messages":     [{"role": "user", "content": "..."},
                       {"role": "assistant", "content": "..."}],
      "tools":        [...tool names from the source trajectory...],
      "skills_used":  [...]   # heuristic: placeholders found in assistant turns
    }

Only `json`, `random`, `pathlib`, `typer`, `tqdm`, and `collections.Counter`
are used so the script runs in any plain Python 3.10+ environment.

Usage:
    python generate_dataset.py --input trajectories.jsonl \\
                               --output dataset.jsonl \\
                               --n-samples 1000
"""

from __future__ import annotations

import json
import random
import re
from collections import Counter
from pathlib import Path
from typing import Iterable, Iterator

import typer
from tqdm import tqdm


app = typer.Typer(add_completion=False, help=__doc__)


# --------------------------------------------------------------------------- #
# I/O helpers
# --------------------------------------------------------------------------- #

def read_jsonl(path: Path) -> Iterator[dict]:
    """Yield one parsed JSON object per non-empty line in `path`."""
    with path.open("r", encoding="utf-8") as f:
        for lineno, line in enumerate(f, start=1):
            line = line.strip()
            if not line:
                continue
            try:
                yield json.loads(line)
            except json.JSONDecodeError as exc:
                raise typer.BadParameter(
                    f"invalid JSON on line {lineno} of {path}: {exc}"
                ) from exc


def write_jsonl(path: Path, rows: Iterable[dict]) -> int:
    """Write each row as one JSON object per line; return the count written."""
    path.parent.mkdir(parents=True, exist_ok=True)
    n = 0
    with path.open("w", encoding="utf-8") as f:
        for row in rows:
            f.write(json.dumps(row, ensure_ascii=False) + "\n")
            n += 1
    return n


# --------------------------------------------------------------------------- #
# Trajectory -> training sample
# --------------------------------------------------------------------------- #

# Match `{{skill:NAME}}`, `{{tool:NAME}}`, or generic `{{param}}` placeholders.
_PLACEHOLDER_RE = re.compile(r"\{\{\s*(?:[a-zA-Z_]+:)?([a-zA-Z0-9_\-]+)\s*\}\}")


def extract_skills(assistant_text: str) -> list[str]:
    """Return the sorted list of placeholder names found in `assistant_text`."""
    if not assistant_text:
        return []
    return sorted(set(_PLACEHOLDER_RE.findall(assistant_text)))


def trajectory_to_sample(traj: dict) -> dict:
    """Convert one trajectory dict into one chat-format training sample.

    Only `user` and `assistant` turns are kept. `tool` and `system` turns
    are dropped to keep the conversation clean for chat fine-tuning.
    """
    messages: list[dict] = []
    for turn in traj.get("turns", []) or []:
        role = turn.get("role", "")
        content = turn.get("content", "")
        if role in ("user", "assistant") and content:
            messages.append({"role": role, "content": content})

    # Heuristic: collect placeholder names from assistant content.
    skills: list[str] = []
    for m in messages:
        if m["role"] == "assistant":
            skills.extend(extract_skills(m["content"]))

    if skills:
        skills.append("use-skill")
        # dedupe, preserve order
        seen: set[str] = set()
        deduped: list[str] = []
        for s in skills:
            if s not in seen:
                seen.add(s)
                deduped.append(s)
        skills = deduped

    return {
        "messages": messages,
        "tools": list(traj.get("tools_used", []) or []),
        "skills_used": skills,
    }


# --------------------------------------------------------------------------- #
# Main
# --------------------------------------------------------------------------- #

def main(
    input: Path = typer.Option(..., "--input", help="Path to trajectories.jsonl"),
    output: Path = typer.Option(..., "--output", help="Path to write dataset.jsonl"),
    n_samples: int = typer.Option(1000, "--n-samples", min=1, help="Number of samples to emit"),
    seed: int = typer.Option(42, "--seed", help="Random seed for sampling"),
) -> None:
    """Sample N trajectories and write a chat-format training dataset."""
    if not input.exists():
        raise typer.BadParameter(f"input file not found: {input}")

    rng = random.Random(seed)
    all_trajectories = list(read_jsonl(input))
    if not all_trajectories:
        raise typer.BadParameter(f"no trajectories found in {input}")

    if n_samples > len(all_trajectories):
        typer.echo(
            f"[warn] --n-samples={n_samples} > available={len(all_trajectories)}; "
            f"sampling with replacement",
            err=True,
        )
        picked = [rng.choice(all_trajectories) for _ in range(n_samples)]
    else:
        picked = rng.sample(all_trajectories, n_samples)

    skill_counter: Counter[str] = Counter()
    tool_counter: Counter[str] = Counter()
    skipped = 0

    def gen() -> Iterator[dict]:
        nonlocal skipped
        for traj in tqdm(picked, desc="formatting", unit="traj"):
            sample = trajectory_to_sample(traj)
            if not sample["messages"]:
                skipped += 1
                continue
            for s in sample["skills_used"]:
                skill_counter[s] += 1
            for t in sample["tools"]:
                tool_counter[t] += 1
            yield sample

    written = write_jsonl(output, gen())

    # --- summary table ---
    typer.echo("")
    typer.echo(f"wrote {written} samples to {output} (skipped {skipped} empty)")
    typer.echo("")
    typer.echo("skills_used (top 10):")
    for name, count in skill_counter.most_common(10):
        typer.echo(f"  {name:<32s} {count}")
    typer.echo("")
    typer.echo("tools (top 10):")
    for name, count in tool_counter.most_common(10):
        typer.echo(f"  {name:<32s} {count}")


if __name__ == "__main__":
    typer.run(main)
