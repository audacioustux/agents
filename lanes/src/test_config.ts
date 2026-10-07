// Imported first by every test file, so rules.ts reads this fixed config instead of the live lanes.config.json that the
// operator edits (caps, default harness). Nothing here may import another module: it must run before rules.ts loads.

const TEST_CONFIG = new URL("test.config.json", import.meta.url).pathname;
Deno.env.set("LANES_CONFIG", TEST_CONFIG);
