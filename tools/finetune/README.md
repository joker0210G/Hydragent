# hydra-finetune

A small LoRA fine-tuning pipeline for the Hydra agent. It contains two CLI
scripts that turn raw agent trajectories into supervised training data and
then run a low-rank adapter fine-tune on a base causal LM. The pipeline is
designed to be cheap to run on a single GPU and easy to inspect during
development.

## Quick start

```bash
# 1. Generate the supervised dataset from a trajectories.jsonl file
python generate_dataset.py \
    --input  ./trajectories.jsonl \
    --output ./dataset.jsonl \
    --n-samples 1000

# 2. Fine-tune a LoRA adapter on the dataset
python train_lora.py \
    --dataset    ./dataset.jsonl \
    --base-model meta-llama/Llama-3.2-1B \
    --output     ./adapter-out \
    --epochs     3 \
    --lr         2e-4 \
    --batch-size 4 \
    --lora-r     16
```

Both scripts are pure Python, run from any working directory, and emit
structured logs to stdout.

## Input format (`trajectories.jsonl`)

Each line of the input file is a JSON object representing one agent
trajectory. The schema is:

```json
{
  "session_id": "sess-2026-06-14-001",
  "tools_used": ["web_search", "code_exec"],
  "turns": [
    {"role": "user",      "content": "Convert this CSV to JSON"},
    {"role": "assistant", "content": "Sure. I will use {{skill:convert-csv-to-json}} ..."},
    {"role": "tool",      "content": "{ ...tool output... }"},
    {"role": "assistant", "content": "Here is the JSON result ..."}
  ]
}
```

* `session_id` (str): unique session identifier.
* `tools_used` (list[str]): tool names invoked during the trajectory.
* `turns` (list[dict]): ordered chat-style turns. `role` is one of
  `user`, `assistant`, `tool`, or `system`. `content` is plain text.

## Output format (`dataset.jsonl`)

Each line is a JSON object with a `messages` field (chat-format list) plus
metadata about the tools and skills observed in the source trajectory:

```json
{
  "messages": [
    {"role": "user",      "content": "Convert this CSV to JSON"},
    {"role": "assistant", "content": "Sure. I will use {{skill:convert-csv-to-json}} ..."}
  ],
  "tools": ["web_search"],
  "skills_used": ["convert-csv-to-json", "use-skill"]
}
```

* `messages` is filtered to user/assistant turns only (system/tool turns are
  folded into the assistant context or dropped).
* `tools` mirrors `tools_used` from the source trajectory.
* `skills_used` is a heuristic list. Any skill name found in assistant
  content (matched as `{{skill:NAME}}` or `{{param}}`) is recorded, and the
  meta-tag `"use-skill"` is appended whenever at least one placeholder was
  found.

## Hardware requirements

| Step                | Min hardware                  | Notes                                  |
|---------------------|-------------------------------|----------------------------------------|
| `generate_dataset`  | CPU only                      | Pure stdlib + typer + tqdm.            |
| `train_lora`        | CUDA GPU, ≥16 GB VRAM         | Single-GPU is fine; uses bf16/fp16.     |

For a 1B-parameter base model with `lora_r=16`, a 16 GB card (e.g. RTX 4080
or A4000) is enough to train at `batch_size=4` with gradient checkpointing.
Larger base models or higher LoRA ranks will need more VRAM.

## Layout

```
tools/finetune/
├── pyproject.toml          # project metadata + dependencies
├── requirements.txt        # pip-installable deps
├── generate_dataset.py     # trajectory -> supervised dataset
├── train_lora.py           # supervised dataset -> LoRA adapter
└── README.md               # this file
```
