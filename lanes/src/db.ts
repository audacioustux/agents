// The optional shared database module (the `db` config key): one PostgreSQL server, and a Redis server when `db.redis`
// is set, for every lane. Each lane gets a database cloned from a template that is built once per template identity, and
// the sweep removes what ended lanes leave behind. The servers are reached through `Cluster`, so the allocation rules
// are tested against a fake.

import { createHash } from "node:crypto";
import { join, relative, resolve } from "node:path";
import { Refusal } from "./refusal.ts";
import { lockFile, readText } from "./registry.ts";
import { baseEnv, type Config, config, type Registry } from "./rules.ts";

/** What the manager records in a database's comment, so the server itself is the only record. */
export interface LaneNote {
  lane: string;
  head: string;
  redis: number;
  worktree: string;
  created: string;
}
export interface TemplateNote {
  head: string;
  used: string;
}
/** A database the manager owns: `lane_*` and `tpl_*` carry a note; a `tplb_<head>_*` (a template being built) has none. */
export type Entry = { name: string; note: LaneNote | TemplateNote | null };
type Lane = { name: string; note: LaneNote };
type Template = { name: string; note: TemplateNote };
const lanes = (es: Entry[]) => es.filter((e): e is Lane => e.name.startsWith("lane_"));
const templates = (es: Entry[]) => es.filter((e): e is Template => e.name.startsWith("tpl_"));

export interface Cluster {
  ensureUp(): Promise<void>;
  /** A server this manager started once and has not stopped cleanly: whether it should be running now. */
  shouldBeUp(): boolean;
  list(): Promise<Entry[]>;
  create(name: string, note: LaneNote, template: string): Promise<void>;
  /** `db.migrateCommand` in the worktree against `name`, which it creates; with no command, an empty `name`. */
  migrate(worktree: string, name: string): Promise<void>;
  rename(from: string, to: string): Promise<void>;
  annotate(name: string, note: Entry["note"]): Promise<void>;
  drop(name: string): Promise<void>;
  flushRedis(db: number): Promise<void>;
}

export type DbSettings = NonNullable<Config["db"]>;

/** The `db` settings, or a refusal when this installation does not use the module. */
export const settings = (): DbSettings => {
  if (!config.db) throw new Refusal('REFUSED: the db module is off: add a "db" key to the config file');
  return config.db;
};
const dataDir = () => resolve(config.root, settings().dir);

export interface Locks {
  server: string;
  allocate: string;
  template(head: string): string;
}
export const locksIn = (dir: string): Locks => ({
  server: `${dir}/.server.lock`,
  allocate: `${dir}/.allocate.lock`,
  template: (head) => `${dir}/.template-${head}.lock`,
});

/** The template's identity: a hash over every file under `db.templateInputs` (paths relative to the worktree, a directory
 * meaning everything below it), so two worktrees whose inputs are identical share one template. */
export function templateHead(worktree: string): string {
  const walk = (path: string): string[] =>
    Deno.statSync(path).isDirectory
      ? Array.from(Deno.readDirSync(path)).flatMap((e) => walk(join(path, e.name)))
      : [path];
  const files = settings().templateInputs.flatMap((input) => {
    try {
      return walk(join(worktree, input));
    } catch {
      throw new Refusal(`REFUSED: template input ${input} is not in ${worktree}`);
    }
  });
  const hash = createHash("sha256");
  for (const f of files.sort()) hash.update(`${relative(worktree, f)}\0`).update(Deno.readFileSync(f));
  return hash.digest("hex").slice(0, 16);
}

const laneDatabase = (lane: string) => `lane_${lane.replaceAll("-", "_").toLowerCase()}`;
const templateOf = (head: string) => `tpl_${head}`;
const now = () => new Date().toISOString();

/** The variables a lane's suites read, from `db.env`: `{pg}` is the server's URL, `{database}` the lane's database,
 * `{redis}` the URL of its Redis database. Without `db.env`, `DATABASE_URL`, `DATABASE_ADMIN_URL` (the server's own
 * database, for suites that create scratch databases) and, with Redis, `REDIS_URL`. */
export function environment(database: string, redis: number, s: DbSettings = settings()): Record<string, string> {
  const values = {
    pg: `postgresql://postgres@127.0.0.1:${s.pgPort}`,
    database,
    redis: s.redis ? `redis://127.0.0.1:${s.redis.port}/${redis}` : "",
  };
  const env = s.env ?? { ...DEFAULT_ENV, ...(s.redis && { REDIS_URL: "{redis}" }) };
  return Object.fromEntries(
    Object.entries(env).map((
      [k, v],
    ) => [k, v.replace(/\{(pg|database|redis)\}/g, (_, key: keyof typeof values) => values[key])]),
  );
}
const DEFAULT_ENV = { DATABASE_URL: "{pg}/{database}", DATABASE_ADMIN_URL: "{pg}/postgres" };

/** `lanes db new`: the lane's database, cloned from the template of its worktree's head, which the first lane on a head
 * builds and the rest wait for. The same lane on the same head keeps the database it has; on another head it gets a
 * fresh one. */
export async function create(
  c: Cluster,
  a: { lane: string; worktree: string },
  locks = locksIn(dataDir()),
): Promise<Record<string, string>> {
  if (!/^[A-Za-z0-9][A-Za-z0-9_-]{0,40}$/.test(a.lane)) {
    throw new Error(`lane name ${JSON.stringify(a.lane)} must be letters, digits, '-' and '_', at most 41`);
  }
  const worktree = resolve(a.worktree);
  const head = templateHead(worktree);
  await lockFile(locks.server, () => c.ensureUp());
  await lockFile(locks.template(head), async () => {
    if (templates(await c.list()).some((t) => t.name === templateOf(head))) {
      return c.annotate(templateOf(head), { head, used: now() });
    }
    const building = `tplb_${head}_${crypto.randomUUID().slice(0, 8)}`;
    try {
      await c.migrate(worktree, building);
    } catch (e) {
      await c.drop(building);
      throw e;
    }
    await c.rename(building, templateOf(head));
    await c.annotate(templateOf(head), { head, used: now() });
  });
  return await lockFile(locks.allocate, async () => {
    const all = lanes(await c.list());
    const name = laneDatabase(a.lane);
    const old = all.find((e) => e.name === name);
    if (old && old.note.lane !== a.lane) {
      throw new Error(`${name} belongs to lane ${old.note.lane}, and ${a.lane} maps to the same database name`);
    }
    if (old?.note.head === head) return environment(name, old.note.redis);
    const taken = new Set(all.filter((e) => e !== old).map((e) => e.note.redis));
    const redis = old?.note.redis ?? freeRedisDatabase(taken);
    if (old) await c.drop(name);
    await c.flushRedis(redis);
    await c.create(name, { lane: a.lane, head, redis, worktree, created: now() }, templateOf(head));
    return environment(name, redis);
  });
}

/** The lowest Redis database number from 1 that no lane holds (0 is left to operators); 0 when Redis is not configured. */
function freeRedisDatabase(taken: Set<number>): number {
  const databases = settings().redis?.databases;
  if (databases === undefined) return 0;
  const free = [...Array(databases).keys()].slice(1).find((n) => !taken.has(n));
  if (free === undefined) throw new Refusal(`REFUSED: all ${databases - 1} Redis databases are held by lanes`);
  return free;
}

/** `lanes db drop`: the lane's database and the Redis database it held; none is not an error. */
export async function drop(c: Cluster, lane: string, locks = locksIn(dataDir())): Promise<string> {
  await lockFile(locks.server, () => c.ensureUp());
  return await lockFile(locks.allocate, async () => {
    const name = laneDatabase(lane);
    const have = lanes(await c.list()).find((e) => e.name === name);
    if (!have) return `${name}: none`;
    await c.drop(name);
    await c.flushRedis(have.note.redis);
    return `${name}: dropped`;
  });
}

/** One supervisor pass. Drops the database of a lane that ended for good (DONE or STOPPED: the other ended states get
 * resumed and still need it) and of one the registry does not know once it is `orphanHours` old; then each template no
 * database uses and nobody has asked for in `templateKeepMinutes`; then any template left half-built by a crash (a
 * build holds its head's lock throughout, so one still there once the lock is free is dead). Returns what it dropped. */
export async function sweep(
  c: Cluster,
  r: Registry,
  at = new Date(),
  locks = locksIn(dataDir()),
): Promise<string[]> {
  const dropped: string[] = [];
  const minutesSince = (iso: string) => (at.getTime() - Date.parse(iso)) / 60_000;
  await lockFile(locks.allocate, async () => {
    for (const e of lanes(await c.list())) {
      const status = r.lanes[e.note.lane]?.status;
      const over = status === undefined
        ? minutesSince(e.note.created) > settings().orphanHours * 60
        : status === "DONE" || status === "STOPPED";
      if (!over) continue;
      await c.drop(e.name);
      await c.flushRedis(e.note.redis);
      dropped.push(`${e.name} (${status ?? "lane not in the registry"})`);
    }
  });
  const heads = new Set((await c.list()).flatMap((e) => /^tpl(?:b)?_([0-9a-f]{16})/.exec(e.name)?.slice(1) ?? []));
  for (const head of heads) {
    await lockFile(locks.template(head), async () => {
      const all = await c.list();
      const inUse = lanes(all).some((e) => e.note.head === head);
      for (const e of all.filter((e) => e.name.startsWith(`tplb_${head}_`))) {
        await c.drop(e.name);
        dropped.push(`${e.name} (half-built)`);
      }
      const t = templates(all).find((e) => e.note.head === head);
      if (t && !inUse && minutesSince(t.note.used) > settings().templateKeepMinutes) {
        await c.drop(t.name);
        dropped.push(`${t.name} (unused)`);
      }
    });
  }
  return dropped;
}

/** The supervisor's pass over the servers: one that was started and did not stop cleanly is started again, then the sweep
 * runs. Nothing happens on a machine where no lane ever asked for a database. Returns what the sweep dropped. */
export async function maintain(c: Cluster, r: Registry, locks?: Locks): Promise<string[]> {
  if (!c.shouldBeUp()) return [];
  const held = locks ?? locksIn(dataDir());
  await lockFile(held.server, () => c.ensureUp());
  return await sweep(c, r, new Date(), held);
}

// ── The real servers ──

const decode = (b: Uint8Array) => new TextDecoder().decode(b);
async function run(argv: string[], o: { cwd?: string; env?: Record<string, string> } = {}): Promise<string> {
  const [cmd, ...args] = argv;
  const out = await new Deno.Command(cmd, { args, cwd: o.cwd, env: o.env ?? baseEnv(), clearEnv: true, stdin: "null" })
    .output();
  if (!out.success) {
    throw new Error(`${argv.join(" ")}: ${(decode(out.stderr) || decode(out.stdout)).trim().slice(-2000)}`);
  }
  return decode(out.stdout);
}

/** PostgreSQL for data nobody keeps: no fsync, no commit wait, no full-page writes. `max_connections` should cover
 * every lane's connections at once; shared buffers hold a few lanes' working sets and the page cache the rest. */
export const postgresSettings = (): string[] => [
  `port=${settings().pgPort}`,
  "listen_addresses=127.0.0.1",
  `unix_socket_directories=${dataDir()}`,
  `max_connections=${settings().maxConnections}`,
  `shared_buffers=${settings().sharedBuffers}`,
  "fsync=off",
  "synchronous_commit=off",
  "full_page_writes=off",
  "max_wal_size=4GB",
  "checkpoint_timeout=30min",
  "timezone=UTC",
];

const quote = (s: string) => `'${s.replaceAll("'", "''")}'`;
const identifier = (name: string) => {
  if (!/^[a-z0-9_]+$/.test(name)) throw new Error(`database name ${name} needs [a-z0-9_]`);
  return `"${name}"`;
};
const wait = (ms: number) => new Promise((done) => setTimeout(done, ms));

export class RealCluster implements Cluster {
  private get pgdata() {
    return `${dataDir()}/data`;
  }
  private get redisDir() {
    return `${dataDir()}/redis`;
  }

  private psql(sql: string) {
    return run([
      ...["psql", "-X", "-q", "-At", "-v", "ON_ERROR_STOP=1"],
      ...["-h", dataDir(), "-p", `${settings().pgPort}`, "-U", "postgres", "-d", "postgres", "-c", sql],
    ]);
  }
  private redis(...args: string[]) {
    return run(["redis-cli", "-p", `${settings().redis!.port}`, ...args]);
  }
  private pgUp = () =>
    run(["pg_isready", "-q", "-h", dataDir(), "-p", `${settings().pgPort}`, "-U", "postgres", "-d", "postgres"]).then(
      () => true,
      () => false,
    );
  private redisUp = () => this.redis("ping").then((r) => r.trim() === "PONG", () => false);

  shouldBeUp() {
    if (!config.db) return false;
    return [`${this.pgdata}/postmaster.pid`, `${this.redisDir}/redis.pid`].some((f) => readText(f) !== null);
  }

  async ensureUp() {
    if (!await this.pgUp()) {
      Deno.mkdirSync(dataDir(), { recursive: true });
      if (readText(`${this.pgdata}/PG_VERSION`) === null) {
        await run([
          "initdb",
          "-D",
          this.pgdata,
          "-U",
          "postgres",
          "-A",
          "trust",
          "--no-sync",
          ...settings().initdbArgs,
        ]);
      }
      const options = postgresSettings().map((s) => `-c ${s}`).join(" ");
      await run([
        "pg_ctl",
        "start",
        "-D",
        this.pgdata,
        "-l",
        `${dataDir()}/postgres.log`,
        "-w",
        "-t",
        "60",
        "-o",
        options,
      ]);
    }
    const redis = settings().redis;
    if (redis && !await this.redisUp()) {
      Deno.mkdirSync(this.redisDir, { recursive: true });
      await run([
        ...["redis-server", "--port", `${redis.port}`, "--bind", "127.0.0.1", "--dir", this.redisDir],
        ...["--save", "", "--appendonly", "no", "--databases", `${redis.databases}`, "--daemonize", "yes"],
        ...["--pidfile", `${this.redisDir}/redis.pid`, "--logfile", `${this.redisDir}/redis.log`],
      ]);
      for (let i = 0; i < 50 && !await this.redisUp(); i++) await wait(100);
      if (!await this.redisUp()) throw new Error(`redis did not answer on port ${redis.port}`);
    }
  }

  async list(): Promise<Entry[]> {
    const rows = await this.psql(
      `SELECT coalesce(json_agg(json_build_object('name', datname, 'note', shobj_description(oid, 'pg_database'))), '[]')
         FROM pg_database WHERE datname ~ '^(lane|tpl|tplb)_'`,
    );
    return (JSON.parse(rows) as { name: string; note: string | null }[]).map((r) => ({
      name: r.name,
      note: r.note ? JSON.parse(r.note) : null,
    }));
  }

  async create(name: string, note: LaneNote, template: string) {
    await this.psql(`CREATE DATABASE ${identifier(name)} TEMPLATE ${identifier(template)}`);
    await this.annotate(name, note);
  }

  async migrate(worktree: string, name: string) {
    await this.psql(`CREATE DATABASE ${identifier(name)}`);
    const command = settings().migrateCommand;
    if (command) await run(command, { cwd: worktree, env: { ...baseEnv(), ...environment(name, 0) } });
  }

  async rename(from: string, to: string) {
    await this.psql(`ALTER DATABASE ${identifier(from)} RENAME TO ${identifier(to)}`);
  }

  async annotate(name: string, note: Entry["note"]) {
    await this.psql(`COMMENT ON DATABASE ${identifier(name)} IS ${quote(JSON.stringify(note))}`);
  }

  async drop(name: string) {
    await this.psql(`DROP DATABASE IF EXISTS ${identifier(name)} WITH (FORCE)`);
  }

  async flushRedis(db: number) {
    if (settings().redis) await this.redis("-n", `${db}`, "flushdb");
  }
}
