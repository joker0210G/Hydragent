"""train_lora.py — LoRA fine-tune a causal LM on a Hydra chat-format dataset.

Reads a dataset.jsonl produced by `generate_dataset.py` and fine-tunes a
LoRA adapter on top of a base causal LM. Heavy deps (transformers / peft /
accelerate / torch) are imported lazily so the script can still print a
helpful message when the user has not installed them yet.

Usage:
    python train_lora.py --dataset dataset.jsonl \\
                         --base-model meta-llama/Llama-3.2-1B \\
                         --output adapter-out \\
                         --epochs 3 --lr 2e-4 \\
                         --batch-size 4 --lora-r 16
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Iterator

import typer


app = typer.Typer(add_completion=False, help=__doc__)


# --------------------------------------------------------------------------- #
# Friendly ImportError for heavy deps
# --------------------------------------------------------------------------- #

_REQUIRED = [
    ("datasets", "datasets>=2.14"),
    ("transformers", "transformers>=4.36"),
    ("peft", "peft>=0.7"),
    ("accelerate", "accelerate>=0.24"),
    ("torch", "torch>=2.1"),
]


def _require_heavy_deps() -> dict:
    """Import torch / transformers / peft / datasets / accelerate or print a
    friendly error and exit."""
    try:
        import torch  # noqa: F401
        import transformers  # noqa: F401
        import datasets  # noqa: F401
        import peft  # noqa: F401
        import accelerate  # noqa: F401
    except ImportError as exc:
        missing = exc.name or "unknown"
        pkgs = "\n    ".join(p for _, p in _REQUIRED)
        typer.echo(
            f"[error] missing dependency: {missing!r}.\n"
            f"install the fine-tuning dependencies first:\n"
            f"    pip install -r requirements.txt\n"
            f"or, individually:\n    {pkgs}",
            err=True,
        )
        raise typer.Exit(code=1) from exc

    import torch
    import transformers
    import datasets
    import peft
    import accelerate  # noqa: F401  (side-effect import for accelerator)

    return {
        "torch": torch,
        "transformers": transformers,
        "datasets": datasets,
        "peft": peft,
    }


# --------------------------------------------------------------------------- #
# Dataset
# --------------------------------------------------------------------------- #

def read_jsonl(path: Path) -> Iterator[dict]:
    with path.open("r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            yield json.loads(line)


def build_hf_dataset(deps: dict, jsonl_path: Path, tokenizer, max_length: int = 1024):
    """Convert our chat-format JSONL into a HuggingFace Dataset of
    `{"input_ids": ..., "labels": ...}` examples using the tokenizer's
    `apply_chat_template` if available, else a simple concat fallback."""
    from datasets import Dataset

    rows = []
    for sample in read_jsonl(jsonl_path):
        messages = sample.get("messages", [])
        if not messages:
            continue

        if hasattr(tokenizer, "apply_chat_template") and tokenizer.chat_template:
            text = tokenizer.apply_chat_template(
                messages, tokenize=False, add_generation_prompt=False
            )
        else:
            # simple fallback: concatenate with role tags
            text = "\n".join(f"{m['role']}: {m['content']}" for m in messages)

        enc = tokenizer(
            text,
            truncation=True,
            max_length=max_length,
            padding=False,
            return_tensors=None,
        )
        enc["labels"] = list(enc["input_ids"])
        rows.append(enc)

    return Dataset.from_list(rows)


# --------------------------------------------------------------------------- #
# Main
# --------------------------------------------------------------------------- #

def main(
    dataset: Path = typer.Option(..., "--dataset", help="Path to dataset.jsonl"),
    base_model: str = typer.Option(..., "--base-model", help="HF model id or local path"),
    output: Path = typer.Option(..., "--output", help="Directory to write the LoRA adapter"),
    epochs: int = typer.Option(3, "--epochs", min=1),
    lr: float = typer.Option(2e-4, "--lr"),
    batch_size: int = typer.Option(4, "--batch-size", min=1),
    lora_r: int = typer.Option(16, "--lora-r", min=1),
    lora_alpha: int = typer.Option(32, "--lora-alpha", min=1),
    lora_dropout: float = typer.Option(0.05, "--lora-dropout", ge=0.0, le=1.0),
    log_every: int = typer.Option(50, "--log-every", min=1, help="Log loss every N steps"),
    max_length: int = typer.Option(1024, "--max-length", min=16),
) -> None:
    deps = _require_heavy_deps()
    torch = deps["torch"]
    transformers = deps["transformers"]
    peft = deps["peft"]

    if not dataset.exists():
        raise typer.BadParameter(f"dataset not found: {dataset}")

    output.mkdir(parents=True, exist_ok=True)

    typer.echo(f"[info] loading tokenizer: {base_model}")
    tokenizer = transformers.AutoTokenizer.from_pretrained(base_model)
    if tokenizer.pad_token is None:
        tokenizer.pad_token = tokenizer.eos_token

    typer.echo(f"[info] loading model: {base_model}")
    model = transformers.AutoModelForCausalLM.from_pretrained(
        base_model,
        torch_dtype=torch.bfloat16 if torch.cuda.is_available() else torch.float32,
    )

    lora_config = peft.LoraConfig(
        r=lora_r,
        lora_alpha=lora_alpha,
        lora_dropout=lora_dropout,
        target_modules=["q_proj", "v_proj"],
        task_type="CAUSAL_LM",
    )
    model = peft.get_peft_model(model, lora_config)
    model.print_trainable_parameters()

    typer.echo(f"[info] building dataset from {dataset}")
    hf_ds = build_hf_dataset(deps, dataset, tokenizer, max_length=max_length)
    typer.echo(f"[info] {len(hf_ds)} training examples")

    def collate(batch):
        # left-pad to longest in batch
        import torch as _torch
        max_len = max(len(b["input_ids"]) for b in batch)
        pad_id = tokenizer.pad_token_id
        input_ids, labels, attn = [], [], []
        for b in batch:
            n = max_len - len(b["input_ids"])
            input_ids.append(b["input_ids"] + [pad_id] * n)
            labels.append(b["labels"] + [-100] * n)
            attn.append([1] * len(b["input_ids"]) + [0] * n)
        return {
            "input_ids": _torch.tensor(input_ids, dtype=_torch.long),
            "labels": _torch.tensor(labels, dtype=_torch.long),
            "attention_mask": _torch.tensor(attn, dtype=_torch.long),
        }

    args = transformers.TrainingArguments(
        output_dir=str(output),
        num_train_epochs=epochs,
        learning_rate=lr,
        per_device_train_batch_size=batch_size,
        logging_steps=log_every,
        save_strategy="no",
        report_to=[],
        bf16=torch.cuda.is_available(),
        fp16=False,
        remove_unused_columns=False,
    )

    trainer = transformers.Trainer(
        model=model,
        args=args,
        train_dataset=hf_ds,
        data_collator=collate,
    )

    typer.echo("[info] starting training")
    trainer.train()

    typer.echo(f"[info] saving LoRA adapter to {output}")
    model.save_pretrained(str(output))
    tokenizer.save_pretrained(str(output))
    typer.echo("[info] done")


if __name__ == "__main__":
    typer.run(main)
