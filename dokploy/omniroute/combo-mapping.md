# OmniRoute Combo Mapping

## pro-chat (Pro Chat)

- Strategy: `priority`
- Context length: `500000`
- Tags: `chat`, `premium`

1. `cc/claude-opus-4-8`
2. `agy/gemini-3.1-pro-high`
3. `zai/glm-5.2`
4. `cc/claude-sonnet-5`
5. `agy/gemini-3.5-flash-high`
6. `zai/glm-5.1`
7. `cx/gpt-5.4`
8. `bzl/kimi-k2.6`
9. `mistral/mistral-large-latest`

## best-chat (Best Chat)

- Strategy: `weighted`
- Context length: `500000`
- Tags: `chat`, `balanced`, `premium`

1. `cc/claude-sonnet-5`
2. `agy/gemini-3.5-flash-high`
3. `zai/glm-5.1`
4. `cx/gpt-5.4`
5. `bzl/kimi-k2.6`
6. `mistral/mistral-large-latest`

## chat (Chat)

- Strategy: `weighted`
- Context length: `500000`
- Tags: `chat`, `balanced`, `fast`

1. `cx/gpt-5.4`
2. `bzl/kimi-k2.6`
3. `mistral/mistral-large-latest`

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
2. `cx/gpt-5.3-codex-spark`
3. `cx/gpt-5.5-xhigh`
4. `cc/claude-sonnet-5`
5. `cx/gpt-5.5-high`
6. `minimax/MiniMax-M3`
7. `mistral/codestral-latest`

## best-coding (Best Coding)

- Strategy: `weighted`
- Context length: `500000`
- Tags: `coding`, `premium`, `balanced`
- Description: You are an expert coding assistant. Write clean, efficient,
  well-documented code.

1. `cc/claude-sonnet-5`
2. `cx/gpt-5.5-high`
3. `minimax/MiniMax-M3`
4. `mistral/codestral-latest`

## coding (Coding)

- Strategy: `weighted`
- Context length: `500000`
- Tags: `coding`, `balanced`, `fast`, `premium`

1. `minimax/MiniMax-M3`
2. `mistral/codestral-latest`

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

1. `cx/gpt-5.3-codex-spark`
2. `minimax/MiniMax-M3`
3. `agy/gemini-3.5-flash-high`
4. `gh/gemini-3.5-flash`

## best-fast (Best Fast)

- Strategy: `weighted`
- Context length: `500000`
- Tags: `fast`, `fast`, `balanced`

1. `minimax/MiniMax-M3`
2. `agy/gemini-3.5-flash-high`
3. `gh/gemini-3.5-flash`

## fast (Fast)

- Strategy: `weighted`
- Context length: `500000`
- Tags: `fast`, `fast`

1. `agy/gemini-3.5-flash-high`
2. `gh/gemini-3.5-flash`

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
7. `agy/gpt-oss-120b-medium`
8. `agy/gemini-2.5-pro`

## pro-reasoning (Pro Reasoning)

- Strategy: `priority`
- Context length: `500000`
- Tags: `reasoning_deep`, `premium`
- Description: You are a deep reasoning assistant. Think carefully step by step.

1. `cx/gpt-5.5-xhigh`
2. `cc/claude-opus-4-8`
3. `zai/glm-5.2`
4. `cx/gpt-5.5-high`
5. `zai/glm-5.1`
6. `cc/claude-sonnet-5`

## best-reasoning (Best Reasoning)

- Strategy: `weighted`
- Context length: `500000`
- Tags: `reasoning_deep`, `reasoning`, `premium`
- Description: You are a deep reasoning assistant. Think carefully step by step.

1. `zai/glm-5.2`
2. `cx/gpt-5.5-high`
3. `zai/glm-5.1`
4. `cc/claude-sonnet-5`

## reasoning (Reasoning)

- Strategy: `weighted`
- Context length: `500000`
- Tags: `reasoning`, `balanced`

1. `cx/gpt-5.5-high`
2. `zai/glm-5.1`
3. `cc/claude-sonnet-5`

## pro-vision (Pro Vision)

- Strategy: `priority`
- Context length: `500000`
- Tags: `vision`, `premium`

1. `cc/claude-sonnet-5`
2. `vertex/gemini-3.1-pro-preview`
3. `agy/gemini-3.5-flash-high`
4. `cx/gpt-5.5-high`

## best-vision (Best Vision)

- Strategy: `weighted`
- Context length: `500000`
- Tags: `vision`, `premium`, `balanced`

1. `vertex/gemini-3.1-pro-preview`
2. `agy/gemini-3.5-flash-high`
3. `cx/gpt-5.5-high`

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

## Available providers — all imported models per provider connection (from `GET /api/providers/{id}/models`, not the routable `/api/models?all=true` catalog subset)

Configured provider prefixes with imported models (`oc` = OpenCode Free, distinct from `ollamacloud` = Ollama Cloud paid tier):

- `agy`
- `bzl`
- `cc`
- `cerebras`
- `cf`
- `cx`
- `fmd`
- `gh`
- `groq`
- `kiro`
- `mcode`
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
- `agy/gemini-3.5-flash-low`
- `agy/gemini-pro-agent`
- `agy/gpt-oss-120b-medium`

### bzl

- `bzl/Z-Image-Turbo`
- `bzl/aion-1.0`
- `bzl/aion-1.0-mini`
- `bzl/aion-2.0`
- `bzl/aion-rp-llama-3.1-8b`
- `bzl/all-minilm-l12-v2`
- `bzl/all-minilm-l6-v2`
- `bzl/all-mpnet-base-v2`
- `bzl/bge-base-en-v1.5`
- `bzl/bge-large-en-v1.5`
- `bzl/bge-m3`
- `bzl/claude-3-haiku`
- `bzl/claude-fable-5`
- `bzl/claude-haiku-4.5`
- `bzl/claude-opus-4`
- `bzl/claude-opus-4.1`
- `bzl/claude-opus-4.5`
- `bzl/claude-opus-4.6`
- `bzl/claude-opus-4.7`
- `bzl/claude-opus-4.8`
- `bzl/claude-sonnet-4`
- `bzl/claude-sonnet-4.5`
- `bzl/claude-sonnet-4.6`
- `bzl/claude-sonnet-5`
- `bzl/codestral-2508`
- `bzl/codestral-embed-2505`
- `bzl/cogito-v2.1-671b`
- `bzl/command-a`
- `bzl/command-r-08-2024`
- `bzl/command-r-plus-08-2024`
- `bzl/command-r7b-12-2024`
- `bzl/cydonia-24b-v4.1`
- `bzl/deepseek-chat`
- `bzl/deepseek-chat-v3-0324`
- `bzl/deepseek-chat-v3.1`
- `bzl/deepseek-r1`
- `bzl/deepseek-r1-0528`
- `bzl/deepseek-r1-distill-llama-70b`
- `bzl/deepseek-v3.1-terminus`
- `bzl/deepseek-v3.2`
- `bzl/deepseek-v3.2-exp`
- `bzl/deepseek-v4-flash`
- `bzl/deepseek-v4-pro`
- `bzl/devstral-2512`
- `bzl/e5-base-v2`
- `bzl/e5-large-v2`
- `bzl/ernie-4.5-vl-424b-a47b`
- `bzl/fugu-ultra`
- `bzl/gemini-2.5-flash`
- `bzl/gemini-2.5-flash-image`
- `bzl/gemini-2.5-flash-lite`
- `bzl/gemini-2.5-flash-lite-preview-09-2025`
- `bzl/gemini-2.5-pro`
- `bzl/gemini-2.5-pro-preview`
- `bzl/gemini-2.5-pro-preview-05-06`
- `bzl/gemini-3-flash-preview`
- `bzl/gemini-3-pro-image`
- `bzl/gemini-3.1-flash-image`
- `bzl/gemini-3.1-flash-image-preview`
- `bzl/gemini-3.1-flash-lite-preview`
- `bzl/gemini-3.1-pro-preview`
- `bzl/gemini-3.1-pro-preview-customtools`
- `bzl/gemini-3.5-flash`
- `bzl/gemini-embedding-001`
- `bzl/gemini-embedding-2-preview`
- `bzl/gemma-2-27b-it`
- `bzl/gemma-3-12b-it`
- `bzl/gemma-3-27b-it`
- `bzl/gemma-3-4b-it`
- `bzl/gemma-3n-e4b-it`
- `bzl/gemma-4-26b-a4b-it`
- `bzl/gemma-4-31b-it`
- `bzl/glm-4.5`
- `bzl/glm-4.5-air`
- `bzl/glm-4.5v`
- `bzl/glm-4.6`
- `bzl/glm-4.6v`
- `bzl/glm-4.7`
- `bzl/glm-4.7-flash`
- `bzl/glm-5`
- `bzl/glm-5-turbo`
- `bzl/glm-5.1`
- `bzl/glm-5.2`
- `bzl/glm-5v-turbo`
- `bzl/gpt-3.5-turbo`
- `bzl/gpt-3.5-turbo-0613`
- `bzl/gpt-3.5-turbo-16k`
- `bzl/gpt-3.5-turbo-instruct`
- `bzl/gpt-4`
- `bzl/gpt-4-turbo`
- `bzl/gpt-4-turbo-preview`
- `bzl/gpt-4.1`
- `bzl/gpt-4.1-mini`
- `bzl/gpt-4.1-nano`
- `bzl/gpt-4o`
- `bzl/gpt-4o-2024-05-13`
- `bzl/gpt-4o-2024-08-06`
- `bzl/gpt-4o-2024-11-20`
- `bzl/gpt-4o-mini`
- `bzl/gpt-4o-mini-2024-07-18`
- `bzl/gpt-4o-mini-search-preview`
- `bzl/gpt-4o-search-preview`
- `bzl/gpt-5`
- `bzl/gpt-5-chat`
- `bzl/gpt-5-codex`
- `bzl/gpt-5-mini`
- `bzl/gpt-5-nano`
- `bzl/gpt-5-pro`
- `bzl/gpt-5.1`
- `bzl/gpt-5.1-chat`
- `bzl/gpt-5.1-codex`
- `bzl/gpt-5.1-codex-max`
- `bzl/gpt-5.1-codex-mini`
- `bzl/gpt-5.2`
- `bzl/gpt-5.2-chat`
- `bzl/gpt-5.2-codex`
- `bzl/gpt-5.2-pro`
- `bzl/gpt-5.3-chat`
- `bzl/gpt-5.3-codex`
- `bzl/gpt-5.4`
- `bzl/gpt-5.4-image-2`
- `bzl/gpt-5.4-mini`
- `bzl/gpt-5.4-nano`
- `bzl/gpt-5.4-pro`
- `bzl/gpt-5.5`
- `bzl/gpt-oss-120b`
- `bzl/gpt-oss-20b`
- `bzl/gpt-oss-safeguard-20b`
- `bzl/granite-4.0-h-micro`
- `bzl/grok-4.20`
- `bzl/grok-4.20-multi-agent`
- `bzl/grok-4.3`
- `bzl/grok-build-0.1`
- `bzl/gte-base`
- `bzl/gte-large`
- `bzl/happyhorse-1.0`
- `bzl/happyhorse-1.1`
- `bzl/hermes-3-llama-3.1-405b`
- `bzl/hermes-3-llama-3.1-70b`
- `bzl/hermes-4-405b`
- `bzl/hermes-4-70b`
- `bzl/hunyuan-a13b-instruct`
- `bzl/inflection-3-pi`
- `bzl/inflection-3-productivity`
- `bzl/jamba-large-1.7`
- `bzl/kat-coder-pro-v2`
- `bzl/kimi-k2`
- `bzl/kimi-k2-0905`
- `bzl/kimi-k2-thinking`
- `bzl/kimi-k2.5`
- `bzl/kimi-k2.6`
- `bzl/l3-lunaris-8b`
- `bzl/l3.1-70b-hanami-x1`
- `bzl/l3.1-euryale-70b`
- `bzl/l3.3-euryale-70b`
- `bzl/lfm-2-24b-a2b`
- `bzl/llama-3-8b-instruct`
- `bzl/llama-3.1-70b-instruct`
- `bzl/llama-3.1-8b-instruct`
- `bzl/llama-3.2-11b-vision-instruct`
- `bzl/llama-3.2-1b-instruct`
- `bzl/llama-3.2-3b-instruct`
- `bzl/llama-3.3-70b-instruct`
- `bzl/llama-3.3-nemotron-super-49b-v1.5`
- `bzl/llama-4-maverick`
- `bzl/llama-4-scout`
- `bzl/llama-guard-4-12b`
- `bzl/magnum-v4-72b`
- `bzl/mercury-2`
- `bzl/mimo-v2.5`
- `bzl/mimo-v2.5-pro`
- `bzl/minimax-01`
- `bzl/minimax-m1`
- `bzl/minimax-m2`
- `bzl/minimax-m2-her`
- `bzl/minimax-m2.1`
- `bzl/minimax-m2.5`
- `bzl/minimax-m2.7`
- `bzl/minimax-m3`
- `bzl/ministral-14b-2512`
- `bzl/ministral-3b-2512`
- `bzl/ministral-8b-2512`
- `bzl/mistral-embed-2312`
- `bzl/mistral-large`
- `bzl/mistral-large-2407`
- `bzl/mistral-large-2512`
- `bzl/mistral-medium-3`
- `bzl/mistral-medium-3.1`
- `bzl/mistral-nemo`
- `bzl/mistral-saba`
- `bzl/mistral-small-24b-instruct-2501`
- `bzl/mistral-small-2603`
- `bzl/mistral-small-3.1-24b-instruct`
- `bzl/mistral-small-3.2-24b-instruct`
- `bzl/mixtral-8x22b-instruct`
- `bzl/morph-v3-fast`
- `bzl/morph-v3-large`
- `bzl/multi-qa-mpnet-base-dot-v1`
- `bzl/multilingual-e5-large`
- `bzl/mythomax-l2-13b`
- `bzl/nemotron-3-nano-30b-a3b`
- `bzl/nemotron-3-super-120b-a12b`
- `bzl/nova-2-lite-v1`
- `bzl/nova-lite-v1`
- `bzl/nova-micro-v1`
- `bzl/nova-premier-v1`
- `bzl/nova-pro-v1`
- `bzl/o1`
- `bzl/o1-pro`
- `bzl/o3`
- `bzl/o3-deep-research`
- `bzl/o3-mini`
- `bzl/o3-mini-high`
- `bzl/o3-pro`
- `bzl/o4-mini`
- `bzl/o4-mini-deep-research`
- `bzl/o4-mini-high`
- `bzl/olmo-3-32b-think`
- `bzl/palmyra-x5`
- `bzl/paraphrase-minilm-l6-v2`
- `bzl/phi-4`
- `bzl/pplx-embed-v1-4b`
- `bzl/qwen-2.5-72b-instruct`
- `bzl/qwen-2.5-7b-instruct`
- `bzl/qwen-2.5-coder-32b-instruct`
- `bzl/qwen-plus`
- `bzl/qwen-plus-2025-07-28`
- `bzl/qwen-plus-2025-07-28:thinking`
- `bzl/qwen2.5-vl-72b-instruct`
- `bzl/qwen3-14b`
- `bzl/qwen3-235b-a22b`
- `bzl/qwen3-235b-a22b-2507`
- `bzl/qwen3-235b-a22b-thinking-2507`
- `bzl/qwen3-30b-a3b`
- `bzl/qwen3-30b-a3b-instruct-2507`
- `bzl/qwen3-30b-a3b-thinking-2507`
- `bzl/qwen3-32b`
- `bzl/qwen3-8b`
- `bzl/qwen3-coder`
- `bzl/qwen3-coder-30b-a3b-instruct`
- `bzl/qwen3-coder-flash`
- `bzl/qwen3-coder-next`
- `bzl/qwen3-coder-plus`
- `bzl/qwen3-embedding-4b`
- `bzl/qwen3-embedding-8b`
- `bzl/qwen3-max`
- `bzl/qwen3-max-thinking`
- `bzl/qwen3-next-80b-a3b-instruct`
- `bzl/qwen3-next-80b-a3b-thinking`
- `bzl/qwen3-vl-235b-a22b-instruct`
- `bzl/qwen3-vl-235b-a22b-thinking`
- `bzl/qwen3-vl-30b-a3b-instruct`
- `bzl/qwen3-vl-30b-a3b-thinking`
- `bzl/qwen3-vl-32b-instruct`
- `bzl/qwen3-vl-8b-instruct`
- `bzl/qwen3-vl-8b-thinking`
- `bzl/qwen3.5-122b-a10b`
- `bzl/qwen3.5-27b`
- `bzl/qwen3.5-35b-a3b`
- `bzl/qwen3.5-397b-a17b`
- `bzl/qwen3.5-9b`
- `bzl/qwen3.5-flash-02-23`
- `bzl/qwen3.5-plus-02-15`
- `bzl/qwen3.5-plus-20260420`
- `bzl/qwen3.6-27b`
- `bzl/qwen3.6-35b-a3b`
- `bzl/qwen3.6-plus`
- `bzl/qwen3.7-max`
- `bzl/reka-edge`
- `bzl/reka-flash-3`
- `bzl/relace-search`
- `bzl/remm-slerp-l2-13b`
- `bzl/rocinante-12b`
- `bzl/router`
- `bzl/seed-1.6`
- `bzl/seed-1.6-flash`
- `bzl/seed-2.0-lite`
- `bzl/seed-2.0-mini`
- `bzl/skyfall-36b-v2`
- `bzl/solar-pro-3`
- `bzl/sonar`
- `bzl/sonar-deep-research`
- `bzl/sonar-pro`
- `bzl/sonar-pro-search`
- `bzl/sonar-reasoning-pro`
- `bzl/step-3.5-flash`
- `bzl/text-embedding-3-large`
- `bzl/text-embedding-3-small`
- `bzl/text-embedding-ada-002`
- `bzl/trinity-large-thinking`
- `bzl/trinity-mini`
- `bzl/ui-tars-1.5-7b`
- `bzl/unslopnemo-12b`
- `bzl/voxtral-small-24b-2507`
- `bzl/weaver`
- `bzl/wizardlm-2-8x22b`

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

- `cerebras/gemma-4-31b`
- `cerebras/gpt-oss-120b`
- `cerebras/zai-glm-4.7`

### cf

- `cf/@cf/ai4bharat/indictrans2-en-indic-1B`
- `cf/@cf/aisingapore/gemma-sea-lion-v4-27b-it`
- `cf/@cf/baai/bge-base-en-v1.5`
- `cf/@cf/baai/bge-large-en-v1.5`
- `cf/@cf/baai/bge-m3`
- `cf/@cf/baai/bge-reranker-base`
- `cf/@cf/baai/bge-small-en-v1.5`
- `cf/@cf/black-forest-labs/flux-1-schnell`
- `cf/@cf/black-forest-labs/flux-2-dev`
- `cf/@cf/black-forest-labs/flux-2-klein-4b`
- `cf/@cf/black-forest-labs/flux-2-klein-9b`
- `cf/@cf/bytedance/stable-diffusion-xl-lightning`
- `cf/@cf/deepgram/aura-1`
- `cf/@cf/deepgram/aura-2-en`
- `cf/@cf/deepgram/aura-2-es`
- `cf/@cf/deepgram/flux`
- `cf/@cf/deepgram/nova-3`
- `cf/@cf/deepseek-ai/deepseek-r1-distill-qwen-32b`
- `cf/@cf/google/embeddinggemma-300m`
- `cf/@cf/google/gemma-2b-it-lora`
- `cf/@cf/google/gemma-4-26b-a4b-it`
- `cf/@cf/google/gemma-7b-it-lora`
- `cf/@cf/huggingface/distilbert-sst-2-int8`
- `cf/@cf/ibm-granite/granite-4.0-h-micro`
- `cf/@cf/leonardo/lucid-origin`
- `cf/@cf/leonardo/phoenix-1.0`
- `cf/@cf/llava-hf/llava-1.5-7b-hf`
- `cf/@cf/lykon/dreamshaper-8-lcm`
- `cf/@cf/meta-llama/llama-2-7b-chat-hf-lora`
- `cf/@cf/meta/llama-3.1-8b-instruct-fp8`
- `cf/@cf/meta/llama-3.2-11b-vision-instruct`
- `cf/@cf/meta/llama-3.2-1b-instruct`
- `cf/@cf/meta/llama-3.2-3b-instruct`
- `cf/@cf/meta/llama-3.3-70b-instruct-fp8-fast`
- `cf/@cf/meta/llama-4-scout-17b-16e-instruct`
- `cf/@cf/meta/llama-guard-3-8b`
- `cf/@cf/meta/m2m100-1.2b`
- `cf/@cf/microsoft/resnet-50`
- `cf/@cf/mistral/mistral-7b-instruct-v0.2-lora`
- `cf/@cf/mistralai/mistral-small-3.1-24b-instruct`
- `cf/@cf/moondream/moondream3.1-9B-A2B`
- `cf/@cf/moonshotai/kimi-k2.6`
- `cf/@cf/moonshotai/kimi-k2.7-code`
- `cf/@cf/myshell-ai/melotts`
- `cf/@cf/nvidia/nemotron-3-120b-a12b`
- `cf/@cf/openai/gpt-oss-120b`
- `cf/@cf/openai/gpt-oss-20b`
- `cf/@cf/openai/whisper`
- `cf/@cf/openai/whisper-large-v3-turbo`
- `cf/@cf/openai/whisper-tiny-en`
- `cf/@cf/pfnet/plamo-embedding-1b`
- `cf/@cf/pipecat-ai/smart-turn-v2`
- `cf/@cf/qwen/qwen2.5-coder-32b-instruct`
- `cf/@cf/qwen/qwen3-30b-a3b-fp8`
- `cf/@cf/qwen/qwen3-embedding-0.6b`
- `cf/@cf/qwen/qwq-32b`
- `cf/@cf/runwayml/stable-diffusion-v1-5-img2img`
- `cf/@cf/runwayml/stable-diffusion-v1-5-inpainting`
- `cf/@cf/stabilityai/stable-diffusion-xl-base-1.0`
- `cf/@cf/zai-org/glm-4.7-flash`
- `cf/@cf/zai-org/glm-5.2`

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

### fmd

- `fmd/gpt-5.3-codex`
- `fmd/gpt-5.4`
- `fmd/gpt-5.4-mini`
- `fmd/gpt-5.5`

### gh

- `gh/claude-haiku-4.5`
- `gh/claude-opus-4.5`
- `gh/claude-opus-4.7`
- `gh/claude-opus-4.8`
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

### groq

- `groq/allam-2-7b`
- `groq/canopylabs/orpheus-arabic-saudi`
- `groq/canopylabs/orpheus-v1-english`
- `groq/groq/compound`
- `groq/groq/compound-mini`
- `groq/llama-3.1-8b-instant`
- `groq/llama-3.3-70b-versatile`
- `groq/meta-llama/llama-4-scout-17b-16e-instruct`
- `groq/meta-llama/llama-prompt-guard-2-22m`
- `groq/meta-llama/llama-prompt-guard-2-86m`
- `groq/openai/gpt-oss-120b`
- `groq/openai/gpt-oss-20b`
- `groq/openai/gpt-oss-safeguard-20b`
- `groq/qwen/qwen3-32b`
- `groq/qwen/qwen3.6-27b`
- `groq/whisper-large-v3`
- `groq/whisper-large-v3-turbo`

### kiro

- `kiro/claude-haiku-4.5`
- `kiro/claude-sonnet-4.5`
- `kiro/claude-sonnet-5`
- `kiro/deepseek-3.2`
- `kiro/glm-5`
- `kiro/minimax-m2.1`
- `kiro/minimax-m2.5`
- `kiro/qwen3-coder-next`

### mcode

- `mcode/mimo-auto`

### minimax

- `minimax/MiniMax-M2`
- `minimax/MiniMax-M2.1`
- `minimax/MiniMax-M2.1-highspeed`
- `minimax/MiniMax-M2.5`
- `minimax/MiniMax-M2.5-highspeed`
- `minimax/MiniMax-M2.7`
- `minimax/MiniMax-M2.7-highspeed`
- `minimax/MiniMax-M3`

### mistral

- `mistral/codestral-2508`
- `mistral/codestral-embed`
- `mistral/codestral-embed-2505`
- `mistral/codestral-latest`
- `mistral/devstral-2512`
- `mistral/devstral-latest`
- `mistral/devstral-medium-latest`
- `mistral/labs-leanstral-1-5`
- `mistral/labs-leanstral-1-5-1`
- `mistral/magistral-medium-2509`
- `mistral/magistral-medium-latest`
- `mistral/magistral-small-2509`
- `mistral/magistral-small-latest`
- `mistral/ministral-14b-2512`
- `mistral/ministral-14b-latest`
- `mistral/ministral-3b-2512`
- `mistral/ministral-3b-latest`
- `mistral/ministral-8b-2512`
- `mistral/ministral-8b-latest`
- `mistral/mistral-code-agent-latest`
- `mistral/mistral-code-fim-latest`
- `mistral/mistral-code-latest`
- `mistral/mistral-embed`
- `mistral/mistral-embed-2312`
- `mistral/mistral-large-2512`
- `mistral/mistral-large-latest`
- `mistral/mistral-medium`
- `mistral/mistral-medium-2505`
- `mistral/mistral-medium-2508`
- `mistral/mistral-medium-2604`
- `mistral/mistral-medium-3`
- `mistral/mistral-medium-3-5`
- `mistral/mistral-medium-3.5`
- `mistral/mistral-medium-latest`
- `mistral/mistral-moderation-2603`
- `mistral/mistral-ocr-2512`
- `mistral/mistral-ocr-3`
- `mistral/mistral-ocr-3-0`
- `mistral/mistral-ocr-4`
- `mistral/mistral-ocr-4-0`
- `mistral/mistral-ocr-latest`
- `mistral/mistral-small-2506`
- `mistral/mistral-small-2603`
- `mistral/mistral-small-latest`
- `mistral/mistral-tiny-2407`
- `mistral/mistral-tiny-latest`
- `mistral/mistral-vibe-cli-fast`
- `mistral/mistral-vibe-cli-latest`
- `mistral/mistral-vibe-cli-with-tools`
- `mistral/open-mistral-nemo`
- `mistral/open-mistral-nemo-2407`
- `mistral/voxtral-mini-2507`
- `mistral/voxtral-mini-2602`
- `mistral/voxtral-mini-latest`
- `mistral/voxtral-mini-realtime-2602`
- `mistral/voxtral-mini-realtime-latest`
- `mistral/voxtral-mini-transcribe-realtime-2602`
- `mistral/voxtral-mini-tts-2603`
- `mistral/voxtral-mini-tts-latest`
- `mistral/voxtral-small-2507`
- `mistral/voxtral-small-latest`

### nvidia

- `nvidia/01-ai/yi-large`
- `nvidia/abacusai/dracarys-llama-3.1-70b-instruct`
- `nvidia/adept/fuyu-8b`
- `nvidia/ai21labs/jamba-1.5-large-instruct`
- `nvidia/aisingapore/sea-lion-7b-instruct`
- `nvidia/baai/bge-m3`
- `nvidia/bigcode/starcoder2-15b`
- `nvidia/bytedance/seed-oss-36b-instruct`
- `nvidia/databricks/dbrx-instruct`
- `nvidia/deepseek-ai/deepseek-coder-6.7b-instruct`
- `nvidia/deepseek-ai/deepseek-v4-flash`
- `nvidia/deepseek-ai/deepseek-v4-pro`
- `nvidia/google/codegemma-1.1-7b`
- `nvidia/google/codegemma-7b`
- `nvidia/google/deplot`
- `nvidia/google/diffusiongemma-26b-a4b-it`
- `nvidia/google/gemma-2-2b-it`
- `nvidia/google/gemma-2b`
- `nvidia/google/gemma-3-12b-it`
- `nvidia/google/gemma-3-4b-it`
- `nvidia/google/gemma-3n-e2b-it`
- `nvidia/google/gemma-3n-e4b-it`
- `nvidia/google/gemma-4-31b-it`
- `nvidia/google/recurrentgemma-2b`
- `nvidia/ibm/granite-3.0-3b-a800m-instruct`
- `nvidia/ibm/granite-3.0-8b-instruct`
- `nvidia/ibm/granite-34b-code-instruct`
- `nvidia/ibm/granite-8b-code-instruct`
- `nvidia/meta/codellama-70b`
- `nvidia/meta/llama-3.1-70b-instruct`
- `nvidia/meta/llama-3.1-8b-instruct`
- `nvidia/meta/llama-3.2-11b-vision-instruct`
- `nvidia/meta/llama-3.2-1b-instruct`
- `nvidia/meta/llama-3.2-3b-instruct`
- `nvidia/meta/llama-3.2-90b-vision-instruct`
- `nvidia/meta/llama-3.3-70b-instruct`
- `nvidia/meta/llama-4-maverick-17b-128e-instruct`
- `nvidia/meta/llama-guard-4-12b`
- `nvidia/meta/llama2-70b`
- `nvidia/microsoft/kosmos-2`
- `nvidia/microsoft/phi-3-vision-128k-instruct`
- `nvidia/microsoft/phi-3.5-moe-instruct`
- `nvidia/microsoft/phi-4-mini-instruct`
- `nvidia/microsoft/phi-4-multimodal-instruct`
- `nvidia/minimaxai/minimax-m2.7`
- `nvidia/minimaxai/minimax-m3`
- `nvidia/mistralai/codestral-22b-instruct-v0.1`
- `nvidia/mistralai/ministral-14b-instruct-2512`
- `nvidia/mistralai/mistral-7b-instruct-v0.3`
- `nvidia/mistralai/mistral-large`
- `nvidia/mistralai/mistral-large-2-instruct`
- `nvidia/mistralai/mistral-large-3-675b-instruct-2512`
- `nvidia/mistralai/mistral-medium-3.5-128b`
- `nvidia/mistralai/mistral-nemotron`
- `nvidia/mistralai/mistral-small-4-119b-2603`
- `nvidia/mistralai/mixtral-8x22b-v0.1`
- `nvidia/mistralai/mixtral-8x7b-instruct-v0.1`
- `nvidia/moonshotai/kimi-k2.6`
- `nvidia/nv-mistralai/mistral-nemo-12b-instruct`
- `nvidia/nvidia/ai-synthetic-video-detector`
- `nvidia/nvidia/cosmos-reason2-8b`
- `nvidia/nvidia/embed-qa-4`
- `nvidia/nvidia/gliner-pii`
- `nvidia/nvidia/ising-calibration-1-35b-a3b`
- `nvidia/nvidia/llama-3.1-nemoguard-8b-content-safety`
- `nvidia/nvidia/llama-3.1-nemoguard-8b-topic-control`
- `nvidia/nvidia/llama-3.1-nemotron-51b-instruct`
- `nvidia/nvidia/llama-3.1-nemotron-70b-instruct`
- `nvidia/nvidia/llama-3.1-nemotron-nano-8b-v1`
- `nvidia/nvidia/llama-3.1-nemotron-nano-vl-8b-v1`
- `nvidia/nvidia/llama-3.1-nemotron-safety-guard-8b-v3`
- `nvidia/nvidia/llama-3.1-nemotron-ultra-253b-v1`
- `nvidia/nvidia/llama-3.2-nemoretriever-1b-vlm-embed-v1`
- `nvidia/nvidia/llama-3.2-nv-embedqa-1b-v1`
- `nvidia/nvidia/llama-3.3-nemotron-super-49b-v1`
- `nvidia/nvidia/llama-3.3-nemotron-super-49b-v1.5`
- `nvidia/nvidia/llama-nemotron-embed-1b-v2`
- `nvidia/nvidia/llama-nemotron-embed-vl-1b-v2`
- `nvidia/nvidia/llama3-chatqa-1.5-70b`
- `nvidia/nvidia/mistral-nemo-minitron-8b-8k-instruct`
- `nvidia/nvidia/nemoretriever-parse`
- `nvidia/nvidia/nemotron-3-content-safety`
- `nvidia/nvidia/nemotron-3-nano-30b-a3b`
- `nvidia/nvidia/nemotron-3-nano-omni-30b-a3b-reasoning`
- `nvidia/nvidia/nemotron-3-super-120b-a12b`
- `nvidia/nvidia/nemotron-3-ultra-550b-a55b`
- `nvidia/nvidia/nemotron-3.5-content-safety`
- `nvidia/nvidia/nemotron-4-340b-instruct`
- `nvidia/nvidia/nemotron-4-340b-reward`
- `nvidia/nvidia/nemotron-content-safety-reasoning-4b`
- `nvidia/nvidia/nemotron-mini-4b-instruct`
- `nvidia/nvidia/nemotron-nano-12b-v2-vl`
- `nvidia/nvidia/nemotron-nano-3-30b-a3b`
- `nvidia/nvidia/nemotron-parse`
- `nvidia/nvidia/neva-22b`
- `nvidia/nvidia/nv-embed-v1`
- `nvidia/nvidia/nv-embedcode-7b-v1`
- `nvidia/nvidia/nv-embedqa-e5-v5`
- `nvidia/nvidia/nv-embedqa-mistral-7b-v2`
- `nvidia/nvidia/nvclip`
- `nvidia/nvidia/nvidia-nemotron-nano-9b-v2`
- `nvidia/nvidia/riva-translate-4b-instruct`
- `nvidia/nvidia/riva-translate-4b-instruct-v1.1`
- `nvidia/nvidia/vila`
- `nvidia/openai/gpt-oss-120b`
- `nvidia/openai/gpt-oss-20b`
- `nvidia/qwen/qwen3-next-80b-a3b-instruct`
- `nvidia/qwen/qwen3.5-122b-a10b`
- `nvidia/qwen/qwen3.5-397b-a17b`
- `nvidia/sarvamai/sarvam-m`
- `nvidia/snowflake/arctic-embed-l`
- `nvidia/stepfun-ai/step-3.5-flash`
- `nvidia/stepfun-ai/step-3.7-flash`
- `nvidia/stockmark/stockmark-2-100b-instruct`
- `nvidia/upstage/solar-10.7b-instruct`
- `nvidia/writer/palmyra-creative-122b`
- `nvidia/writer/palmyra-fin-70b-32k`
- `nvidia/writer/palmyra-med-70b`
- `nvidia/writer/palmyra-med-70b-32k`
- `nvidia/z-ai/glm-5.2`
- `nvidia/zyphra/zamba2-7b-instruct`

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

- `ollamacloud/deepseek-v3.1:671b`
- `ollamacloud/deepseek-v3.2`
- `ollamacloud/deepseek-v4-flash`
- `ollamacloud/deepseek-v4-pro`
- `ollamacloud/devstral-2:123b`
- `ollamacloud/devstral-small-2:24b`
- `ollamacloud/gemini-3-flash-preview`
- `ollamacloud/gemma3:12b`
- `ollamacloud/gemma3:27b`
- `ollamacloud/gemma3:4b`
- `ollamacloud/gemma4:31b`
- `ollamacloud/glm-4.7`
- `ollamacloud/glm-5`
- `ollamacloud/glm-5.1`
- `ollamacloud/glm-5.2`
- `ollamacloud/gpt-oss:120b`
- `ollamacloud/gpt-oss:20b`
- `ollamacloud/kimi-k2.5`
- `ollamacloud/kimi-k2.6`
- `ollamacloud/kimi-k2.7-code`
- `ollamacloud/minimax-m2.1`
- `ollamacloud/minimax-m2.5`
- `ollamacloud/minimax-m2.7`
- `ollamacloud/minimax-m3`
- `ollamacloud/ministral-3:14b`
- `ollamacloud/ministral-3:3b`
- `ollamacloud/ministral-3:8b`
- `ollamacloud/mistral-large-3:675b`
- `ollamacloud/nemotron-3-nano:30b`
- `ollamacloud/nemotron-3-super`
- `ollamacloud/nemotron-3-ultra`
- `ollamacloud/qwen3-coder-next`
- `ollamacloud/qwen3-coder:480b`
- `ollamacloud/qwen3.5:397b`

### openrouter

- `openrouter/ai21/jamba-large-1.7`
- `openrouter/aion-labs/aion-2.0`
- `openrouter/aion-labs/aion-3.0`
- `openrouter/aion-labs/aion-3.0-mini`
- `openrouter/aion-labs/aion-rp-llama-3.1-8b`
- `openrouter/allenai/olmo-3-32b-think`
- `openrouter/amazon/nova-2-lite-v1`
- `openrouter/amazon/nova-lite-v1`
- `openrouter/amazon/nova-micro-v1`
- `openrouter/amazon/nova-premier-v1`
- `openrouter/amazon/nova-pro-v1`
- `openrouter/anthracite-org/magnum-v4-72b`
- `openrouter/anthropic/claude-3-haiku`
- `openrouter/anthropic/claude-fable-5`
- `openrouter/anthropic/claude-haiku-4.5`
- `openrouter/anthropic/claude-opus-4`
- `openrouter/anthropic/claude-opus-4.1`
- `openrouter/anthropic/claude-opus-4.5`
- `openrouter/anthropic/claude-opus-4.6`
- `openrouter/anthropic/claude-opus-4.7`
- `openrouter/anthropic/claude-opus-4.7-fast`
- `openrouter/anthropic/claude-opus-4.8`
- `openrouter/anthropic/claude-opus-4.8-fast`
- `openrouter/anthropic/claude-sonnet-4`
- `openrouter/anthropic/claude-sonnet-4.5`
- `openrouter/anthropic/claude-sonnet-4.6`
- `openrouter/anthropic/claude-sonnet-5`
- `openrouter/arcee-ai/coder-large`
- `openrouter/arcee-ai/trinity-large-thinking`
- `openrouter/arcee-ai/trinity-mini`
- `openrouter/arcee-ai/virtuoso-large`
- `openrouter/baidu/ernie-4.5-vl-424b-a47b`
- `openrouter/bytedance-seed/seed-1.6`
- `openrouter/bytedance-seed/seed-1.6-flash`
- `openrouter/bytedance-seed/seed-2.0-lite`
- `openrouter/bytedance-seed/seed-2.0-mini`
- `openrouter/bytedance/ui-tars-1.5-7b`
- `openrouter/cognitivecomputations/dolphin-mistral-24b-venice-edition:free`
- `openrouter/cohere/command-a`
- `openrouter/cohere/command-r-08-2024`
- `openrouter/cohere/command-r-plus-08-2024`
- `openrouter/cohere/command-r7b-12-2024`
- `openrouter/cohere/north-mini-code:free`
- `openrouter/deepcogito/cogito-v2.1-671b`
- `openrouter/deepseek/deepseek-chat`
- `openrouter/deepseek/deepseek-chat-v3-0324`
- `openrouter/deepseek/deepseek-chat-v3.1`
- `openrouter/deepseek/deepseek-r1`
- `openrouter/deepseek/deepseek-r1-0528`
- `openrouter/deepseek/deepseek-r1-distill-llama-70b`
- `openrouter/deepseek/deepseek-v3.1-terminus`
- `openrouter/deepseek/deepseek-v3.2`
- `openrouter/deepseek/deepseek-v3.2-exp`
- `openrouter/deepseek/deepseek-v4-flash`
- `openrouter/deepseek/deepseek-v4-pro`
- `openrouter/google/gemini-2.5-flash`
- `openrouter/google/gemini-2.5-flash-image`
- `openrouter/google/gemini-2.5-flash-lite`
- `openrouter/google/gemini-2.5-flash-lite-preview-09-2025`
- `openrouter/google/gemini-2.5-pro`
- `openrouter/google/gemini-2.5-pro-preview`
- `openrouter/google/gemini-2.5-pro-preview-05-06`
- `openrouter/google/gemini-3-flash-preview`
- `openrouter/google/gemini-3-pro-image`
- `openrouter/google/gemini-3-pro-image-preview`
- `openrouter/google/gemini-3.1-flash-image`
- `openrouter/google/gemini-3.1-flash-image-preview`
- `openrouter/google/gemini-3.1-flash-lite`
- `openrouter/google/gemini-3.1-flash-lite-image`
- `openrouter/google/gemini-3.1-flash-lite-preview`
- `openrouter/google/gemini-3.1-pro-preview`
- `openrouter/google/gemini-3.1-pro-preview-customtools`
- `openrouter/google/gemini-3.5-flash`
- `openrouter/google/gemma-2-27b-it`
- `openrouter/google/gemma-3-12b-it`
- `openrouter/google/gemma-3-27b-it`
- `openrouter/google/gemma-3-4b-it`
- `openrouter/google/gemma-3n-e4b-it`
- `openrouter/google/gemma-4-26b-a4b-it`
- `openrouter/google/gemma-4-26b-a4b-it:free`
- `openrouter/google/gemma-4-31b-it`
- `openrouter/google/gemma-4-31b-it:free`
- `openrouter/google/lyria-3-clip-preview`
- `openrouter/google/lyria-3-pro-preview`
- `openrouter/gryphe/mythomax-l2-13b`
- `openrouter/ibm-granite/granite-4.0-h-micro`
- `openrouter/ibm-granite/granite-4.1-8b`
- `openrouter/inception/mercury-2`
- `openrouter/inclusionai/ling-2.6-1t`
- `openrouter/inclusionai/ling-2.6-flash`
- `openrouter/inclusionai/ring-2.6-1t`
- `openrouter/inflection/inflection-3-pi`
- `openrouter/inflection/inflection-3-productivity`
- `openrouter/kwaipilot/kat-coder-pro-v2`
- `openrouter/liquid/lfm-2.5-1.2b-instruct:free`
- `openrouter/liquid/lfm-2.5-1.2b-thinking:free`
- `openrouter/mancer/weaver`
- `openrouter/meta-llama/llama-3-8b-instruct`
- `openrouter/meta-llama/llama-3.1-70b-instruct`
- `openrouter/meta-llama/llama-3.1-8b-instruct`
- `openrouter/meta-llama/llama-3.2-11b-vision-instruct`
- `openrouter/meta-llama/llama-3.2-1b-instruct`
- `openrouter/meta-llama/llama-3.2-3b-instruct`
- `openrouter/meta-llama/llama-3.2-3b-instruct:free`
- `openrouter/meta-llama/llama-3.3-70b-instruct`
- `openrouter/meta-llama/llama-3.3-70b-instruct:free`
- `openrouter/meta-llama/llama-4-maverick`
- `openrouter/meta-llama/llama-4-scout`
- `openrouter/meta-llama/llama-guard-4-12b`
- `openrouter/microsoft/phi-4`
- `openrouter/microsoft/wizardlm-2-8x22b`
- `openrouter/minimax/minimax-01`
- `openrouter/minimax/minimax-m1`
- `openrouter/minimax/minimax-m2`
- `openrouter/minimax/minimax-m2-her`
- `openrouter/minimax/minimax-m2.1`
- `openrouter/minimax/minimax-m2.5`
- `openrouter/minimax/minimax-m2.7`
- `openrouter/minimax/minimax-m3`
- `openrouter/mistralai/codestral-2508`
- `openrouter/mistralai/devstral-2512`
- `openrouter/mistralai/ministral-14b-2512`
- `openrouter/mistralai/ministral-3b-2512`
- `openrouter/mistralai/ministral-8b-2512`
- `openrouter/mistralai/mistral-large`
- `openrouter/mistralai/mistral-large-2407`
- `openrouter/mistralai/mistral-large-2512`
- `openrouter/mistralai/mistral-medium-3`
- `openrouter/mistralai/mistral-medium-3-5`
- `openrouter/mistralai/mistral-medium-3.1`
- `openrouter/mistralai/mistral-nemo`
- `openrouter/mistralai/mistral-saba`
- `openrouter/mistralai/mistral-small-24b-instruct-2501`
- `openrouter/mistralai/mistral-small-2603`
- `openrouter/mistralai/mistral-small-3.1-24b-instruct`
- `openrouter/mistralai/mistral-small-3.2-24b-instruct`
- `openrouter/mistralai/mixtral-8x22b-instruct`
- `openrouter/mistralai/voxtral-small-24b-2507`
- `openrouter/moonshotai/kimi-k2`
- `openrouter/moonshotai/kimi-k2-0905`
- `openrouter/moonshotai/kimi-k2-thinking`
- `openrouter/moonshotai/kimi-k2.5`
- `openrouter/moonshotai/kimi-k2.6`
- `openrouter/moonshotai/kimi-k2.7-code`
- `openrouter/morph/morph-v3-fast`
- `openrouter/morph/morph-v3-large`
- `openrouter/nex-agi/nex-n2-mini`
- `openrouter/nex-agi/nex-n2-pro`
- `openrouter/nousresearch/hermes-3-llama-3.1-405b`
- `openrouter/nousresearch/hermes-3-llama-3.1-405b:free`
- `openrouter/nousresearch/hermes-3-llama-3.1-70b`
- `openrouter/nousresearch/hermes-4-405b`
- `openrouter/nousresearch/hermes-4-70b`
- `openrouter/nvidia/llama-3.3-nemotron-super-49b-v1.5`
- `openrouter/nvidia/nemotron-3-nano-30b-a3b`
- `openrouter/nvidia/nemotron-3-nano-30b-a3b:free`
- `openrouter/nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free`
- `openrouter/nvidia/nemotron-3-super-120b-a12b`
- `openrouter/nvidia/nemotron-3-super-120b-a12b:free`
- `openrouter/nvidia/nemotron-3-ultra-550b-a55b`
- `openrouter/nvidia/nemotron-3-ultra-550b-a55b:free`
- `openrouter/nvidia/nemotron-3.5-content-safety:free`
- `openrouter/nvidia/nemotron-nano-12b-v2-vl:free`
- `openrouter/nvidia/nemotron-nano-9b-v2:free`
- `openrouter/openai/gpt-3.5-turbo`
- `openrouter/openai/gpt-3.5-turbo-0613`
- `openrouter/openai/gpt-3.5-turbo-16k`
- `openrouter/openai/gpt-3.5-turbo-instruct`
- `openrouter/openai/gpt-4`
- `openrouter/openai/gpt-4-turbo`
- `openrouter/openai/gpt-4-turbo-preview`
- `openrouter/openai/gpt-4.1`
- `openrouter/openai/gpt-4.1-mini`
- `openrouter/openai/gpt-4.1-nano`
- `openrouter/openai/gpt-4o`
- `openrouter/openai/gpt-4o-2024-05-13`
- `openrouter/openai/gpt-4o-2024-08-06`
- `openrouter/openai/gpt-4o-2024-11-20`
- `openrouter/openai/gpt-4o-mini`
- `openrouter/openai/gpt-4o-mini-2024-07-18`
- `openrouter/openai/gpt-4o-mini-search-preview`
- `openrouter/openai/gpt-4o-search-preview`
- `openrouter/openai/gpt-5`
- `openrouter/openai/gpt-5-chat`
- `openrouter/openai/gpt-5-codex`
- `openrouter/openai/gpt-5-image`
- `openrouter/openai/gpt-5-image-mini`
- `openrouter/openai/gpt-5-mini`
- `openrouter/openai/gpt-5-nano`
- `openrouter/openai/gpt-5-pro`
- `openrouter/openai/gpt-5.1`
- `openrouter/openai/gpt-5.1-chat`
- `openrouter/openai/gpt-5.1-codex`
- `openrouter/openai/gpt-5.1-codex-max`
- `openrouter/openai/gpt-5.1-codex-mini`
- `openrouter/openai/gpt-5.2`
- `openrouter/openai/gpt-5.2-chat`
- `openrouter/openai/gpt-5.2-codex`
- `openrouter/openai/gpt-5.2-pro`
- `openrouter/openai/gpt-5.3-chat`
- `openrouter/openai/gpt-5.3-codex`
- `openrouter/openai/gpt-5.4`
- `openrouter/openai/gpt-5.4-image-2`
- `openrouter/openai/gpt-5.4-mini`
- `openrouter/openai/gpt-5.4-nano`
- `openrouter/openai/gpt-5.4-pro`
- `openrouter/openai/gpt-5.5`
- `openrouter/openai/gpt-5.5-pro`
- `openrouter/openai/gpt-audio`
- `openrouter/openai/gpt-audio-mini`
- `openrouter/openai/gpt-chat-latest`
- `openrouter/openai/gpt-oss-120b`
- `openrouter/openai/gpt-oss-120b:free`
- `openrouter/openai/gpt-oss-20b`
- `openrouter/openai/gpt-oss-20b:free`
- `openrouter/openai/gpt-oss-safeguard-20b`
- `openrouter/openai/o1`
- `openrouter/openai/o1-pro`
- `openrouter/openai/o3`
- `openrouter/openai/o3-deep-research`
- `openrouter/openai/o3-mini`
- `openrouter/openai/o3-mini-high`
- `openrouter/openai/o3-pro`
- `openrouter/openai/o4-mini`
- `openrouter/openai/o4-mini-deep-research`
- `openrouter/openai/o4-mini-high`
- `openrouter/openrouter/auto`
- `openrouter/openrouter/bodybuilder`
- `openrouter/openrouter/free`
- `openrouter/openrouter/fusion`
- `openrouter/openrouter/pareto-code`
- `openrouter/perceptron/perceptron-mk1`
- `openrouter/perplexity/sonar`
- `openrouter/perplexity/sonar-deep-research`
- `openrouter/perplexity/sonar-pro`
- `openrouter/perplexity/sonar-pro-search`
- `openrouter/perplexity/sonar-reasoning-pro`
- `openrouter/poolside/laguna-m.1`
- `openrouter/poolside/laguna-m.1:free`
- `openrouter/poolside/laguna-xs-2.1`
- `openrouter/poolside/laguna-xs-2.1:free`
- `openrouter/poolside/laguna-xs.2`
- `openrouter/poolside/laguna-xs.2:free`
- `openrouter/qwen/qwen-2.5-72b-instruct`
- `openrouter/qwen/qwen-2.5-7b-instruct`
- `openrouter/qwen/qwen-2.5-coder-32b-instruct`
- `openrouter/qwen/qwen-plus`
- `openrouter/qwen/qwen-plus-2025-07-28`
- `openrouter/qwen/qwen-plus-2025-07-28:thinking`
- `openrouter/qwen/qwen2.5-vl-72b-instruct`
- `openrouter/qwen/qwen3-14b`
- `openrouter/qwen/qwen3-235b-a22b`
- `openrouter/qwen/qwen3-235b-a22b-2507`
- `openrouter/qwen/qwen3-235b-a22b-thinking-2507`
- `openrouter/qwen/qwen3-30b-a3b`
- `openrouter/qwen/qwen3-30b-a3b-instruct-2507`
- `openrouter/qwen/qwen3-30b-a3b-thinking-2507`
- `openrouter/qwen/qwen3-32b`
- `openrouter/qwen/qwen3-8b`
- `openrouter/qwen/qwen3-coder`
- `openrouter/qwen/qwen3-coder-30b-a3b-instruct`
- `openrouter/qwen/qwen3-coder-flash`
- `openrouter/qwen/qwen3-coder-next`
- `openrouter/qwen/qwen3-coder-plus`
- `openrouter/qwen/qwen3-coder:free`
- `openrouter/qwen/qwen3-max`
- `openrouter/qwen/qwen3-max-thinking`
- `openrouter/qwen/qwen3-next-80b-a3b-instruct`
- `openrouter/qwen/qwen3-next-80b-a3b-instruct:free`
- `openrouter/qwen/qwen3-next-80b-a3b-thinking`
- `openrouter/qwen/qwen3-vl-235b-a22b-instruct`
- `openrouter/qwen/qwen3-vl-235b-a22b-thinking`
- `openrouter/qwen/qwen3-vl-30b-a3b-instruct`
- `openrouter/qwen/qwen3-vl-30b-a3b-thinking`
- `openrouter/qwen/qwen3-vl-32b-instruct`
- `openrouter/qwen/qwen3-vl-8b-instruct`
- `openrouter/qwen/qwen3-vl-8b-thinking`
- `openrouter/qwen/qwen3.5-122b-a10b`
- `openrouter/qwen/qwen3.5-27b`
- `openrouter/qwen/qwen3.5-35b-a3b`
- `openrouter/qwen/qwen3.5-397b-a17b`
- `openrouter/qwen/qwen3.5-9b`
- `openrouter/qwen/qwen3.5-flash-02-23`
- `openrouter/qwen/qwen3.5-plus-02-15`
- `openrouter/qwen/qwen3.5-plus-20260420`
- `openrouter/qwen/qwen3.6-27b`
- `openrouter/qwen/qwen3.6-35b-a3b`
- `openrouter/qwen/qwen3.6-flash`
- `openrouter/qwen/qwen3.6-max-preview`
- `openrouter/qwen/qwen3.6-plus`
- `openrouter/qwen/qwen3.7-max`
- `openrouter/qwen/qwen3.7-plus`
- `openrouter/rekaai/reka-edge`
- `openrouter/rekaai/reka-flash-3`
- `openrouter/relace/relace-apply-3`
- `openrouter/relace/relace-search`
- `openrouter/sakana/fugu-ultra`
- `openrouter/sao10k/l3-lunaris-8b`
- `openrouter/sao10k/l3.1-70b-hanami-x1`
- `openrouter/sao10k/l3.1-euryale-70b`
- `openrouter/sao10k/l3.3-euryale-70b`
- `openrouter/stepfun/step-3.5-flash`
- `openrouter/stepfun/step-3.7-flash`
- `openrouter/tencent/hunyuan-a13b-instruct`
- `openrouter/tencent/hy3`
- `openrouter/tencent/hy3-preview`
- `openrouter/tencent/hy3:free`
- `openrouter/thedrummer/cydonia-24b-v4.1`
- `openrouter/thedrummer/rocinante-12b`
- `openrouter/thedrummer/skyfall-36b-v2`
- `openrouter/thedrummer/unslopnemo-12b`
- `openrouter/undi95/remm-slerp-l2-13b`
- `openrouter/upstage/solar-pro-3`
- `openrouter/writer/palmyra-x5`
- `openrouter/x-ai/grok-4.20`
- `openrouter/x-ai/grok-4.20-multi-agent`
- `openrouter/x-ai/grok-4.3`
- `openrouter/x-ai/grok-4.5`
- `openrouter/x-ai/grok-build-0.1`
- `openrouter/xiaomi/mimo-v2.5`
- `openrouter/xiaomi/mimo-v2.5-pro`
- `openrouter/z-ai/glm-4.5`
- `openrouter/z-ai/glm-4.5-air`
- `openrouter/z-ai/glm-4.5v`
- `openrouter/z-ai/glm-4.6`
- `openrouter/z-ai/glm-4.6v`
- `openrouter/z-ai/glm-4.7`
- `openrouter/z-ai/glm-4.7-flash`
- `openrouter/z-ai/glm-5`
- `openrouter/z-ai/glm-5-turbo`
- `openrouter/z-ai/glm-5.1`
- `openrouter/z-ai/glm-5.2`
- `openrouter/z-ai/glm-5v-turbo`
- `openrouter/~anthropic/claude-fable-latest`
- `openrouter/~anthropic/claude-haiku-latest`
- `openrouter/~anthropic/claude-opus-latest`
- `openrouter/~anthropic/claude-sonnet-latest`
- `openrouter/~google/gemini-flash-latest`
- `openrouter/~google/gemini-pro-latest`
- `openrouter/~moonshotai/kimi-latest`
- `openrouter/~openai/gpt-latest`
- `openrouter/~openai/gpt-mini-latest`
- `openrouter/~x-ai/grok-latest`

### vertex

- `vertex/antigravity-preview-05-2026`
- `vertex/aqa`
- `vertex/deep-research-max-preview-04-2026`
- `vertex/deep-research-preview-04-2026`
- `vertex/deep-research-pro-preview-12-2025`
- `vertex/gemini-2.0-flash`
- `vertex/gemini-2.0-flash-001`
- `vertex/gemini-2.0-flash-lite`
- `vertex/gemini-2.0-flash-lite-001`
- `vertex/gemini-2.5-computer-use-preview-10-2025`
- `vertex/gemini-2.5-flash`
- `vertex/gemini-2.5-flash-image`
- `vertex/gemini-2.5-flash-lite`
- `vertex/gemini-2.5-flash-native-audio-latest`
- `vertex/gemini-2.5-flash-native-audio-preview-09-2025`
- `vertex/gemini-2.5-flash-native-audio-preview-12-2025`
- `vertex/gemini-2.5-flash-preview-tts`
- `vertex/gemini-2.5-pro`
- `vertex/gemini-2.5-pro-preview-tts`
- `vertex/gemini-3-flash-preview`
- `vertex/gemini-3-pro-image`
- `vertex/gemini-3-pro-image-preview`
- `vertex/gemini-3-pro-preview`
- `vertex/gemini-3.1-flash-image`
- `vertex/gemini-3.1-flash-image-preview`
- `vertex/gemini-3.1-flash-lite`
- `vertex/gemini-3.1-flash-lite-image`
- `vertex/gemini-3.1-flash-lite-preview`
- `vertex/gemini-3.1-flash-live-preview`
- `vertex/gemini-3.1-flash-tts-preview`
- `vertex/gemini-3.1-pro-preview`
- `vertex/gemini-3.1-pro-preview-customtools`
- `vertex/gemini-3.5-flash`
- `vertex/gemini-3.5-live-translate-preview`
- `vertex/gemini-embedding-001`
- `vertex/gemini-embedding-2`
- `vertex/gemini-embedding-2-preview`
- `vertex/gemini-flash-latest`
- `vertex/gemini-flash-lite-latest`
- `vertex/gemini-omni-flash-preview`
- `vertex/gemini-pro-latest`
- `vertex/gemini-robotics-er-1.5-preview`
- `vertex/gemini-robotics-er-1.6-preview`
- `vertex/gemma-4-26b-a4b-it`
- `vertex/gemma-4-31b-it`
- `vertex/imagen-4.0-fast-generate-001`
- `vertex/imagen-4.0-generate-001`
- `vertex/imagen-4.0-ultra-generate-001`
- `vertex/lyria-3-clip-preview`
- `vertex/lyria-3-pro-preview`
- `vertex/nano-banana-pro-preview`
- `vertex/veo-3.1-fast-generate-preview`
- `vertex/veo-3.1-generate-preview`
- `vertex/veo-3.1-lite-generate-preview`

### zai

- `zai/glm-4.7`
- `zai/glm-4.7-flash`
- `zai/glm-5`
- `zai/glm-5-turbo`
- `zai/glm-5.1`
- `zai/glm-5.2`

### zm

- `zm/anthropic/claude-fable-5`
- `zm/anthropic/claude-fable-5-free`
- `zm/anthropic/claude-haiku-4.5`
- `zm/anthropic/claude-opus-4`
- `zm/anthropic/claude-opus-4.1`
- `zm/anthropic/claude-opus-4.5`
- `zm/anthropic/claude-opus-4.6`
- `zm/anthropic/claude-opus-4.7`
- `zm/anthropic/claude-opus-4.8`
- `zm/anthropic/claude-sonnet-4`
- `zm/anthropic/claude-sonnet-4.5`
- `zm/anthropic/claude-sonnet-4.6`
- `zm/anthropic/claude-sonnet-5`
- `zm/anthropic/claude-sonnet-5-free`
- `zm/baidu/ernie-5.0-thinking-preview`
- `zm/baidu/ernie-5.1`
- `zm/baidu/ernie-x1.1-preview`
- `zm/bytedance/doubao-seed-1.8`
- `zm/bytedance/doubao-seed-2.0-code`
- `zm/bytedance/doubao-seed-2.0-lite`
- `zm/bytedance/doubao-seed-2.0-mini`
- `zm/bytedance/doubao-seed-2.0-pro`
- `zm/bytedance/doubao-seed-2.1-pro`
- `zm/bytedance/doubao-seed-2.1-turbo`
- `zm/bytedance/doubao-seed-character`
- `zm/bytedance/doubao-seed-code`
- `zm/bytedance/doubao-seed-evolving`
- `zm/deepseek/deepseek-chat`
- `zm/deepseek/deepseek-chat-v3.1`
- `zm/deepseek/deepseek-r1-0528`
- `zm/deepseek/deepseek-reasoner`
- `zm/deepseek/deepseek-v3.2`
- `zm/deepseek/deepseek-v3.2-exp`
- `zm/deepseek/deepseek-v4-flash`
- `zm/deepseek/deepseek-v4-pro`
- `zm/google/gemini-2.5-flash`
- `zm/google/gemini-2.5-flash-lite`
- `zm/google/gemini-2.5-pro`
- `zm/google/gemini-3-flash-preview`
- `zm/google/gemini-3.1-flash-lite`
- `zm/google/gemini-3.1-flash-lite-preview`
- `zm/google/gemini-3.1-pro-preview`
- `zm/google/gemini-3.5-flash`
- `zm/google/gemini-embedding-2`
- `zm/inclusionai/ling-2.6-1t`
- `zm/inclusionai/ling-2.6-flash`
- `zm/inclusionai/llada2.1-flash`
- `zm/inclusionai/ring-2.6-1t`
- `zm/kuaishou/kat-coder-pro-v2`
- `zm/meituan/longcat-2.0`
- `zm/meta/llama-3.3-70b-instruct`
- `zm/meta/llama-4-scout-17b-16e-instruct`
- `zm/minimax/minimax-m2`
- `zm/minimax/minimax-m2-her`
- `zm/minimax/minimax-m2.1`
- `zm/minimax/minimax-m2.5`
- `zm/minimax/minimax-m2.5-lightning`
- `zm/minimax/minimax-m2.7`
- `zm/minimax/minimax-m2.7-highspeed`
- `zm/minimax/minimax-m3`
- `zm/mistralai/mistral-large-2512`
- `zm/moonshotai/kimi-k2.5`
- `zm/moonshotai/kimi-k2.6`
- `zm/moonshotai/kimi-k2.7-code`
- `zm/moonshotai/kimi-k2.7-code-highspeed`
- `zm/openai/chat-latest`
- `zm/openai/gpt-4.1`
- `zm/openai/gpt-4.1-mini`
- `zm/openai/gpt-4.1-nano`
- `zm/openai/gpt-4o`
- `zm/openai/gpt-4o-mini`
- `zm/openai/gpt-5`
- `zm/openai/gpt-5-chat`
- `zm/openai/gpt-5-codex`
- `zm/openai/gpt-5-mini`
- `zm/openai/gpt-5-nano`
- `zm/openai/gpt-5-pro`
- `zm/openai/gpt-5.1`
- `zm/openai/gpt-5.1-chat`
- `zm/openai/gpt-5.1-codex`
- `zm/openai/gpt-5.1-codex-mini`
- `zm/openai/gpt-5.2`
- `zm/openai/gpt-5.2-chat`
- `zm/openai/gpt-5.2-codex`
- `zm/openai/gpt-5.2-pro`
- `zm/openai/gpt-5.3-chat`
- `zm/openai/gpt-5.3-codex`
- `zm/openai/gpt-5.4`
- `zm/openai/gpt-5.4-mini`
- `zm/openai/gpt-5.4-nano`
- `zm/openai/gpt-5.4-pro`
- `zm/openai/gpt-5.5`
- `zm/openai/gpt-5.5-pro`
- `zm/openai/gpt-image-1.5`
- `zm/openai/gpt-image-2`
- `zm/openai/o4-mini`
- `zm/openai/text-embedding-3-large`
- `zm/openai/text-embedding-3-small`
- `zm/qwen/qwen3-14b`
- `zm/qwen/qwen3-235b-a22b-2507`
- `zm/qwen/qwen3-235b-a22b-thinking-2507`
- `zm/qwen/qwen3-asr-flash`
- `zm/qwen/qwen3-coder`
- `zm/qwen/qwen3-coder-plus`
- `zm/qwen/qwen3-max`
- `zm/qwen/qwen3-vl-embedding`
- `zm/qwen/qwen3-vl-plus`
- `zm/qwen/qwen3.5-flash`
- `zm/qwen/qwen3.5-plus`
- `zm/qwen/qwen3.6-flash`
- `zm/qwen/qwen3.6-max-preview`
- `zm/qwen/qwen3.6-plus`
- `zm/qwen/qwen3.7-max`
- `zm/qwen/qwen3.7-plus`
- `zm/sapiens-ai/agnes-2.0-flash`
- `zm/stepfun/step-3`
- `zm/stepfun/step-3.5-flash`
- `zm/stepfun/step-3.7-flash`
- `zm/stepfun/step-3.7-flash-free`
- `zm/tencent/hy3`
- `zm/tencent/hy3-preview`
- `zm/x-ai/grok-4.2-fast`
- `zm/x-ai/grok-4.2-fast-non-reasoning`
- `zm/x-ai/grok-4.3`
- `zm/x-ai/grok-build-0.1`
- `zm/xiaomi/mimo-v2.5`
- `zm/xiaomi/mimo-v2.5-pro`
- `zm/z-ai/glm-4.5`
- `zm/z-ai/glm-4.5-air`
- `zm/z-ai/glm-4.6`
- `zm/z-ai/glm-4.6v`
- `zm/z-ai/glm-4.6v-flash`
- `zm/z-ai/glm-4.6v-flash-free`
- `zm/z-ai/glm-4.7`
- `zm/z-ai/glm-4.7-flash-free`
- `zm/z-ai/glm-4.7-flashx`
- `zm/z-ai/glm-5`
- `zm/z-ai/glm-5-turbo`
- `zm/z-ai/glm-5.1`
- `zm/z-ai/glm-5.2`
- `zm/z-ai/glm-5v-turbo`
