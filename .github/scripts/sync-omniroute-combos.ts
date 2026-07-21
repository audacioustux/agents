import { parse as parseYaml } from "jsr:@std/yaml@1";

const DEFAULT_CONFIG_PATH = "dokploy/runner-web/settings/combos.yml";
const OMNIROUTE_ORIGIN = "https://omni.tux.bd";
const USER_AGENT = "omniroute-combo-sync/2.0";

export type ComboConfig = {
  baseUrl: string;
  combos: Record<string, string[]>;
};

export type ModelEntry = {
  model: string;
  [key: string]: unknown;
};

export type LiveCombo = {
  id: string;
  name: string;
  models: ModelEntry[];
  [key: string]: unknown;
};

export type ComboChange = {
  name: string;
  changed: boolean;
};

export type ComboApi = {
  getCombos(): Promise<LiveCombo[]>;
  putCombo(id: string, combo: LiveCombo): Promise<void>;
};

export function parseConfig(source: string): ComboConfig {
  const root = parseYaml(source);
  if (!isRecord(root)) {
    throw new Error("combo config must be a YAML object");
  }

  const baseUrl = readString(root.baseUrl, "baseUrl").replace(/\/+$/, "");
  if (baseUrl !== OMNIROUTE_ORIGIN) {
    throw new Error(`baseUrl must be ${OMNIROUTE_ORIGIN}`);
  }

  const rawCombos = root.combos;
  if (!isRecord(rawCombos) || Object.keys(rawCombos).length === 0) {
    throw new Error("combos must be a non-empty mapping");
  }

  const combos: Record<string, string[]> = {};
  for (const [name, value] of Object.entries(rawCombos)) {
    if (!Array.isArray(value) || value.length === 0) {
      throw new Error(`combos.${name} must be a non-empty array`);
    }

    const seen = new Set<string>();
    combos[name] = value.map((entry) => {
      if (typeof entry !== "string" || entry.trim() === "") {
        throw new Error(`combos.${name} entries must be non-empty strings`);
      }
      if (seen.has(entry)) {
        throw new Error(`combos.${name} has duplicate model ${entry}`);
      }
      seen.add(entry);
      return entry;
    });
  }

  return { baseUrl, combos };
}

export async function sync(
  config: ComboConfig,
  api: ComboApi,
): Promise<ComboChange[]> {
  const liveCombos = await api.getCombos();
  const liveByName = new Map(liveCombos.map((combo) => [combo.name, combo]));
  const declaredNames = new Set(Object.keys(config.combos));

  const extras = liveCombos
    .map((combo) => combo.name)
    .filter((name) => !declaredNames.has(name));
  if (extras.length > 0) {
    throw new Error(`live combo not declared in config: ${extras.join(", ")}`);
  }

  const changes: ComboChange[] = [];
  for (const [name, models] of Object.entries(config.combos)) {
    const liveCombo = liveByName.get(name);
    if (liveCombo === undefined) {
      throw new Error(`declared combo not found: ${name}`);
    }

    const changed = !sameModels(liveCombo.models, models);
    if (changed) {
      await api.putCombo(liveCombo.id, replaceModels(liveCombo, models));
    }
    changes.push({ name, changed });
  }

  return changes;
}

function replaceModels(combo: LiveCombo, desiredModels: string[]): LiveCombo {
  const existingByModel = new Map<string, ModelEntry[]>();
  for (const entry of combo.models) {
    const entries = existingByModel.get(entry.model) ?? [];
    entries.push(structuredClone(entry));
    existingByModel.set(entry.model, entries);
  }

  return {
    ...combo,
    models: desiredModels.map((model, index) => {
      const existing = existingByModel.get(model)?.shift();
      return existing ?? newModelEntry(combo.name, index + 1, model);
    }),
  };
}

function newModelEntry(
  comboName: string,
  position: number,
  model: string,
): ModelEntry {
  return {
    id: `${comboName}-model-${position}-${slug(model)}`,
    kind: "model",
    model,
    providerId: model.split("/", 1)[0],
    weight: 0,
  };
}

function sameModels(entries: ModelEntry[], models: string[]): boolean {
  return entries.length === models.length &&
    entries.every((entry, index) => entry.model === models[index]);
}

export class OmniRouteClient implements ComboApi {
  constructor(
    private readonly baseUrl: string,
    private readonly apiKey: string,
  ) {
    if (apiKey.length === 0) {
      throw new Error("OMNIROUTE_API_KEY is required");
    }
  }

  async getCombos(): Promise<LiveCombo[]> {
    const response = await this.request("GET", "/api/combos");
    if (!isRecord(response) || !Array.isArray(response.combos)) {
      throw new Error("GET /api/combos did not return a combos array");
    }
    return response.combos.map((entry, index) =>
      parseLiveCombo(entry, `combos[${index}]`)
    );
  }

  async putCombo(id: string, combo: LiveCombo): Promise<void> {
    await this.request("PUT", `/api/combos/${encodeURIComponent(id)}`, combo);
  }

  private async request(
    method: string,
    path: string,
    body?: LiveCombo,
  ): Promise<unknown> {
    const url = `${this.baseUrl}${path}`;
    const headers = new Headers({
      Accept: "application/json",
      Authorization: `Bearer ${this.apiKey}`,
      "User-Agent": USER_AGENT,
    });
    const init: RequestInit = { method, headers };
    if (body !== undefined) {
      headers.set("Content-Type", "application/json");
      init.body = JSON.stringify(body);
    }

    const response = await fetch(url, init);
    const text = await response.text();
    if (!response.ok) {
      throw new Error(
        `${method} ${url} failed with status ${response.status}: ${text}`,
      );
    }
    if (text.length === 0) return null;
    try {
      return JSON.parse(text);
    } catch (error) {
      throw new Error(
        `${method} ${url} returned invalid JSON: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
    }
  }
}

function parseLiveCombo(value: unknown, label: string): LiveCombo {
  if (!isRecord(value)) {
    throw new Error(`${label} must be an object`);
  }
  const rawModels = value.models;
  if (!Array.isArray(rawModels)) {
    throw new Error(`${label}.models must be an array`);
  }
  return {
    ...value,
    id: readString(value.id, `${label}.id`),
    name: readString(value.name, `${label}.name`),
    models: rawModels.map((entry, index) =>
      parseModelEntry(entry, `${label}.models[${index}]`)
    ),
  };
}

function parseModelEntry(value: unknown, label: string): ModelEntry {
  if (!isRecord(value)) {
    throw new Error(`${label} must be an object`);
  }
  return { ...value, model: readString(value.model, `${label}.model`) };
}

function readString(value: unknown, label: string): string {
  if (typeof value !== "string" || value.trim() === "") {
    throw new Error(`${label} must be a non-empty string`);
  }
  return value;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function slug(value: string): string {
  return value.replace(/[^A-Za-z0-9]+/g, "-").replace(/^-+|-+$/g, "")
    .toLowerCase();
}

async function main(): Promise<number> {
  const configPath = readArg("--config") ?? DEFAULT_CONFIG_PATH;
  const apiKey = Deno.env.get("OMNIROUTE_API_KEY");
  if (!apiKey) {
    console.error("OMNIROUTE_API_KEY is required");
    return 1;
  }

  try {
    const config = parseConfig(await Deno.readTextFile(configPath));
    const changes = await sync(
      config,
      new OmniRouteClient(config.baseUrl, apiKey),
    );
    console.log(JSON.stringify({ changes }, null, 2));
    return 0;
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    return 1;
  }
}

function readArg(name: string): string | undefined {
  const index = Deno.args.indexOf(name);
  return index >= 0 ? Deno.args[index + 1] : undefined;
}

if (import.meta.main) {
  Deno.exit(await main());
}
