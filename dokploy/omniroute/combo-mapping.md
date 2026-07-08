# OmniRoute Combo Mapping

## pro-chat (Pro Chat)

- Strategy: `priority`
- Context length: `500000`
- Tags: `chat`, `premium`

1. `cc/claude-opus-4-8`
2. `agy/gemini-3.1-pro-high`
3. `zai/glm-5.2`

## best-chat (Best Chat)

- Strategy: `weighted`
- Context length: `500000`
- Tags: `chat`, `balanced`, `premium`

1. `cc/claude-sonnet-5`
2. `agy/gemini-3.5-flash-high`
3. `cx/gpt-5.5-high`
4. `bzl/kimi-k2.6`
5. `mistral/mistral-large-latest`

## chat (Chat)

- Strategy: `weighted`
- Context length: `500000`
- Tags: `chat`, `balanced`, `fast`

1. `cc/claude-sonnet-5`
2. `agy/gemini-3.5-flash-high`
3. `cx/gpt-5.5-high`
4. `mistral/mistral-large-latest`

## claude-opus (Claude Opus)

- Strategy: `priority`
- Context length: `200000`
- Tags: `reasoning_deep`, `coding`, `reasoning`, `premium`

1. `cc/claude-opus-4-8`
2. `bzl/claude-opus-4.7`
3. `agy/claude-opus-4-6-thinking`

## claude-sonnet (Claude Sonnet)

- Strategy: `priority`
- Context length: `200000`
- Tags: `coding`, `reasoning`, `chat`, `premium`, `balanced`

1. `cc/claude-sonnet-5`
2. `bzl/claude-sonnet-4.6`
3. `agy/claude-sonnet-4-6`

## pro-coding (Pro Coding)

- Strategy: `priority`
- Context length: `500000`
- Tags: `coding`, `premium`
- Description: You are an expert coding assistant. Write clean, efficient,
  well-documented code.

1. `cc/claude-opus-4-8`
2. `cx/gpt-5.5-xhigh`
3. `minimax/MiniMax-M3`

## best-coding (Best Coding)

- Strategy: `weighted`
- Context length: `500000`
- Tags: `coding`, `premium`, `balanced`
- Description: You are an expert coding assistant. Write clean, efficient,
  well-documented code.

1. `cc/claude-sonnet-5`
2. `cx/gpt-5.5-high`
3. `minimax/MiniMax-M3`

## coding (Coding)

- Strategy: `weighted`
- Context length: `500000`
- Tags: `coding`, `balanced`, `fast`, `premium`

1. `cc/claude-sonnet-5`
2. `cx/gpt-5.5-high`
3. `minimax/MiniMax-M3`
4. `mistral/codestral-latest`

## best-coding-fast (Best Coding Fast)

- Strategy: `weighted`
- Context length: `500000`
- Tags: `coding`, `fast`, `fast`, `balanced`

1. `cx/gpt-5.3-codex-spark`
2. `minimax/MiniMax-M3`

## pro-fast (Pro Fast)

- Strategy: `priority`
- Context length: `500000`
- Tags: `fast`, `fast`

1. `agy/gemini-3.5-flash-high`
2. `gh/gemini-3.5-flash`
3. `cx/gpt-5.3-codex-spark`

## best-fast (Best Fast)

- Strategy: `weighted`
- Context length: `500000`
- Tags: `fast`, `fast`, `balanced`

1. `cx/gpt-5.3-codex-spark`
2. `agy/gemini-3.5-flash-medium`
3. `gh/gemini-3.5-flash`

## fast (Fast)

- Strategy: `weighted`
- Context length: `500000`
- Tags: `fast`, `fast`

1. `agy/gemini-3.5-flash-medium`
2. `gh/gemini-3.5-flash`
3. `cx/gpt-5.3-codex-spark`

## best-free (Best Free)

- Strategy: `weighted`
- Context length: `131072`
- Tags: `coding`, `chat`, `fast`, `free`
- Description: You are a helpful coding assistant. Write clean, efficient code.

1. `openrouter/auto`
2. `bzl/auto:free`
3. `mistral/devstral-latest`
4. `ollamacloud/minimax-m3`
5. `nvidia/deepseek-ai/deepseek-v4-pro`
6. `cerebras/gpt-oss-120b`

## pro-reasoning (Pro Reasoning)

- Strategy: `priority`
- Context length: `500000`
- Tags: `reasoning_deep`, `premium`
- Description: You are a deep reasoning assistant. Think carefully step by step.

1. `cx/gpt-5.5-xhigh`
2. `zai/glm-5.2`
3. `cc/claude-opus-4-8`

## best-reasoning (Best Reasoning)

- Strategy: `weighted`
- Context length: `500000`
- Tags: `reasoning_deep`, `reasoning`, `premium`
- Description: You are a deep reasoning assistant. Think carefully step by step.

1. `cx/gpt-5.5-high`
2. `zai/glm-5.1`
3. `cc/claude-sonnet-5`

## reasoning (Reasoning)

- Strategy: `weighted`
- Context length: `500000`
- Tags: `reasoning`, `balanced`

1. `cx/gpt-5.5-high`
2. `zai/glm-5.2`
3. `cc/claude-sonnet-5`
4. `vertex/gemini-3.1-pro-preview`

## pro-vision (Pro Vision)

- Strategy: `priority`
- Context length: `500000`
- Tags: `vision`, `premium`

1. `cx/gpt-5.5-high`

## best-vision (Best Vision)

- Strategy: `weighted`
- Context length: `500000`
- Tags: `vision`, `premium`, `balanced`

1. `cx/gpt-5.5-high`

---

## Model → providers — exact model IDs only

Grouped only when the model id after the first `provider/` prefix is exactly
identical. No alias, version, case, or family normalization is applied. Provider
order is best-effort preferred order: official/direct first, hosted
infrastructure next, reseller/free last.

- `claude-sonnet-4-6`: `cc/claude-sonnet-4-6` → `vertex/claude-sonnet-4-6` →
  `agy/claude-sonnet-4-6`
- `gemini-3.1-pro-preview`: `vertex/gemini-3.1-pro-preview` →
  `gh/gemini-3.1-pro-preview` → `bzl/gemini-3.1-pro-preview`
- `glm-5.1`: `zai/glm-5.1` → `ollamacloud/glm-5.1` → `bzl/glm-5.1`
- `gpt-5.4`: `cx/gpt-5.4` → `gh/gpt-5.4` → `bzl/gpt-5.4`
- `gpt-5.4-mini`: `cx/gpt-5.4-mini` → `gh/gpt-5.4-mini` → `bzl/gpt-5.4-mini`
- `gpt-5.5`: `cx/gpt-5.5` → `gh/gpt-5.5` → `bzl/gpt-5.5`
- `claude-fable-5`: `cc/claude-fable-5` → `gh/claude-fable-5`
- `claude-haiku-4.5`: `gh/claude-haiku-4.5` → `bzl/claude-haiku-4.5`
- `claude-opus-4-7`: `cc/claude-opus-4-7` → `vertex/claude-opus-4-7`
- `claude-opus-4.7`: `gh/claude-opus-4.7` → `bzl/claude-opus-4.7`
- `claude-sonnet-4.6`: `gh/claude-sonnet-4.6` → `bzl/claude-sonnet-4.6`
- `claude-sonnet-5`: `cc/claude-sonnet-5` → `gh/claude-sonnet-5`
- `gemini-3-flash-preview`: `vertex/gemini-3-flash-preview` →
  `bzl/gemini-3-flash-preview`
- `gemini-3.1-flash-lite`: `vertex/gemini-3.1-flash-lite` →
  `agy/gemini-3.1-flash-lite`
- `gemma-4-31b-it`: `vertex/gemma-4-31b-it` → `bzl/gemma-4-31b-it`
- `glm-5`: `zai/glm-5` → `bzl/glm-5`
- `gpt-5.3-codex`: `cx/gpt-5.3-codex` → `gh/gpt-5.3-codex`
- `kimi-k2.6`: `ollamacloud/kimi-k2.6` → `bzl/kimi-k2.6`
- `mimo-v2.5`: `mimo/mimo-v2.5` → `bzl/mimo-v2.5`
- `mimo-v2.5-pro`: `mimo/mimo-v2.5-pro` → `bzl/mimo-v2.5-pro`
- `minimax-m2.7`: `ollamacloud/minimax-m2.7` → `bzl/minimax-m2.7`
- `minimax-m3`: `ollamacloud/minimax-m3` → `bzl/minimax-m3`
- `openai/gpt-oss-120b`: `nvidia/openai/gpt-oss-120b` →
  `groq/openai/gpt-oss-120b`
- `openai/gpt-oss-20b`: `nvidia/openai/gpt-oss-20b` → `groq/openai/gpt-oss-20b`

---

## Available providers — configured provider prefixes only (from /api/models?all=true)

Configured provider prefixes used for this list:

- `agy`
- `bzl`
- `cc`
- `cerebras`
- `cf`
- `cx`
- `gh`
- `groq`
- `mimo`
- `minimax`
- `mistral`
- `nvidia`
- `oc`
- `ollamacloud`
- `openrouter`
- `vertex`
- `zai`
- `zm`

### agy

- `agy/claude-opus-4-6-thinking`
- `agy/claude-sonnet-4-6`
- `agy/gemini-2.5-flash`
- `agy/gemini-2.5-flash-lite`
- `agy/gemini-2.5-flash-thinking`
- `agy/gemini-2.5-pro`
- `agy/gemini-3.1-flash-image`
- `agy/gemini-3.1-flash-lite`
- `agy/gemini-3.1-pro-high`
- `agy/gemini-3.1-pro-low`
- `agy/gemini-3.5-flash-high`
- `agy/gemini-3.5-flash-low`
- `agy/gemini-3.5-flash-medium`
- `agy/gemini-pro-agent`
- `agy/gpt-oss-120b-medium`

### bzl

- `bzl/auto:free`
- `bzl/claude-haiku-4.5`
- `bzl/claude-opus-4.7`
- `bzl/claude-sonnet-4.6`
- `bzl/deepseek-v3.2`
- `bzl/gemini-3-flash-preview`
- `bzl/gemini-3.1-flash-lite-preview`
- `bzl/gemini-3.1-pro-preview`
- `bzl/gemma-4-26b-a4b-it`
- `bzl/gemma-4-31b-it`
- `bzl/glm-5`
- `bzl/glm-5.1`
- `bzl/gpt-5.4`
- `bzl/gpt-5.4-mini`
- `bzl/gpt-5.4-nano`
- `bzl/gpt-5.5`
- `bzl/grok-4.20`
- `bzl/grok-4.3`
- `bzl/kimi-k2.5`
- `bzl/kimi-k2.6`
- `bzl/llama-3.3-70b-instruct`
- `bzl/llama-4-maverick`
- `bzl/llama-4-scout`
- `bzl/mimo-v2.5`
- `bzl/mimo-v2.5-pro`
- `bzl/minimax-m2.5`
- `bzl/minimax-m2.7`
- `bzl/minimax-m3`
- `bzl/mistral-large-2512`
- `bzl/mistral-medium-3.1`
- `bzl/mistral-small-2603`
- `bzl/nemotron-3-super-120b-a12b`
- `bzl/qwen3.6-plus`

### cc

- `cc/claude-fable-5`
- `cc/claude-haiku-4-5-20251001`
- `cc/claude-opus-4-5-20251101`
- `cc/claude-opus-4-6`
- `cc/claude-opus-4-7`
- `cc/claude-opus-4-8`
- `cc/claude-sonnet-4-5-20250929`
- `cc/claude-sonnet-4-6`
- `cc/claude-sonnet-5`

### cerebras

- `cerebras/gpt-oss-120b`
- `cerebras/zai-glm-4.7`

### cf

- `cf/@cf/deepseek-ai/deepseek-r1-distill-qwen-32b`
- `cf/@cf/google/gemma-3-12b-it`
- `cf/@cf/google/gemma-4-26b-a4b-it`
- `cf/@cf/meta/llama-3.1-8b-instruct`
- `cf/@cf/meta/llama-3.2-3b-instruct`
- `cf/@cf/meta/llama-3.3-70b-instruct`
- `cf/@cf/meta/llama-3.3-70b-instruct-fp8-fast`
- `cf/@cf/mistral/mistral-7b-instruct-v0.2-lora`
- `cf/@cf/moonshotai/kimi-k2.6`
- `cf/@cf/qwen/qwen2.5-coder-15b-instruct`
- `cf/@cf/qwen/qwen2.5-coder-32b-instruct`
- `cf/@cf/qwen/qwq-32b`
- `cf/@cf/zai-org/glm-4.7-flash`

### cx

- `cx/gpt-5.3-codex`
- `cx/gpt-5.3-codex-spark`
- `cx/gpt-5.4`
- `cx/gpt-5.4-high`
- `cx/gpt-5.4-low`
- `cx/gpt-5.4-medium`
- `cx/gpt-5.4-mini`
- `cx/gpt-5.4-xhigh`
- `cx/gpt-5.5`
- `cx/gpt-5.5-high`
- `cx/gpt-5.5-low`
- `cx/gpt-5.5-medium`
- `cx/gpt-5.5-xhigh`

### gh

- `gh/claude-fable-5`
- `gh/claude-haiku-4.5`
- `gh/claude-opus-4.5`
- `gh/claude-opus-4.7`
- `gh/claude-opus-4.8`
- `gh/claude-opus-4.8-fast`
- `gh/claude-sonnet-4.5`
- `gh/claude-sonnet-4.6`
- `gh/claude-sonnet-5`
- `gh/gemini-3.1-pro-preview`
- `gh/gemini-3.5-flash`
- `gh/gpt-4-0125-preview`
- `gh/gpt-4o-2024-11-20`
- `gh/gpt-4o-mini`
- `gh/gpt-5-mini`
- `gh/gpt-5.3-codex`
- `gh/gpt-5.4`
- `gh/gpt-5.4-mini`
- `gh/gpt-5.5`
- `gh/kimi-k2.7-code`
- `gh/mai-code-1-flash`
- `gh/oswe-vscode-prime`

### groq

- `groq/llama-3.3-70b-versatile`
- `groq/meta-llama/llama-4-scout-17b-16e-instruct`
- `groq/openai/gpt-oss-120b`
- `groq/openai/gpt-oss-20b`
- `groq/openai/gpt-oss-safeguard-20b`
- `groq/qwen/qwen3-32b`
- `groq/qwen/qwen3.6-27b`

### mimo

- `mimo/mimo-v2.5`
- `mimo/mimo-v2.5-pro`

### minimax

- `minimax/MiniMax-M2.5`
- `minimax/MiniMax-M2.5-highspeed`
- `minimax/MiniMax-M2.7`
- `minimax/MiniMax-M2.7-highspeed`
- `minimax/MiniMax-M3`

### mistral

- `mistral/codestral-latest`
- `mistral/devstral-latest`
- `mistral/mistral-large-latest`
- `mistral/mistral-medium-3-5`
- `mistral/mistral-small-latest`

### nvidia

- `nvidia/deepseek-ai/deepseek-v4-flash`
- `nvidia/deepseek-ai/deepseek-v4-pro`
- `nvidia/google/gemma-4-31b-it`
- `nvidia/minimaxai/minimax-m2.7`
- `nvidia/mistralai/devstral-2-123b-instruct-2512`
- `nvidia/mistralai/mistral-large-3-675b-instruct-2512`
- `nvidia/mistralai/mistral-small-4-119b-2603`
- `nvidia/moonshotai/kimi-k2.6`
- `nvidia/nvidia/nemotron-3-super-120b-a12b`
- `nvidia/nvidia/nemotron-3-ultra-550b-a55b`
- `nvidia/openai/gpt-oss-120b`
- `nvidia/openai/gpt-oss-20b`
- `nvidia/qwen/qwen3.5-122b-a10b`
- `nvidia/qwen/qwen3.5-397b-a17b`
- `nvidia/stepfun-ai/step-3.5-flash`
- `nvidia/stepfun-ai/step-3.7-flash`
- `nvidia/z-ai/glm-5.2`

### oc

- `oc/big-pickle`
- `oc/deepseek-v4-flash-free`
- `oc/ling-2.6-1t-free`
- `oc/minimax-m2.5-free`
- `oc/minimax-m3-free`
- `oc/nemotron-3-super-free`
- `oc/qwen3.6-plus-free`
- `oc/trinity-large-preview-free`

### ollamacloud

- `ollamacloud/deepseek-v4-flash`
- `ollamacloud/deepseek-v4-pro`
- `ollamacloud/gemma4:31b`
- `ollamacloud/glm-5.1`
- `ollamacloud/kimi-k2.6`
- `ollamacloud/minimax-m2.7`
- `ollamacloud/minimax-m3`
- `ollamacloud/nemotron-3-super`
- `ollamacloud/qwen3.5:397b`

### openrouter

- `openrouter/auto`

### vertex

- `vertex/DeepSeek-V4-Flash`
- `vertex/DeepSeek-V4-Pro`
- `vertex/GLM-5.1-FP8`
- `vertex/Qwen3.6-35B-A3B`
- `vertex/claude-opus-4-7`
- `vertex/claude-sonnet-4-6`
- `vertex/gemini-3-flash-preview`
- `vertex/gemini-3.1-flash-lite`
- `vertex/gemini-3.1-pro-preview`
- `vertex/gemma-4-31b-it`

### zai

- `zai/glm-4.7`
- `zai/glm-4.7-flash`
- `zai/glm-5`
- `zai/glm-5-turbo`
- `zai/glm-5.1`
- `zai/glm-5.2`

### zm

- `zm/anthropic/claude-opus-4.5`
- `zm/anthropic/claude-sonnet-4.5`
- `zm/deepseek/deepseek-chat`
- `zm/google/gemini-3-flash-preview`
- `zm/google/gemini-3.1-pro-preview`
- `zm/mistralai/mistral-large-2512`
- `zm/openai/gpt-5`
- `zm/x-ai/grok-4.1-fast`
- `zm/z-ai/glm-4.6v-flash`
