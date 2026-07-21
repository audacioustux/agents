import { assertEquals, assertRejects, assertThrows } from "jsr:@std/assert@1";
import {
  type ComboConfig,
  type LiveCombo,
  OmniRouteClient,
  parseConfig,
  sync,
} from "./sync-omniroute-combos.ts";

const VALID_YAML = `
baseUrl: https://omni.tux.bd
combos:
  coding:
    - ollamacloud/minimax-m3
    - bzl/minimax-m3
  best-free:
    - bzl/auto:free
`;

Deno.test("parseConfig: accepts a normal config", () => {
  assertEquals(parseConfig(VALID_YAML), {
    baseUrl: "https://omni.tux.bd",
    combos: {
      coding: ["ollamacloud/minimax-m3", "bzl/minimax-m3"],
      "best-free": ["bzl/auto:free"],
    },
  });
});

Deno.test("parseConfig: rejects non-origin baseUrl", () => {
  assertThrows(
    () => parseConfig("baseUrl: https://attacker.example\ncombos:\n  a: [x]\n"),
    Error,
    "baseUrl must be https://omni.tux.bd",
  );
});

Deno.test("parseConfig: rejects empty or duplicate model lists", () => {
  assertThrows(
    () => parseConfig("baseUrl: https://omni.tux.bd\ncombos:\n  coding: []\n"),
    Error,
    "must be a non-empty array",
  );
  assertThrows(
    () =>
      parseConfig("baseUrl: https://omni.tux.bd\ncombos:\n  coding: [a, a]\n"),
    Error,
    "combos.coding has duplicate model a",
  );
});

Deno.test("sync: skips untouched combos and updates changed combos while preserving metadata", async () => {
  const puts: { id: string; combo: LiveCombo }[] = [];
  const api = {
    getCombos: () =>
      Promise.resolve([
        {
          id: "1",
          name: "coding",
          strategy: "weighted",
          models: [
            {
              id: "c1",
              kind: "model",
              model: "ollamacloud/minimax-m3",
              providerId: "ollamacloud",
              weight: 80,
            },
          ],
        },
        {
          id: "2",
          name: "best-free",
          strategy: "weighted",
          models: [
            {
              id: "f1",
              kind: "model",
              model: "bzl/auto:free",
              providerId: "bzl",
              weight: 100,
            },
          ],
        },
      ]),
    putCombo: (id: string, combo: LiveCombo) => {
      puts.push({ id, combo });
      return Promise.resolve();
    },
  };

  const config: ComboConfig = {
    baseUrl: "https://omni.tux.bd",
    combos: {
      coding: ["ollamacloud/minimax-m3"], // unchanged
      "best-free": ["bzl/auto:free", "new/model"], // changed
    },
  };

  const changes = await sync(config, api);
  assertEquals(changes, [
    { name: "coding", changed: false },
    { name: "best-free", changed: true },
  ]);
  assertEquals(puts.length, 1);
  assertEquals(puts[0].id, "2");
  assertEquals(puts[0].combo.strategy, "weighted");
  assertEquals(puts[0].combo.models.length, 2);
  assertEquals(puts[0].combo.models[0].model, "bzl/auto:free");
  assertEquals(puts[0].combo.models[0].weight, 100); // metadata preserved
  assertEquals(puts[0].combo.models[1].model, "new/model");
  assertEquals(puts[0].combo.models[1].weight, 0); // new entry defaults
});

Deno.test("sync: rejects when a declared combo is missing live", async () => {
  const api = {
    getCombos: () => Promise.resolve([]),
    putCombo: () => Promise.resolve(),
  };
  await assertRejects(
    () =>
      sync({ baseUrl: "https://omni.tux.bd", combos: { missing: ["x"] } }, api),
    Error,
    "declared combo not found: missing",
  );
});

Deno.test("sync: rejects unmanaged live combos", async () => {
  const api = {
    getCombos: () =>
      Promise.resolve([
        { id: "1", name: "coding", models: [] },
        { id: "2", name: "unmanaged", models: [] },
      ]),
    putCombo: () => Promise.resolve(),
  };
  await assertRejects(
    () =>
      sync({ baseUrl: "https://omni.tux.bd", combos: { coding: ["x"] } }, api),
    Error,
    "live combo not declared in config: unmanaged",
  );
});

Deno.test("OmniRouteClient: requires an API key", () => {
  assertThrows(
    () => new OmniRouteClient("https://omni.tux.bd", ""),
    Error,
    "OMNIROUTE_API_KEY is required",
  );
});

Deno.test("OmniRouteClient: requests the combos endpoint with bearer auth", async () => {
  let reqUrl = "";
  let authHeader = "";
  const originalFetch = globalThis.fetch;

  try {
    globalThis.fetch = ((input: RequestInfo | URL, init?: RequestInit) => {
      reqUrl = input.toString();
      const headers = new Headers(init?.headers);
      authHeader = headers.get("Authorization") ?? "";
      return Promise.resolve(
        new Response(
          JSON.stringify({
            combos: [
              {
                id: "1",
                name: "coding",
                models: [
                  { id: "m1", model: "a" },
                  { id: "m2", model: "b" },
                ],
              },
            ],
          }),
          { status: 200, headers: { "content-type": "application/json" } },
        ),
      );
    }) as typeof fetch;

    const client = new OmniRouteClient(
      "https://omni.tux.bd",
      "test-secret-key",
    );
    const combos = await client.getCombos();

    assertEquals(reqUrl, "https://omni.tux.bd/api/combos");
    assertEquals(authHeader, "Bearer test-secret-key");
    assertEquals(combos.length, 1);
    assertEquals(combos[0].id, "1");
    assertEquals(combos[0].name, "coding");
    assertEquals(combos[0].models[0].model, "a");
    assertEquals(combos[0].models[1].model, "b");
  } finally {
    globalThis.fetch = originalFetch;
  }
});
