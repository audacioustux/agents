import { assertEquals, assertRejects } from "jsr:@std/assert@1";
import { parse as parseYaml } from "jsr:@std/yaml@1";

export type ComboConfig = {
  baseUrl: string;
  combos: Record<string, string[]>;
};

export type ComboDiff = {
  name: string;
  action: "update" | "unchanged";
  changedFields: string[];
  beforeModels: string[];
  afterModels: string[];
};

type JsonObject = Record<string, unknown>;

type ComboClient = {
  getCombos(): Promise<JsonObject[]>;
  getModels(): Promise<JsonObject[]>;
  putCombo(comboId: string, payload: JsonObject): Promise<unknown>;
};

const DEFAULT_CONFIG_PATH = ".github/omniroute/combos.yml";
const MANAGED_FIELD = "models";

export class OmniRouteClient implements ComboClient {
  readonly #baseUrl: string;
  readonly #apiKey: string;

  constructor(baseUrl: string, apiKey: string) {
    this.#baseUrl = baseUrl.replace(/\/+$/, "");
    this.#apiKey = apiKey;
  }

  async getCombos(): Promise<JsonObject[]> {
    return responseList(
      await this.#request("GET", "/api/combos"),
      "combos",
      "GET /api/combos",
    );
  }

  async getModels(): Promise<JsonObject[]> {
    return responseList(
      await this.#request("GET", "/api/models?all=true"),
      "models",
      "GET /api/models?all=true",
    );
  }

  async putCombo(comboId: string, payload: JsonObject): Promise<unknown> {
    return await this.#request(
      "PUT",
      `/api/combos/${encodeURIComponent(comboId)}`,
      payload,
    );
  }

  async #request(
    method: string,
    path: string,
    payload?: JsonObject,
  ): Promise<unknown> {
    const url = `${this.#baseUrl}${path}`;
    const headers = new Headers({
      Accept: "application/json",
      Authorization: `Bearer ${this.#apiKey}`,
      "User-Agent": "omniroute-combo-sync/2.0",
    });
    const init: RequestInit = { method, headers };
    if (payload !== undefined) {
      headers.set("Content-Type", "application/json");
      init.body = JSON.stringify(payload);
    }

    const response = await fetch(url, init);
    const body = await response.text();
    if (!response.ok) {
      throw new Error(
        `${method} ${url} failed with status ${response.status}: ${body}`,
      );
    }
    try {
      return body.length === 0 ? null : JSON.parse(body);
    } catch (error) {
      throw new Error(
        `${method} ${url} returned invalid JSON: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
    }
  }
}

export function parseComboConfig(source: string): ComboConfig {
  const parsed = parseYaml(source);
  if (!isObject(parsed)) {
    throw new Error("combo config must be a YAML object");
  }
  const baseUrl = parsed.baseUrl;
  if (typeof baseUrl !== "string" || baseUrl.trim() === "") {
    throw new Error("baseUrl must be a non-empty string");
  }
  if (baseUrl.replace(/\/+$/, "").endsWith("/v1")) {
    throw new Error(
      "baseUrl must be the OmniRoute origin, not the OpenAI /v1 endpoint",
    );
  }
  const combos = parsed.combos;
  if (!isObject(combos) || Object.keys(combos).length === 0) {
    throw new Error("combos must be a non-empty mapping");
  }

  const normalizedCombos: Record<string, string[]> = {};
  for (const [comboName, models] of Object.entries(combos)) {
    if (!Array.isArray(models) || models.length === 0) {
      throw new Error(`combos.${comboName} must be a non-empty string array`);
    }
    const seenModels = new Set<string>();
    normalizedCombos[comboName] = models.map((model, index) => {
      if (typeof model !== "string" || model.trim() === "") {
        throw new Error(
          `combos.${comboName}[${index}] must be a non-empty string`,
        );
      }
      if (seenModels.has(model)) {
        throw new Error(`duplicate model id in combo ${comboName}: ${model}`);
      }
      seenModels.add(model);
      return model;
    });
  }

  return { baseUrl: baseUrl.replace(/\/+$/, ""), combos: normalizedCombos };
}

export function semanticModelEntry(entry: JsonObject): JsonObject {
  const model = entry.model;
  if (typeof model !== "string") {
    throw new Error("model entry is missing model");
  }
  return { model };
}

export function diffCombo(liveCombo: JsonObject, models: string[]): ComboDiff {
  const name = stringField(liveCombo, "name");
  const liveModels = arrayField(liveCombo, "models").map((entry) =>
    semanticModelEntry(objectValue(entry, `${name}.models[]`))
  );
  const desiredModels = models.map((model) =>
    semanticModelEntry(modelEntry(name, 0, model))
  );
  const changedFields = jsonStable(liveModels) === jsonStable(desiredModels)
    ? []
    : [MANAGED_FIELD];
  return {
    name,
    action: changedFields.length === 0 ? "unchanged" : "update",
    changedFields,
    beforeModels: liveModels.map((entry) => String(entry.model)),
    afterModels: models,
  };
}

export function mergeComboModels(
  liveCombo: JsonObject,
  models: string[],
): JsonObject {
  const comboName = stringField(liveCombo, "name");
  const merged = structuredClone(liveCombo);
  const liveEntriesByModel = new Map<string, JsonObject[]>();
  for (const liveEntry of arrayField(liveCombo, "models")) {
    const liveModel = objectValue(liveEntry, `${comboName}.models[]`);
    const model = stringField(liveModel, "model");
    const entries = liveEntriesByModel.get(model) ?? [];
    entries.push(structuredClone(liveModel));
    liveEntriesByModel.set(model, entries);
  }

  merged.models = models.map((model, index) => {
    const existingEntries = liveEntriesByModel.get(model) ?? [];
    const existingEntry = existingEntries.shift();
    if (existingEntry !== undefined) {
      return existingEntry;
    }
    return modelEntry(comboName, index + 1, model);
  });
  return merged;
}

export function validateModelCatalog(
  config: ComboConfig,
  catalog: JsonObject[],
): string[] {
  const validModels = new Set<string>();
  for (const row of catalog) {
    if (typeof row.id === "string") {
      validModels.add(row.id);
    }
    if (typeof row.fullModel === "string") {
      validModels.add(row.fullModel);
    }
    if (typeof row.provider === "string" && typeof row.model === "string") {
      validModels.add(`${row.provider}/${row.model}`);
    }
  }

  const errors: string[] = [];
  for (const [comboName, models] of Object.entries(config.combos)) {
    for (const model of models) {
      if (!validModels.has(model)) {
        errors.push(`${comboName}: unknown model id ${model}`);
      }
    }
  }
  return errors;
}

export async function syncCombos(
  config: ComboConfig,
  client: ComboClient,
  dryRun: boolean,
): Promise<ComboDiff[]> {
  const [liveCombos, catalog] = await Promise.all([
    client.getCombos(),
    client.getModels(),
  ]);
  const catalogErrors = validateModelCatalog(config, catalog);
  if (catalogErrors.length > 0) {
    throw new Error(`invalid combo models:\n${catalogErrors.join("\n")}`);
  }

  const liveByName = new Map<string, JsonObject>();
  for (const liveCombo of liveCombos) {
    liveByName.set(stringField(liveCombo, "name"), liveCombo);
  }

  const declaredNames = new Set(Object.keys(config.combos));
  const extraComboNames = [...liveByName.keys()].filter((name) =>
    !declaredNames.has(name)
  );
  if (extraComboNames.length > 0) {
    throw new Error(
      `live combo not declared in config: ${extraComboNames.join(", ")}`,
    );
  }

  const diffs: ComboDiff[] = [];
  for (const [comboName, models] of Object.entries(config.combos)) {
    const liveCombo = liveByName.get(comboName);
    if (liveCombo === undefined) {
      throw new Error(`declared combo not found: ${comboName}`);
    }
    diffs.push(diffCombo(liveCombo, models));
  }

  if (dryRun) {
    return diffs;
  }

  for (const diff of diffs) {
    if (diff.action === "unchanged") {
      continue;
    }
    const liveCombo = liveByName.get(diff.name)!;
    const comboId = stringField(liveCombo, "id");
    await client.putCombo(
      comboId,
      mergeComboModels(liveCombo, config.combos[diff.name]),
    );
  }

  const verifiedLiveCombos = await client.getCombos();
  const verifiedByName = new Map<string, JsonObject>();
  for (const liveCombo of verifiedLiveCombos) {
    verifiedByName.set(stringField(liveCombo, "name"), liveCombo);
  }
  const failures: string[] = [];
  for (const [comboName, models] of Object.entries(config.combos)) {
    const liveCombo = verifiedByName.get(comboName);
    if (liveCombo === undefined) {
      failures.push(`${comboName}: missing after apply`);
      continue;
    }
    const diff = diffCombo(liveCombo, models);
    if (diff.action !== "unchanged") {
      failures.push(`${comboName}: ${diff.changedFields.join(", ")}`);
    }
  }
  if (failures.length > 0) {
    throw new Error(`post-apply verification failed:\n${failures.join("\n")}`);
  }
  return diffs;
}

function responseList(
  result: unknown,
  wrapperKey: string,
  description: string,
): JsonObject[] {
  const value = isObject(result) && wrapperKey in result
    ? result[wrapperKey]
    : result;
  if (!Array.isArray(value)) {
    throw new Error(`${description} returned non-list JSON`);
  }
  return value.map((entry, index) =>
    objectValue(entry, `${description}[${index}]`)
  );
}

function modelEntry(
  comboName: string,
  position: number,
  model: string,
): JsonObject {
  return {
    id: `${comboName}-model-${position}-${slugModelId(model)}`,
    kind: "model",
    model,
    providerId: providerIdFromModel(model),
    weight: 0,
  };
}

function providerIdFromModel(model: string): string {
  return model.split("/", 1)[0];
}

function slugModelId(model: string): string {
  return model.replace(/[^A-Za-z0-9]+/g, "-").replace(/^-+|-+$/g, "")
    .toLowerCase();
}

function jsonStable(value: unknown): string {
  return JSON.stringify(value, (_key, nested) => {
    if (!isObject(nested) || Array.isArray(nested)) {
      return nested;
    }
    return Object.fromEntries(
      Object.entries(nested).sort(([left], [right]) =>
        left.localeCompare(right)
      ),
    );
  });
}

function isObject(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function objectValue(value: unknown, label: string): JsonObject {
  if (!isObject(value)) {
    throw new Error(`${label} must be an object`);
  }
  return value;
}

function stringField(object: JsonObject, field: string): string {
  const value = object[field];
  if (typeof value !== "string" || value.length === 0) {
    throw new Error(`${field} must be a non-empty string`);
  }
  return value;
}

function arrayField(object: JsonObject, field: string): unknown[] {
  const value = object[field];
  if (!Array.isArray(value)) {
    throw new Error(`${field} must be an array`);
  }
  return value;
}

async function main(): Promise<number> {
  const args = [...Deno.args];
  const dryRun = args.includes("--dry-run");
  const apply = args.includes("--apply");
  if (dryRun === apply) {
    console.error("exactly one of --dry-run or --apply is required");
    return 1;
  }
  const configFlag = args.indexOf("--config");
  const configPath = configFlag >= 0
    ? args[configFlag + 1]
    : DEFAULT_CONFIG_PATH;
  if (configPath === undefined || configPath.length === 0) {
    console.error("--config requires a path");
    return 1;
  }
  const apiKey = Deno.env.get("OMNIROUTE_API_KEY");
  if (apiKey === undefined || apiKey.length === 0) {
    console.error("OMNIROUTE_API_KEY is required");
    return 1;
  }

  try {
    const config = parseComboConfig(await Deno.readTextFile(configPath));
    const diffs = await syncCombos(
      config,
      new OmniRouteClient(config.baseUrl, apiKey),
      dryRun,
    );
    console.log(JSON.stringify({ dryRun, changes: diffs }, null, 2));
    return 0;
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    return 1;
  }
}

Deno.test("parseComboConfig reads base URL and ordered model arrays", () => {
  const config = parseComboConfig(`
baseUrl: https://omni.tux.bd
combos:
  coding:
    - ollamacloud/minimax-m3
    - bzl/minimax-m3
  best-free:
    - bzl/auto:free
`);

  assertEquals(config, {
    baseUrl: "https://omni.tux.bd",
    combos: {
      coding: ["ollamacloud/minimax-m3", "bzl/minimax-m3"],
      "best-free": ["bzl/auto:free"],
    },
  });
});

Deno.test("default combo config parses actual repo file", async () => {
  const config = parseComboConfig(await Deno.readTextFile(DEFAULT_CONFIG_PATH));

  assertEquals(config.baseUrl, "https://omni.tux.bd");
  assertEquals(config.combos.coding, [
    "ollamacloud/minimax-m3",
    "bzl/minimax-m3",
    "minimax/MiniMax-M3",
    "cx/gpt-5.5-high",
    "mistral/codestral-latest",
  ]);
  assertEquals(config.combos["best-free"].includes("bzl/auto:free"), true);
});

Deno.test("diffCombo compares only ordered model ids", () => {
  const diff = diffCombo(
    {
      name: "coding",
      models: [
        {
          id: "live-a",
          kind: "model",
          model: "ollamacloud/minimax-m3",
          providerId: "stale-provider-id",
          weight: 80,
        },
        {
          id: "live-b",
          kind: "model",
          model: "bzl/minimax-m3",
          weight: 20,
        },
      ],
    },
    ["ollamacloud/minimax-m3", "bzl/minimax-m3"],
  );

  assertEquals(diff.action, "unchanged");
  assertEquals(diff.changedFields, []);
});

Deno.test("mergeComboModels preserves unmanaged fields and retained model metadata", () => {
  const merged = mergeComboModels(
    {
      id: "combo-1",
      name: "coding",
      strategy: "weighted",
      sortOrder: 7,
      models: [
        {
          id: "live-a",
          kind: "model",
          model: "ollamacloud/minimax-m3",
          providerId: "ollamacloud",
          weight: 80,
          latencyClass: "fast",
        },
        {
          id: "live-b",
          kind: "model",
          model: "bzl/minimax-m3",
          providerId: "bzl",
          weight: 20,
        },
      ],
    },
    ["bzl/minimax-m3", "ollamacloud/minimax-m3", "minimax/MiniMax-M3"],
  );

  assertEquals(merged.id, "combo-1");
  assertEquals(merged.sortOrder, 7);
  assertEquals(merged.models, [
    {
      id: "live-b",
      kind: "model",
      model: "bzl/minimax-m3",
      providerId: "bzl",
      weight: 20,
    },
    {
      id: "live-a",
      kind: "model",
      model: "ollamacloud/minimax-m3",
      providerId: "ollamacloud",
      weight: 80,
      latencyClass: "fast",
    },
    {
      id: "coding-model-3-minimax-minimax-m3",
      kind: "model",
      model: "minimax/MiniMax-M3",
      providerId: "minimax",
      weight: 0,
    },
  ]);
});

Deno.test("validateModelCatalog accepts id fullModel and provider/model forms", () => {
  const errors = validateModelCatalog(
    {
      baseUrl: "https://omni.tux.bd",
      combos: {
        a: ["direct/id"],
        b: ["cf/@cf/meta/llama-3.3-70b-instruct"],
        c: ["gh/claude-sonnet-5"],
      },
    },
    [
      { id: "direct/id" },
      { fullModel: "cf/@cf/meta/llama-3.3-70b-instruct" },
      { provider: "gh", model: "claude-sonnet-5" },
    ],
  );

  assertEquals(errors, []);
});

Deno.test("syncCombos dry-run validates combos and never writes", async () => {
  const writes: unknown[] = [];
  const diffs = await syncCombos(
    { baseUrl: "https://omni.tux.bd", combos: { coding: ["bzl/minimax-m3"] } },
    {
      getCombos: () =>
        Promise.resolve([
          {
            id: "combo-1",
            name: "coding",
            models: [{
              kind: "model",
              model: "ollamacloud/minimax-m3",
              providerId: "ollamacloud",
              weight: 0,
            }],
          },
        ]),
      getModels: () =>
        Promise.resolve([{ provider: "bzl", model: "minimax-m3" }]),
      putCombo: (_id: string, payload: unknown) => {
        writes.push(payload);
        return Promise.resolve({});
      },
    },
    true,
  );

  assertEquals(writes, []);
  assertEquals(diffs[0].action, "update");
  assertEquals(diffs[0].changedFields, ["models"]);
});

Deno.test("syncCombos apply verifies post-apply state", async () => {
  let applied = false;
  const diffs = await syncCombos(
    { baseUrl: "https://omni.tux.bd", combos: { coding: ["bzl/minimax-m3"] } },
    {
      getCombos: () =>
        Promise.resolve([
          {
            id: "combo-1",
            name: "coding",
            models: applied
              ? [{
                kind: "model",
                model: "bzl/minimax-m3",
                providerId: "bzl",
                weight: 0,
              }]
              : [],
          },
        ]),
      getModels: () =>
        Promise.resolve([{ provider: "bzl", model: "minimax-m3" }]),
      putCombo: () => {
        applied = true;
        return Promise.resolve({});
      },
    },
    false,
  );

  assertEquals(diffs[0].action, "update");
});

Deno.test("syncCombos rejects undeclared live combo names", async () => {
  await assertRejects(
    () =>
      syncCombos(
        {
          baseUrl: "https://omni.tux.bd",
          combos: { missing: ["bzl/minimax-m3"] },
        },
        {
          getCombos: () => Promise.resolve([]),
          getModels: () =>
            Promise.resolve([{ provider: "bzl", model: "minimax-m3" }]),
          putCombo: () => Promise.resolve({}),
        },
        true,
      ),
    Error,
    "declared combo not found: missing",
  );
});

Deno.test("syncCombos rejects extra live combo names", async () => {
  await assertRejects(
    () =>
      syncCombos(
        {
          baseUrl: "https://omni.tux.bd",
          combos: { coding: ["bzl/minimax-m3"] },
        },
        {
          getCombos: () =>
            Promise.resolve([
              { id: "combo-1", name: "coding", models: [] },
              { id: "combo-2", name: "unmanaged", models: [] },
            ]),
          getModels: () =>
            Promise.resolve([{ provider: "bzl", model: "minimax-m3" }]),
          putCombo: () => Promise.resolve({}),
        },
        true,
      ),
    Error,
    "live combo not declared in config: unmanaged",
  );
});

if (import.meta.main) {
  Deno.exit(await main());
}
