# 🦙 Ollama — Deep Technical Documentation

> **Source:** Official Ollama docs (`docs.ollama.com`), GitHub API reference (`github.com/ollama/ollama/docs/api.md`), and live endpoint verification.  
> **Last updated:** July 2026

---

## Table of Contents

1. [Architecture Overview](#1-architecture-overview)
2. [Server & Base URLs](#2-server--base-urls)
3. [Native REST API — All Endpoints](#3-native-rest-api--all-endpoints)
   - [GET /api/tags — List Models](#31-get-apitags--list-local-models)
   - [POST /api/show — Model Info](#32-post-apishow--show-model-information)
   - [POST /api/generate — Raw Completion](#33-post-apigenerate--raw-completion)
   - [POST /api/chat — Chat Completion](#34-post-apichat--chat-completion)
   - [POST /api/embed — Embeddings](#35-post-apiembed--generate-embeddings)
   - [POST /api/pull — Pull Model](#36-post-apipull--pull-model)
   - [POST /api/push — Push Model](#37-post-apipush--push-model)
   - [POST /api/create — Create Model](#38-post-apicreate--create-model)
   - [DELETE /api/delete — Delete Model](#39-delete-apidelete--delete-model)
   - [POST /api/copy — Copy Model](#310-post-apicopy--copy-model)
   - [GET /api/ps — Running Models](#311-get-apips--list-running-models)
   - [Blob Endpoints](#312-blob-endpoints)
   - [GET /api/version — Server Version](#313-get-apiversion)
4. [OpenAI-Compatible API](#4-openai-compatible-api)
5. [Generation Options Reference](#5-generation-options-reference)
6. [Capability: Streaming](#6-capability-streaming)
7. [Capability: Thinking / Chain-of-Thought](#7-capability-thinking--chain-of-thought)
8. [Capability: Structured Outputs](#8-capability-structured-outputs)
9. [Capability: Tool Calling (Function Calling)](#9-capability-tool-calling-function-calling)
10. [Capability: Vision / Multimodal](#10-capability-vision--multimodal)
11. [Capability: Embeddings](#11-capability-embeddings)
12. [Context Length Management](#12-context-length-management)
13. [Model Memory & Keep-Alive](#13-model-memory--keep-alive)
14. [Modelfile Reference](#14-modelfile-reference)
15. [CLI Reference](#15-cli-reference)
16. [Environment Variables](#16-environment-variables)
17. [Platform-Specific Notes](#17-platform-specific-notes)
    - [Windows](#171-windows)
    - [macOS](#172-macos)
    - [Linux](#173-linux)
18. [Complete Endpoint Quick-Reference Table](#18-complete-endpoint-quick-reference-table)

---

## 1. Architecture Overview

Ollama is a local LLM runtime server that:
- Exposes a **native REST API** at `http://localhost:11434/api`
- Also exposes an **OpenAI-compatible API** at `http://localhost:11434/v1`
- Manages model storage, loading/unloading from VRAM/RAM
- Handles process spawning, GPU detection, and memory management automatically
- Runs as a **background process** (system tray on Windows/macOS, systemd service on Linux)

```
User / Application
       │
       ▼
┌─────────────────────────────────────────────┐
│           Ollama Server (:11434)             │
│  ┌──────────────────┐ ┌──────────────────┐  │
│  │  Native REST API │ │  OpenAI Compat   │  │
│  │  /api/*          │ │  /v1/*           │  │
│  └──────────────────┘ └──────────────────┘  │
│         ▼                                    │
│  ┌─────────────────────────────────────────┐ │
│  │          Model Runtime (llama.cpp)      │ │
│  │  GPU (CUDA/ROCm/Metal/Vulkan/DirectML)  │ │
│  └─────────────────────────────────────────┘ │
└─────────────────────────────────────────────┘
```

---

## 2. Server & Base URLs

| Purpose | URL |
|---------|-----|
| Native API base | `http://localhost:11434/api` |
| OpenAI-compat base | `http://localhost:11434/v1` |
| Default host | `127.0.0.1:11434` |
| Override via env | `OLLAMA_HOST=0.0.0.0:11434` |

> No authentication is required by default. OpenAI-compat endpoints require an `Authorization: Bearer <any-string>` header (e.g., `"ollama"`), but the value is not validated.

---

## 3. Native REST API — All Endpoints

### 3.1 GET `/api/tags` — List Local Models

Returns all locally downloaded models.

```bash
curl http://localhost:11434/api/tags
```

**Response:**
```json
{
  "models": [
    {
      "name": "llama3.2:latest",
      "model": "llama3.2:latest",
      "modified_at": "2025-09-26T14:38:54.725Z",
      "size": 2019393189,
      "digest": "sha256-a80c4f17...",
      "details": {
        "parent_model": "",
        "format": "gguf",
        "family": "llama",
        "families": ["llama"],
        "parameter_size": "3.2B",
        "quantization_level": "Q4_K_M"
      }
    }
  ]
}
```

**Response field breakdown:**

| Field | Type | Description |
|-------|------|-------------|
| `name` | string | Model tag (e.g. `llama3.2:latest`) |
| `model` | string | Model identifier |
| `modified_at` | string | ISO 8601 timestamp of last modification |
| `size` | integer | Size in bytes |
| `digest` | string | SHA256 hash of the model |
| `details.format` | string | Always `"gguf"` for local models |
| `details.family` | string | Model architecture (e.g. `"llama"`, `"gemma"`) |
| `details.parameter_size` | string | Human-readable param count (e.g. `"3.2B"`) |
| `details.quantization_level` | string | Quantization type (e.g. `"Q4_K_M"`, `"F16"`) |

---

### 3.2 POST `/api/show` — Show Model Information

Returns detailed metadata about a model including its Modelfile, template, system prompt, and parameters.

```bash
curl http://localhost:11434/api/show -d '{"model": "llama3.2", "verbose": true}'
```

**Request fields:**

| Field | Required | Description |
|-------|----------|-------------|
| `model` | ✅ | Model name |
| `verbose` | No | If `true`, returns low-level architecture metadata in `model_info` |

**Response fields:**

| Field | Description |
|-------|-------------|
| `license` | Model's legal license text |
| `modelfile` | Full Modelfile contents as a string |
| `parameters` | Runtime parameters as text (e.g. `stop "<\|eot_id\|>"`) |
| `template` | Go template string used for prompting |
| `system` | System prompt embedded in the model |
| `details` | Same structure as `/api/tags` details |
| `model_info` | (verbose only) Low-level architecture metadata (layers, head count, etc.) |

> [!TIP]
> Use `/api/show` to auto-detect model capabilities (e.g., context length from `model_info.llama.context_length`, whether the model supports thinking, etc.)

---

### 3.3 POST `/api/generate` — Raw Completion

Single-turn text completion. Streaming by default.

```
POST /api/generate
```

**Full Request Schema:**
```json
{
  "model": "llama3.2",
  "prompt": "Why is the sky blue?",
  "suffix": "",
  "images": ["<base64-string>"],
  "system": "You are a scientist.",
  "template": "...",
  "context": [],
  "stream": true,
  "raw": false,
  "format": "json",
  "think": true,
  "keep_alive": "5m",
  "options": {
    "temperature": 0.7,
    "num_ctx": 8192,
    "seed": 42
  }
}
```

**Request Parameter Reference:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `model` | string | — | **Required.** Model name (`model:tag`) |
| `prompt` | string | — | The prompt text |
| `suffix` | string | — | Text to append after the model's response (for fill-in-middle / FIM) |
| `images` | array | — | Base64-encoded images for multimodal models |
| `system` | string | — | Override the model's system prompt |
| `template` | string | — | Override the model's prompt template |
| `context` | array | — | Token context array from a prior `/api/generate` call (deprecated conversation memory) |
| `stream` | bool | `true` | If `false`, returns a single JSON response instead of NDJSON stream |
| `raw` | bool | `false` | If `true`, bypasses the template — use when providing a fully-formatted prompt |
| `format` | string/object | — | `"json"` for JSON mode, or a JSON Schema object for structured output |
| `think` | bool/string | — | Enable thinking. `true`/`false` or levels: `"low"`, `"medium"`, `"high"`, `"max"` |
| `keep_alive` | string | `"5m"` | How long to keep model loaded after request. `"0"` = unload immediately, `"-1"` = keep forever |
| `options` | object | — | Advanced generation parameters (see [Section 5](#5-generation-options-reference)) |

**Streaming Response (NDJSON — one JSON per line):**
```json
{"model": "llama3.2", "created_at": "2023-08-04T08:52:19Z", "response": "The", "done": false}
{"model": "llama3.2", "created_at": "2023-08-04T08:52:19Z", "response": " sky", "done": false}
```

**Final chunk (with stats):**
```json
{
  "model": "llama3.2",
  "created_at": "2023-08-04T19:22:45Z",
  "response": "",
  "done": true,
  "done_reason": "stop",
  "context": [1, 2, 3],
  "total_duration": 10706818083,
  "load_duration": 6338219291,
  "prompt_eval_count": 26,
  "prompt_eval_duration": 130079000,
  "eval_count": 259,
  "eval_duration": 4232710000
}
```

**Performance formula:** `tokens/sec = eval_count / eval_duration * 10^9`

**When `think: true`**, each chunk also contains:
```json
{"response": "", "thinking": "Let me reason about this...", "done": false}
```

---

### 3.4 POST `/api/chat` — Chat Completion

Multi-turn chat with message history. The primary endpoint for conversational AI.

```
POST /api/chat
```

**Full Request Schema:**
```json
{
  "model": "llama3.2",
  "messages": [
    {"role": "system", "content": "You are a helpful assistant."},
    {"role": "user", "content": "Hello", "images": ["<base64>"]},
    {"role": "assistant", "content": "Hi there!"},
    {"role": "tool", "tool_name": "get_weather", "content": "22°C"}
  ],
  "tools": [...],
  "stream": false,
  "format": {...},
  "think": true,
  "keep_alive": "5m",
  "options": {...}
}
```

**Message Roles:**

| Role | Description |
|------|-------------|
| `system` | System-level instructions |
| `user` | Human turn. Supports `images` field for multimodal |
| `assistant` | Prior model response. Supports `thinking` and `tool_calls` fields |
| `tool` | Tool execution result. Requires `tool_name` field |

**Non-streaming Response:**
```json
{
  "model": "llama3.2",
  "created_at": "2023-12-12T14:13:43Z",
  "message": {
    "role": "assistant",
    "content": "The answer is 42.",
    "thinking": "Let me work through this...",
    "tool_calls": [
      {
        "function": {
          "name": "get_weather",
          "arguments": {"city": "London"}
        }
      }
    ]
  },
  "done": true,
  "done_reason": "stop",
  "total_duration": 5191566416,
  "load_duration": 2154458,
  "prompt_eval_count": 26,
  "prompt_eval_duration": 383809000,
  "eval_count": 298,
  "eval_duration": 4799921000
}
```

> [!IMPORTANT]
> For streaming `/api/chat`, the token appears in `message.content` (not `response`). Thinking tokens appear in `message.thinking`. Both must be **accumulated** across chunks to reconstruct the full message for the next request.

---

### 3.5 POST `/api/embed` — Generate Embeddings

Generate vector embeddings for one or more text inputs.

```bash
# Single input
curl http://localhost:11434/api/embed -d '{
  "model": "nomic-embed-text",
  "input": "The sky is blue"
}'

# Batch input
curl http://localhost:11434/api/embed -d '{
  "model": "nomic-embed-text",
  "input": ["The sky is blue", "The sun is yellow"]
}'
```

**Response:**
```json
{
  "model": "nomic-embed-text",
  "embeddings": [
    [0.0107422, -0.00823975, ...],
    [0.0128174, -0.0076294, ...]
  ],
  "total_duration": 14143917,
  "load_duration": 1019500,
  "prompt_eval_count": 8
}
```

> **Note:** Vectors are **L2-normalized**. The old endpoint `/api/embeddings` is deprecated — always use `/api/embed`.

---

### 3.6 POST `/api/pull` — Pull Model

Download a model from the Ollama registry. Streams download progress.

```json
{"model": "llama3.2", "stream": true}
```

**Streamed response chunks:**
```json
{"status": "pulling manifest"}
{"status": "pulling f43380823ab2", "digest": "sha256-f43380823ab2", "total": 3825819519, "completed": 1024}
{"status": "verifying sha256 digest"}
{"status": "writing manifest"}
{"status": "success"}
```

---

### 3.7 POST `/api/push` — Push Model to Registry

```json
{"model": "myusername/mymodel:v1.0", "stream": true}
```

Requires being signed in (`ollama signin`). Streams upload progress.

---

### 3.8 POST `/api/create` — Create Model

Create a new model from a Modelfile string inline.

```json
{
  "model": "my-assistant",
  "modelfile": "FROM llama3.2\nSYSTEM \"You are a helpful coding assistant.\"\nPARAMETER temperature 0.3\nPARAMETER num_ctx 8192",
  "stream": true
}
```

Alternatively, point to a file path:
```json
{"model": "my-model", "path": "/path/to/Modelfile", "stream": false}
```

---

### 3.9 DELETE `/api/delete` — Delete Model

```json
{"model": "llama3.2:latest"}
```

Returns HTTP 200 on success, HTTP 404 if model not found.

---

### 3.10 POST `/api/copy` — Copy Model

Clone a model under a new name:
```json
{"source": "llama3.2", "destination": "my-llama"}
```

---

### 3.11 GET `/api/ps` — List Running Models

Returns all models currently loaded into memory (VRAM or RAM).

```bash
curl http://localhost:11434/api/ps
```

**Response:**
```json
{
  "models": [
    {
      "name": "llama3.2:latest",
      "model": "llama3.2:latest",
      "size": 2019393189,
      "digest": "sha256-abc123",
      "details": {...},
      "expires_at": "2024-06-04T14:38:31.83751Z",
      "size_vram": 2019393189
    }
  ]
}
```

| Field | Description |
|-------|-------------|
| `expires_at` | When the model will be unloaded due to keep-alive timeout |
| `size_vram` | Bytes currently occupying VRAM (0 if CPU-only) |

---

### 3.12 Blob Endpoints

Used for importing raw model weight files (GGUF blobs).

| Method | Endpoint | Description |
|--------|----------|-------------|
| `HEAD` | `/api/blobs/:digest` | Check if a blob exists by SHA256 digest |
| `POST` | `/api/blobs/:digest` | Upload a raw blob (raw binary body) |

```bash
# Check existence
curl -I http://localhost:11434/api/blobs/sha256-abc123

# Upload (from file)
curl -T model.gguf http://localhost:11434/api/blobs/sha256-$(sha256sum model.gguf | cut -d' ' -f1)
```

---

### 3.13 GET `/api/version`

Returns the running Ollama server version.

```bash
curl http://localhost:11434/api/version
# {"version": "0.9.0"}
```

---

## 4. OpenAI-Compatible API

Ollama ships a drop-in OpenAI API compatibility layer. Use any OpenAI SDK by pointing `base_url` to the Ollama server.

**Base URL:** `http://localhost:11434/v1`  
**API Key:** Any non-empty string (e.g., `"ollama"`) — required by header but not validated.

### Supported Endpoints

| Method | Endpoint | Description |
|--------|----------|-------------|
| `GET` | `/v1/models` | List available models |
| `POST` | `/v1/chat/completions` | Chat completions (supports tools, streaming) |
| `POST` | `/v1/completions` | Text completions |
| `POST` | `/v1/embeddings` | Embeddings |

### Python (OpenAI SDK):
```python
from openai import OpenAI

client = OpenAI(
    base_url="http://localhost:11434/v1",
    api_key="ollama"   # required but not validated
)

response = client.chat.completions.create(
    model="llama3.2",
    messages=[{"role": "user", "content": "Hello!"}]
)
print(response.choices[0].message.content)
```

### Structured Outputs via OpenAI-compat:
```python
response = client.chat.completions.create(
    model="llama3.2",
    messages=[...],
    response_format={"type": "json_schema", "json_schema": {...}}
)
```

> [!WARNING]
> OpenAI compatibility is marked **experimental**. Not all OpenAI features may work. Tool calling and structured outputs are supported. Vision support depends on model.

---

## 5. Generation Options Reference

Passed as the `options` object in `/api/generate` or `/api/chat`. All are optional.

### Sampling Parameters

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `temperature` | float | `0.8` | Randomness. `0` = deterministic, higher = more creative |
| `top_k` | int | `40` | Top-K sampling — limit to top K most probable tokens |
| `top_p` | float | `0.9` | Nucleus sampling — cumulative probability cutoff |
| `min_p` | float | `0.0` | Min probability relative to the most likely token |
| `typical_p` | float | `1.0` | Typical probability sampling |
| `repeat_penalty` | float | `1.1` | Penalize repeated tokens (>1 = stronger penalty) |
| `repeat_last_n` | int | `64` | Look-back window for repetition penalty. `0` = disabled, `-1` = full context |
| `presence_penalty` | float | `0.0` | Penalize tokens that have appeared at all |
| `frequency_penalty` | float | `0.0` | Penalize tokens by how often they appeared |
| `seed` | int | `0` | Fixed random seed for reproducibility |
| `mirostat` | int | `0` | `0` = off, `1` = Mirostat v1, `2` = Mirostat v2 |
| `mirostat_eta` | float | `0.1` | Mirostat learning rate |
| `mirostat_tau` | float | `5.0` | Mirostat target perplexity |
| `tfs_z` | float | `1.0` | Tail-free sampling Z parameter |

### Context & Generation Control

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `num_ctx` | int | `2048` | Context window size in tokens |
| `num_predict` | int | `-1` | Max output tokens (`-1` = unlimited, `128` = limit to 128) |
| `draft_num_predict` | int | `4` | Draft tokens for speculative decoding |
| `num_keep` | int | `5` | Number of tokens from the initial prompt to always retain |
| `stop` | string[] | — | Stop sequences — model halts on any match |

### Hardware Control

| Parameter | Type | Description |
|-----------|------|-------------|
| `num_gpu` | int | Number of GPU layers to offload (0 = CPU only) |
| `num_thread` | int | CPU threads to use |
| `numa` | bool | Enable NUMA memory allocation |
| `use_mmap` | bool | Memory-map model file |
| `use_mlock` | bool | Lock model in RAM (prevent swap) |
| `low_vram` | bool | Reduce VRAM usage (may reduce speed) |

### Example — All Options:
```json
{
  "model": "llama3.2",
  "prompt": "Why is the sky blue?",
  "stream": false,
  "options": {
    "num_ctx": 8192,
    "num_predict": 512,
    "temperature": 0.7,
    "top_k": 40,
    "top_p": 0.9,
    "min_p": 0.0,
    "repeat_penalty": 1.1,
    "repeat_last_n": 64,
    "seed": 42,
    "stop": ["Human:", "###"],
    "num_gpu": 99,
    "num_thread": 8
  }
}
```

---

## 6. Capability: Streaming

Streaming is **enabled by default** in the REST API and **disabled by default** in the SDKs.

### Format: NDJSON (Newline-Delimited JSON)

Each line is a self-contained JSON object. This is **not** standard SSE format.

```
{"model":"llama3.2","created_at":"...","response":"Hello","done":false}\n
{"model":"llama3.2","created_at":"...","response":" there","done":false}\n
{"model":"llama3.2","created_at":"...","response":"","done":true,"eval_count":15,...}\n
```

### Three streaming scenarios:

#### 1. Basic chat streaming
Each chunk has `message.content` with a token fragment.

```python
from ollama import chat

stream = chat(
    model='qwen3',
    messages=[{'role': 'user', 'content': 'What is 17 × 23?'}],
    stream=True,
)

for chunk in stream:
    print(chunk.message.content, end='', flush=True)
```

#### 2. Thinking + content streaming
Detect `message.thinking` field to identify reasoning tokens before final answer tokens.

```python
in_thinking = False
thinking = ''
content = ''

for chunk in stream:
    if chunk.message.thinking:
        if not in_thinking:
            in_thinking = True
            print('🤔 Thinking:\n', end='')
        print(chunk.message.thinking, end='', flush=True)
        thinking += chunk.message.thinking
    elif chunk.message.content:
        if in_thinking:
            in_thinking = False
            print('\n\n💬 Answer:\n', end='')
        print(chunk.message.content, end='', flush=True)
        content += chunk.message.content
```

#### 3. Tool calls streaming
`tool_calls` appear in chunks. Accumulate and process after stream ends.

```python
tool_calls = []
for chunk in stream:
    if chunk.message.tool_calls:
        tool_calls.extend(chunk.message.tool_calls)
# Process all accumulated tool calls after streaming completes
```

> [!IMPORTANT]
> When using streaming with tool calling or thinking, you **must accumulate** all partial fields (`thinking`, `content`, `tool_calls`) and send them back as a single message object in the next request. Failing to do this causes the model to lose context.

### Disable streaming (single response):
```json
{"model": "llama3.2", "messages": [...], "stream": false}
```

---

## 7. Capability: Thinking / Chain-of-Thought

Models with thinking support emit a `thinking` field containing the internal reasoning trace, separate from the final `content`/`response`.

### Supported Models (as of July 2026)

| Model | Think Control | Notes |
|-------|--------------|-------|
| `qwen3` | `true`/`false` or levels | Default: enabled |
| `deepseek-r1` | `true`/`false` | Default: enabled |
| `deepseek-v3.1` | `true`/`false` | Default: enabled |
| `gpt-oss` | `"low"`, `"medium"`, `"high"` only | Cannot fully disable; no boolean |

Browse all: [ollama.com/search?c=thinking](https://ollama.com/search?c=thinking)

### API Control

**In request body (`/api/chat` or `/api/generate`):**
```json
{"think": true}    // enable (boolean)
{"think": false}   // disable
{"think": "high"}  // level: "low", "medium", "high", "max"
```

> [!NOTE]
> GPT-OSS only accepts levels (`"low"`, `"medium"`, `"high"`). Passing `true`/`false` is silently ignored for that model.

### Response Fields

| Endpoint | Thinking field | Answer field |
|----------|---------------|--------------|
| `/api/chat` | `message.thinking` | `message.content` |
| `/api/generate` | `thinking` (top-level) | `response` |

**Example — `/api/chat` with `think: true`:**
```bash
curl http://localhost:11434/api/chat -d '{
  "model": "qwen3",
  "messages": [{"role": "user", "content": "How many Rs in strawberry?"}],
  "think": true,
  "stream": false
}'
```

**Response:**
```json
{
  "message": {
    "role": "assistant",
    "thinking": "Let me count: s-t-r-a-w-b-e-r-r-y. I see r at positions 3, 8, 9. That's 3 Rs.",
    "content": "There are 3 letter Rs in the word 'strawberry'."
  }
}
```

### CLI Control

```bash
ollama run qwen3 --think "Where should I visit in Lisbon?"
ollama run deepseek-r1 --think=false "Summarize this article"
ollama run deepseek-r1 --hidethinking "Is 9.9 bigger than 9.11?"
ollama run gpt-oss --think=low "Draft a headline"
```

**Inside interactive session:**
```
/set think        → enable thinking
/set nothink      → disable thinking
```

---

## 8. Capability: Structured Outputs

Ollama supports two modes of structured output enforcement:

| Mode | `format` value | Description |
|------|---------------|-------------|
| **JSON mode** | `"json"` | Guarantees valid JSON; structure not enforced |
| **Schema mode** | JSON Schema object | Constrained decoding — forces output to exactly match schema |

> [!NOTE]
> Ollama Cloud **does not** support structured outputs. Local only.

### Mode 1: JSON Mode

```bash
curl -X POST http://localhost:11434/api/chat -d '{
  "model": "llama3.2",
  "messages": [{"role": "user", "content": "Tell me about Canada. Respond in JSON."}],
  "stream": false,
  "format": "json"
}'
```

> [!IMPORTANT]
> Always instruct the model in the prompt to respond in JSON. Without this, the model may produce excessive whitespace or non-JSON preamble.

### Mode 2: Schema-Enforced Structured Output

Provide a JSON Schema object to `format`. Ollama uses **constrained decoding** to guarantee the output exactly matches the schema.

```bash
curl -X POST http://localhost:11434/api/chat -d '{
  "model": "llama3.2",
  "messages": [{"role": "user", "content": "Tell me about Canada."}],
  "stream": false,
  "format": {
    "type": "object",
    "properties": {
      "name":      {"type": "string"},
      "capital":   {"type": "string"},
      "languages": {"type": "array", "items": {"type": "string"}}
    },
    "required": ["name", "capital", "languages"]
  }
}'
```

**Response:**
```json
{
  "message": {
    "role": "assistant",
    "content": "{\"name\":\"Canada\",\"capital\":\"Ottawa\",\"languages\":[\"English\",\"French\"]}"
  }
}
```

### Python with Pydantic (Recommended Pattern):
```python
from ollama import chat
from pydantic import BaseModel

class Country(BaseModel):
    name: str
    capital: str
    languages: list[str]

response = chat(
    model='llama3.2',
    messages=[{'role': 'user', 'content': 'Tell me about Canada.'}],
    format=Country.model_json_schema(),
)

# Parse and validate
country = Country.model_validate_json(response.message.content)
print(country.name, country.capital)
```

### Vision + Structured Outputs:
```python
class ImageDescription(BaseModel):
    summary: str
    objects: list[str]
    scene: str
    time_of_day: Literal['Morning', 'Afternoon', 'Evening', 'Night']

response = chat(
    model='gemma4',
    messages=[{
        'role': 'user',
        'content': 'Describe this image.',
        'images': ['path/to/image.jpg'],
    }],
    format=ImageDescription.model_json_schema(),
    options={'temperature': 0},  # Lower temp = more deterministic
)
```

### Tips for Reliable Structured Outputs:
- Set `temperature: 0` for maximum determinism
- Include the schema description in the prompt text too
- Use Pydantic (Python) or Zod (JavaScript) for schema reuse + validation
- Works through OpenAI-compat API via `response_format`

---

## 9. Capability: Tool Calling (Function Calling)

Ollama supports native tool calling, allowing models to request execution of external functions and use their results.

### Best Models for Tool Calling
- `llama3.1`, `llama3.3` (and later)
- `qwen2.5`, `qwen3`
- `mistral-nemo`
- `deepseek-r1` (with thinking)

### Tool Schema Format

```json
{
  "type": "function",
  "function": {
    "name": "function_name",
    "description": "What this function does",
    "parameters": {
      "type": "object",
      "required": ["param1"],
      "properties": {
        "param1": {
          "type": "string",
          "description": "Description of param1"
        },
        "param2": {
          "type": "integer",
          "description": "Description of param2"
        }
      }
    }
  }
}
```

### Pattern 1: Single-Shot Tool Calling

**Step 1 — Send tool definitions with user message:**
```bash
curl http://localhost:11434/api/chat -d '{
  "model": "qwen3",
  "messages": [{"role": "user", "content": "What is the temperature in New York?"}],
  "stream": false,
  "tools": [{
    "type": "function",
    "function": {
      "name": "get_temperature",
      "description": "Get the current temperature for a city",
      "parameters": {
        "type": "object",
        "required": ["city"],
        "properties": {
          "city": {"type": "string", "description": "City name"}
        }
      }
    }
  }]
}'
```

**Model response with tool call:**
```json
{
  "message": {
    "role": "assistant",
    "content": "",
    "tool_calls": [{
      "function": {
        "index": 0,
        "name": "get_temperature",
        "arguments": {"city": "New York"}
      }
    }]
  }
}
```

**Step 2 — Execute tool and send result:**
```bash
curl http://localhost:11434/api/chat -d '{
  "model": "qwen3",
  "messages": [
    {"role": "user", "content": "What is the temperature in New York?"},
    {"role": "assistant", "tool_calls": [{"function": {"index": 0, "name": "get_temperature", "arguments": {"city": "New York"}}}]},
    {"role": "tool", "tool_name": "get_temperature", "content": "22°C"}
  ],
  "stream": false
}'
```

### Pattern 2: Parallel Tool Calling

Model can request multiple tools at once. Each `tool_call` has an `index` field to identify it.

```json
{
  "tool_calls": [
    {"function": {"index": 0, "name": "get_temperature", "arguments": {"city": "New York"}}},
    {"function": {"index": 1, "name": "get_conditions", "arguments": {"city": "New York"}}},
    {"function": {"index": 2, "name": "get_temperature", "arguments": {"city": "London"}}},
    {"function": {"index": 3, "name": "get_conditions", "arguments": {"city": "London"}}}
  ]
}
```

Return all results as separate `tool` messages in the next request.

### Pattern 3: Multi-Turn Agent Loop

Run a while loop that continues until no more `tool_calls` are returned.

```python
from ollama import chat

messages = [{'role': 'user', 'content': 'What is (11434+12341)*412?'}]

while True:
    response = chat(
        model='qwen3',
        messages=messages,
        tools=[add, multiply],
        think=True,
    )
    messages.append(response.message)

    if response.message.tool_calls:
        for tc in response.message.tool_calls:
            result = available_functions[tc.function.name](**tc.function.arguments)
            messages.append({
                'role': 'tool',
                'tool_name': tc.function.name,
                'content': str(result)
            })
    else:
        break  # No more tool calls — final answer available

print(response.message.content)
```

### Pattern 4: Tool Calling with Streaming

Accumulate all fields during the stream, then process:

```python
thinking = ''
content = ''
tool_calls = []

stream = chat(model='qwen3', messages=messages, tools=[...], stream=True, think=True)

for chunk in stream:
    if chunk.message.thinking:
        thinking += chunk.message.thinking
    if chunk.message.content:
        content += chunk.message.content
    if chunk.message.tool_calls:
        tool_calls.extend(chunk.message.tool_calls)

# After stream: add accumulated assistant message
messages.append({
    'role': 'assistant',
    'thinking': thinking,
    'content': content,
    'tool_calls': tool_calls
})

# Process tool calls and append tool results
for call in tool_calls:
    result = execute_tool(call.function.name, call.function.arguments)
    messages.append({'role': 'tool', 'tool_name': call.function.name, 'content': result})
```

### Python SDK Auto-Schema Parsing

The Python SDK automatically converts Python functions into JSON schemas from their type hints and docstrings:

```python
def get_weather(city: str) -> str:
    """Get the current temperature for a city.
    
    Args:
        city: The name of the city
    Returns:
        The current temperature
    """
    return "22°C"

# Pass function directly — SDK converts to schema automatically
response = chat(model='qwen3', messages=messages, tools=[get_weather])
```

---

## 10. Capability: Vision / Multimodal

Attach images to messages for vision-capable models.

### Supported Models
`llava`, `llava-phi3`, `moondream`, `bakllava`, `llama3.2-vision`, `gemma3`, `gemma4`, `gpt-oss`

### In `/api/chat`:
```json
{
  "role": "user",
  "content": "What is in this image?",
  "images": ["<base64-encoded-image-string>"]
}
```

### In `/api/generate`:
```json
{
  "model": "llava",
  "prompt": "Describe this image in detail.",
  "images": ["<base64-encoded-image-string>"],
  "stream": false
}
```

### Python SDK (auto-encodes file path):
```python
response = chat(
    model='llama3.2-vision',
    messages=[{
        'role': 'user',
        'content': 'What is in this image?',
        'images': ['/path/to/image.jpg'],  # auto-encoded to base64
    }]
)
```

### Vision + Structured Outputs:
Vision models fully support the `format` parameter, enabling deterministic structured descriptions of images (see Section 8).

---

## 11. Capability: Embeddings

Generate dense vector representations for semantic search, RAG, and similarity tasks.

```bash
curl http://localhost:11434/api/embed -d '{
  "model": "nomic-embed-text",
  "input": "Llamas are social animals"
}'
```

### Batch Embeddings:
```bash
curl http://localhost:11434/api/embed -d '{
  "model": "nomic-embed-text",
  "input": ["Llamas are social animals", "They live in herds"]
}'
```

### Recommended Embedding Models:
- `nomic-embed-text` — General purpose, 768-dim
- `mxbai-embed-large` — Higher quality, 1024-dim
- `all-minilm` — Lightweight, 384-dim
- `snowflake-arctic-embed` — Strong retrieval performance

### Properties:
- Vectors are **L2-normalized** (unit length)
- Supports `keep_alive` and `options` fields
- Use `/api/embed` (not deprecated `/api/embeddings`)

---

## 12. Context Length Management

Context length is the total number of tokens the model can "see" at once (prompt + response combined).

### Ollama's Automatic Defaults (by available VRAM):

| VRAM | Default Context |
|------|----------------|
| < 24 GiB | 4k tokens (4,096) |
| 24–48 GiB | 32k tokens (32,768) |
| ≥ 48 GiB | 256k tokens (262,144) |

> [!IMPORTANT]
> For agents, web search, RAG, and coding tasks, set context to at least **64,000 tokens**.

### Ways to Set Context Length:

**1. Per-request via `options`:**
```json
{
  "model": "llama3.2",
  "messages": [...],
  "options": {"num_ctx": 32768}
}
```

**2. Server-wide via environment variable:**
```bash
OLLAMA_CONTEXT_LENGTH=65536 ollama serve
```

**3. Permanently in Modelfile:**
```
FROM llama3.2
PARAMETER num_ctx 32768
```

**4. In Ollama App:** Settings → Context Length slider

### Verify Context & Offloading:
```bash
ollama ps
```
```
NAME             ID              SIZE      PROCESSOR    CONTEXT    UNTIL
gemma4:latest    c6eb396dbd59    9.6 GB    100% GPU     131072     2 minutes from now
```

- `100% GPU` = fully on GPU (best performance)
- `CPU` or `GPU/CPU` = split or CPU-only (slower)

---

## 13. Model Memory & Keep-Alive

Ollama keeps models loaded in VRAM/RAM between requests to avoid reload latency.

### Default Behavior:
- Models are unloaded after **5 minutes** of inactivity
- Server-wide default: `OLLAMA_KEEP_ALIVE=5m`

### Per-Request Control:

| `keep_alive` value | Effect |
|--------------------|--------|
| `"5m"` | Keep loaded for 5 minutes (default) |
| `"30m"` | Keep loaded for 30 minutes |
| `"0"` or `"0s"` | Unload immediately after request |
| `"-1"` | Keep loaded indefinitely (until server restart) |
| `"2h"` | Keep for 2 hours |

```json
{
  "model": "llama3.2",
  "messages": [...],
  "keep_alive": "30m"
}
```

### Pre-warm a model (load without generating):
```bash
curl http://localhost:11434/api/generate -d '{"model": "llama3.2", "keep_alive": -1}'
# Empty prompt → loads model, returns immediately, keeps in VRAM
```

### Manually unload a model:
```bash
# Via CLI
ollama stop llama3.2

# Via API
curl http://localhost:11434/api/generate -d '{"model": "llama3.2", "keep_alive": 0}'
```

### Monitor loaded models:
```bash
ollama ps
```

### Server-wide configuration:
```bash
OLLAMA_KEEP_ALIVE=30m ollama serve
OLLAMA_MAX_LOADED_MODELS=3 ollama serve   # max 3 models in memory simultaneously
OLLAMA_NUM_PARALLEL=4 ollama serve        # max 4 parallel requests per model
```

---

## 14. Modelfile Reference

Modelfiles are Dockerfile-like blueprints for customizing and sharing models.

### Format:
```
# Comments start with #
INSTRUCTION arguments
```

### Full Instruction Set:

| Instruction | Required | Description |
|-------------|----------|-------------|
| `FROM` | ✅ Yes | Base model to build from |
| `PARAMETER` | No | Runtime parameter defaults |
| `TEMPLATE` | No | Go template for prompt formatting |
| `SYSTEM` | No | System prompt |
| `ADAPTER` | No | LoRA/QLoRA adapter file path |
| `LICENSE` | No | Legal license text |
| `MESSAGE` | No | Pre-seed conversation history |
| `REQUIRES` | No | Minimum Ollama version required |

### `FROM` — Base Model Sources:

```
# From Ollama registry:
FROM llama3.2

# From local GGUF file:
FROM ./my-model.gguf

# From Safetensors directory:
FROM /path/to/safetensors/dir/
```

Supported Safetensors architectures: Llama (1/2/3/3.1/3.2), Mistral (1/2/Mixtral), Gemma (1/2), Phi3

### `PARAMETER` — All Valid Values:

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `num_ctx` | int | `2048` | Context window tokens |
| `num_predict` | int | `-1` | Max output tokens (`-1` = unlimited) |
| `draft_num_predict` | int | `4` | Speculative decoding draft tokens |
| `temperature` | float | `0.8` | Sampling temperature |
| `top_k` | int | `40` | Top-K sampling |
| `top_p` | float | `0.9` | Nucleus sampling |
| `min_p` | float | `0.0` | Min probability threshold |
| `repeat_penalty` | float | `1.1` | Repetition penalty |
| `repeat_last_n` | int | `64` | Repetition look-back window |
| `seed` | int | `0` | Random seed |
| `stop` | string | — | Stop sequences (can set multiple) |

### `TEMPLATE` — Go Template Variables:

| Variable | Description |
|----------|-------------|
| `{{ .System }}` | The system prompt |
| `{{ .Prompt }}` | The user's message |
| `{{ .Response }}` | Model's response (generation happens here) |

### Complete Modelfile Example:
```
FROM llama3.2

SYSTEM """
You are a senior Rust developer. Always:
- Write idiomatic Rust code
- Include error handling
- Explain your choices briefly
"""

PARAMETER temperature 0.3
PARAMETER num_ctx 16384
PARAMETER top_p 0.85
PARAMETER repeat_penalty 1.05
PARAMETER stop "<|eot_id|>"
PARAMETER stop "Human:"
```

### Common Workflows:
```bash
# Create model from Modelfile
ollama create my-rust-assistant -f ./Modelfile

# View existing model's Modelfile
ollama show llama3.2 --modelfile

# Create from inline API call
curl http://localhost:11434/api/create -d '{
  "model": "my-model",
  "modelfile": "FROM llama3.2\nSYSTEM \"You are helpful.\"\nPARAMETER temperature 0.5"
}'
```

---

## 15. CLI Reference

### Core Commands:

| Command | Description |
|---------|-------------|
| `ollama serve` | Start the API server manually |
| `ollama run <model>` | Interactive chat REPL |
| `ollama run <model> "prompt"` | One-shot prompt |
| `ollama pull <model>` | Download model |
| `ollama push <model>` | Upload model to registry |
| `ollama ls` / `ollama list` | List downloaded models |
| `ollama ps` | Show loaded models (with VRAM usage) |
| `ollama show <model>` | Show model info |
| `ollama show --modelfile <model>` | Show Modelfile |
| `ollama create <name> -f <file>` | Create model from Modelfile |
| `ollama cp <src> <dst>` | Copy model |
| `ollama rm <model>` | Delete model |
| `ollama stop <model>` | Unload model from memory |
| `ollama launch` | Launch IDE/app integrations |
| `ollama signin` | Sign in to Ollama registry |
| `ollama signout` | Sign out |

### `ollama run` Flags:

| Flag | Description |
|------|-------------|
| `--verbose` | Show token stats and timing after response |
| `--think` | Enable thinking/reasoning for supported models |
| `--think=false` | Disable thinking |
| `--hidethinking` | Run thinking but suppress trace output |
| `--keepalive <duration>` | Keep model loaded for specified time |

```bash
ollama run llama3.2 --verbose
ollama run qwen3 --think "Solve: 2x + 7 = 15"
ollama run deepseek-r1 --think=false "Summarize this"
ollama run qwen3 --hidethinking "What is the capital of France?"
ollama run llama3.2 --keepalive 30m
```

### Interactive Session Commands:
```
/set parameter <param> <value>  → Set generation parameter
/set think                      → Enable thinking
/set nothink                    → Disable thinking
/show modelfile                 → Display model's Modelfile
/show info                      → Show model details
/load <model>                   → Load a different model
/save <session>                 → Save session
/bye                            → Exit session
```

### `ollama launch` — IDE Integrations:
```bash
ollama launch                          # interactive: choose integration
ollama launch claude                   # launch Claude Code with Ollama
ollama launch claude --model qwen3.5   # specific model
ollama launch opencode                 # launch OpenCode
ollama launch droid --config           # configure without launching
```

**Supported integrations:** OpenCode, Claude Code, Codex (OpenAI), VS Code, Droid (Factory)

### Multiline Input in REPL:
```
>>> """Hello,
... world!
... """
```

### Vision in CLI:
```bash
ollama run llama3.2-vision "What is in this image? /path/to/image.png"
```

---

## 16. Environment Variables

Set these before starting `ollama serve` (or via systemd/launchd on Linux/macOS).

| Variable | Default | Description |
|----------|---------|-------------|
| `OLLAMA_HOST` | `127.0.0.1:11434` | Server bind address. Use `0.0.0.0:11434` to expose on network |
| `OLLAMA_MODELS` | `~/.ollama/models` | Model storage directory |
| `OLLAMA_KEEP_ALIVE` | `5m` | Default model keep-alive duration |
| `OLLAMA_CONTEXT_LENGTH` | (VRAM-based) | Override default context window for all requests |
| `OLLAMA_MAX_LOADED_MODELS` | — | Max number of models simultaneously in memory |
| `OLLAMA_NUM_PARALLEL` | — | Max parallel requests per model |
| `OLLAMA_FLASH_ATTENTION` | `0` | Enable Flash Attention (`1` = on) |
| `OLLAMA_KV_CACHE_TYPE` | — | KV cache quantization type (`q8_0`, `q4_0`, `f16`) |
| `CUDA_VISIBLE_DEVICES` | — | Restrict to specific NVIDIA GPU indices |
| `GGML_VK_VISIBLE_DEVICES` | — | Restrict to specific Vulkan GPU indices |
| `HSA_OVERRIDE_GFX_VERSION` | — | Force AMD GPU architecture (e.g., `10.3.0` for RX 6000) |
| `OLLAMA_DEBUG` | `0` | Enable verbose debug logging |

---

## 17. Platform-Specific Notes

### 17.1 Windows

**Requirements:**
- Windows 10 22H2 or newer (Home or Pro)
- NVIDIA: driver ≥ 551.61
- AMD ROCm: ROCm v7 / HIP7-capable driver; else Vulkan fallback

**GPU Acceleration:**
| GPU | Backend |
|-----|---------|
| NVIDIA | CUDA (auto-detected) |
| AMD Radeon | ROCm v7 or **DirectML** (no ROCm needed on Win10+) |
| Fallback | Vulkan (enabled by default) |

> [!NOTE]
> Some RDNA2 / RX 6000-series cards may not expose ROCm v7 on Windows. Vulkan is the recommended fallback. If a mixed iGPU+dGPU system picks the wrong GPU, set `GGML_VK_VISIBLE_DEVICES` to the discrete GPU index.

**Installation:**
```powershell
# Standard installer (no admin required)
OllamaSetup.exe

# Custom install dir
OllamaSetup.exe /DIR="D:\Ollama"
```

**File locations:**
| Path | Contents |
|------|----------|
| `%LOCALAPPDATA%\Ollama` | Logs, downloaded updates |
| `%LOCALAPPDATA%\Programs\Ollama` | Binaries (auto-added to PATH) |
| `%HOMEPATH%\.ollama` | Models and config |
| `%TEMP%\ollama*` | Temporary executable files |

**Setting Environment Variables on Windows:**
1. Start Menu → search "environment variables"
2. Click "Edit environment variables for your account"
3. Add/Edit variable (e.g., `OLLAMA_MODELS=D:\models`)
4. Click OK, then **Quit Ollama from system tray** and relaunch

**API from PowerShell:**
```powershell
(Invoke-WebRequest -Method POST -Body '{"model":"llama3.2", "prompt":"Why is the sky blue?", "stream": false}' -Uri http://localhost:11434/api/generate).Content | ConvertFrom-Json
```

**As a Windows Service (standalone CLI):**
```powershell
# Download ollama-windows-amd64.zip, extract, then use NSSM:
nssm install OllamaServer "C:\ollama\ollama.exe" "serve"
nssm start OllamaServer
```

---

### 17.2 macOS

**Requirements:**
- macOS 14 (Sonoma) or newer
- Apple Silicon (M1-M5) or Intel with Metal support

**GPU:** Metal API (automatic), MLX framework for Apple Silicon.  
Unified Memory Architecture means GPU and CPU share the same memory pool — no VRAM bottleneck.

**Installation:**
```bash
# Option 1: GUI app (recommended)
# Download .dmg → drag to Applications → auto-starts with menu bar icon

# Option 2: Homebrew (CLI only)
brew install ollama
# Requires manual: ollama serve
```

**Setting Environment Variables:**
```bash
# Set before starting app (persists across reboots)
launchctl setenv OLLAMA_HOST "0.0.0.0:11434"
launchctl setenv OLLAMA_MODELS "/Volumes/ExternalDrive/ollama-models"
# Then restart Ollama app
```

**Model storage:** `~/.ollama/models`

---

### 17.3 Linux

**Install:**
```bash
curl -fsSL https://ollama.com/install.sh | sh
```

Installs to `/usr/local/bin/ollama` and creates a `ollama.service` systemd unit.

**GPU Support:**

| GPU | Requirements |
|-----|-------------|
| NVIDIA | CUDA 12.1+ toolkit + drivers (auto-detected) |
| AMD ROCm | ROCm v7+; add `ollama` user to groups |
| Intel Arc | OneAPI (experimental) |
| Vulkan | Any Vulkan-capable GPU |

**AMD ROCm Setup:**
```bash
sudo usermod -aG render,video ollama
sudo systemctl restart ollama

# Force AMD GPU architecture version (for older cards):
# Add to systemd unit: Environment="HSA_OVERRIDE_GFX_VERSION=10.3.0"
```

**Configure via systemd:**
```bash
sudo systemctl edit ollama.service
```
```ini
[Service]
Environment="OLLAMA_HOST=0.0.0.0:11434"
Environment="OLLAMA_KEEP_ALIVE=30m"
Environment="OLLAMA_CONTEXT_LENGTH=32768"
Environment="CUDA_VISIBLE_DEVICES=0"
```
```bash
sudo systemctl daemon-reload && sudo systemctl restart ollama
```

**Verify GPU usage:**
```bash
ollama ps              # PROCESSOR column: "100% GPU" = fully offloaded
nvidia-smi             # Check VRAM usage
rocm-smi               # For AMD GPUs
```

**Run Ollama without GPU (CPU-only):**
```bash
OLLAMA_NUM_GPU=0 ollama serve
```

---

## 18. Complete Endpoint Quick-Reference Table

| Method | Endpoint | Description | Auth |
|--------|----------|-------------|------|
| `GET` | `/api/tags` | List all local models | None |
| `POST` | `/api/show` | Get model info/metadata/Modelfile | None |
| `POST` | `/api/generate` | Raw text completion (streaming) | None |
| `POST` | `/api/chat` | Chat completion with history (streaming) | None |
| `POST` | `/api/embed` | Generate embeddings (current) | None |
| `POST` | `/api/embeddings` | ⚠️ Deprecated — use `/api/embed` | None |
| `POST` | `/api/pull` | Download model from registry | None |
| `POST` | `/api/push` | Upload model to registry | Signed in |
| `POST` | `/api/create` | Create model from Modelfile | None |
| `DELETE` | `/api/delete` | Remove model | None |
| `POST` | `/api/copy` | Clone model under new name | None |
| `GET` | `/api/ps` | List currently loaded/running models | None |
| `HEAD` | `/api/blobs/:digest` | Check if blob exists | None |
| `POST` | `/api/blobs/:digest` | Upload raw blob (GGUF weight file) | None |
| `GET` | `/api/version` | Get server version string | None |
| `GET` | `/v1/models` | OpenAI-compat: list models | Bearer token |
| `POST` | `/v1/chat/completions` | OpenAI-compat: chat (tools, streaming) | Bearer token |
| `POST` | `/v1/completions` | OpenAI-compat: raw text completion | Bearer token |
| `POST` | `/v1/embeddings` | OpenAI-compat: embeddings | Bearer token |

---

*Documentation compiled from: [docs.ollama.com](https://docs.ollama.com), [github.com/ollama/ollama/docs/api.md](https://github.com/ollama/ollama/blob/main/docs/api.md) — July 2026*
