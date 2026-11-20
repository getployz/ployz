import { execFile as execFileCallback } from "node:child_process";
import { randomUUID } from "node:crypto";
import { promisify } from "node:util";
import { it } from "@effect/vitest";
import { ConfigProvider, Effect, Layer, Schema } from "effect";
import { afterAll, beforeAll, expect, vi } from "vitest";
import { callStoreAsMember, readStore } from "#/modules/config-store/config-store.server";
import { mintOrganizationTokenUnchecked } from "#/modules/identity/organization-token.server";
import { member, user } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";
import { FULL_GRANT, type TokenGrant } from "#/modules/identity/token-grant";
import { handleLogsRead } from "#/routes/api/logs/-logs.handler";
import { AuthLive } from "#/server/auth.server";
import { Database } from "#/server/database.server";
import { encodePublicError, statusForPublicError } from "#/server/public-error";
import { seedStoreOrganization, storeTestCloudWith } from "#/test/store-cloud";

const execFile = promisify(execFileCallback);
const docker = (...args: ReadonlyArray<string>) => execFile("docker", args, { maxBuffer: 10 * 1024 * 1024 });

const PASSWORD = "logs-route-test";
const container = `ployz-logs-route-test-${process.pid}-${randomUUID().slice(0, 8)}`;
let clickhouse = "";

const ACME = "00000000-0000-4000-8000-0000000000c1";
const OTHER = "00000000-0000-4000-8000-0000000000d2";
const production = { project: "shop", environment: "production" };

type ReadLogsBody = { environment: typeof production; search?: string; organizationId?: string; organizationSlug?: string };
const ReadLogsAnswer = Schema.Struct({ lines: Schema.Array(Schema.Struct({ message: Schema.String })) });

beforeAll(async () => {
  await docker("run", "--detach", "--rm", "--name", container, "--publish", "127.0.0.1::8123", "--tmpfs", "/var/lib/clickhouse:rw",
    "--env", "CLICKHOUSE_USER=otel", "--env", `CLICKHOUSE_PASSWORD=${PASSWORD}`, "--env", "CLICKHOUSE_DB=otel", "clickhouse/clickhouse-server:25.8");
  const { stdout } = await docker("port", container, "8123/tcp");
  clickhouse = `http://127.0.0.1:${stdout.trim().split("\n")[0]?.split(":").at(-1)}`;
  await vi.waitFor(async () => { if (!(await fetch(new URL("/ping", clickhouse))).ok) throw new Error("not yet"); }, { timeout: 60_000, interval: 500 });
  await execFile(process.execPath, ["scripts/observe-migrate.ts"], {
    cwd: process.cwd(),
    env: { ...process.env, OBSERVE_CLICKHOUSE_URL: clickhouse, CLICKHOUSE_USER: "otel", CLICKHOUSE_PASSWORD: PASSWORD },
  });
}, 240_000);

afterAll(async () => { await docker("rm", "--force", container).catch(() => undefined); }, 60_000);

const seedLine = async (org: string, namespace: string, body: string) => {
  const target = new URL("/", clickhouse);
  target.searchParams.set("query", "INSERT INTO otel.otel_logs (Timestamp, Body, ResourceAttributes, LogAttributes) FORMAT JSONEachRow");
  const now = Date.now();
  const response = await fetch(target, {
    method: "POST",
    headers: { authorization: `Basic ${Buffer.from(`otel:${PASSWORD}`).toString("base64")}` },
    body: JSON.stringify({
      Timestamp: `${Math.floor(now / 1000) - 5}.000000000`,
      Body: body,
      ResourceAttributes: { "ployz.org": org, "ployz.machine": "a".repeat(32), "ployz.retention_days": "30" },
      LogAttributes: { "ployz.namespace": namespace, "ployz.service.name": "api", "ployz.service.id": randomUUID(), "container.id": "c-one", "log.iostream": "stdout" },
    }),
  });
  if (!response.ok) throw new Error(await response.text());
};

/** Organization `id` with a `shop/production` Environment and a member who owns it, as a Store write makes it. */
const organizationWithShop = Effect.fn(function* (id: string, slug: string, owner: string) {
  yield* callStoreAsMember({ userId: owner }, slug, {
    operation: "write", command: { command: "create_project", id: randomUUID(), name: "shop", default_environment: randomUUID() },
  });
  const view = yield* readStore(id, { query: "namespace", environment: production });
  return "namespace" in view ? view.namespace : expect.fail("no namespace");
});

it.live("reads the caller's Organization's lines only, and only with logs:read", () =>
  Effect.gen(function* () {
    const env = { OBSERVE_CLICKHOUSE_URL: clickhouse, CLICKHOUSE_USER: "otel", CLICKHOUSE_PASSWORD: PASSWORD };
    const cloud = yield* storeTestCloudWith(Layer.merge(AuthLive, ConfigProvider.layer(ConfigProvider.fromEnv({ env: {} }))), { env });
    yield* Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const acmeOwner = yield* seedStoreOrganization(ACME, { name: "Acme", slug: "acme" });
      const [bo] = yield* drizzle.insert(user).values({ email: "bo@example.test", name: "Bo" }).returning();
      const otherOwner = bo?.id ?? expect.fail("no user");
      yield* drizzle.insert(organization).values({ id: OTHER, name: "Other", slug: "other" });
      yield* drizzle.insert(member).values({ userId: otherOwner, organizationId: OTHER, role: "owner" });
      // Two Organizations' "shop/production" share one namespace, so the Organization is all that tells their lines apart.
      const namespace = yield* organizationWithShop(ACME, "acme", acmeOwner);
      expect(yield* organizationWithShop(OTHER, "other", otherOwner)).toBe(namespace);
      yield* Effect.promise(async () => {
        await seedLine(ACME, namespace, "acme order paid");
        await seedLine(OTHER, namespace, "other order paid");
      });

      const tokenFor = (organizationId: string, userId: string, grant: TokenGrant) =>
        mintOrganizationTokenUnchecked({ organizationId, userId, name: "cli", kind: "cli", expiresAt: new Date(Date.now() + 3_600_000), grant })
          .pipe(Effect.map((token) => token.secret));
      const post = (secret: string | null, body: ReadLogsBody) => {
        const headers = new Headers({ "content-type": "application/json" });
        if (secret !== null) headers.set("authorization", `Bearer ${secret}`);
        return handleLogsRead(new Request("http://localhost:3000/api/logs/read", { method: "POST", headers, body: JSON.stringify(body) })).pipe(
          Effect.flatMap((response) => Effect.promise(() => response.json()).pipe(Effect.map((json) => ({
            status: response.status, messages: Schema.decodeUnknownSync(ReadLogsAnswer)(json).lines.map((line) => line.message),
          })))),
          Effect.catch((error) => Effect.succeed({ status: statusForPublicError(encodePublicError(error)), messages: [] as string[] })),
        );
      };

      const acme = yield* tokenFor(ACME, acmeOwner, FULL_GRANT);
      const other = yield* tokenFor(OTHER, otherOwner, FULL_GRANT);
      const noLogs = yield* tokenFor(ACME, acmeOwner, { permissions: ["changes:deploy"], environments: null });

      expect(yield* post(acme, { environment: production })).toEqual({ status: 200, messages: ["acme order paid"] });
      // Other's token reads its own Environment, which holds only its own line, and the body can't name another Organization.
      expect(yield* post(other, { environment: production })).toEqual({ status: 200, messages: ["other order paid"] });
      expect(yield* post(other, { environment: production, organizationId: ACME, organizationSlug: "acme" }))
        .toEqual({ status: 200, messages: ["other order paid"] });
      expect(yield* post(other, { environment: production, search: "acme" })).toEqual({ status: 200, messages: [] });
      expect(yield* post(noLogs, { environment: production })).toEqual({ status: 403, messages: [] });
      expect(yield* post(null, { environment: production })).toEqual({ status: 401, messages: [] });
    }).pipe(Effect.provide(cloud));
  }).pipe(Effect.scoped), 240_000);
