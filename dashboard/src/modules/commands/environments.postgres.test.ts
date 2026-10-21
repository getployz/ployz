import { assert, it } from "@effect/vitest";
import { Effect } from "effect";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { mintOrganizationTokenUnchecked } from "#/modules/identity/organization-token.server";
import { presetGrant, type TokenGrant } from "#/modules/identity/token-grant";
import { organizationServerUpgrades } from "#/modules/server-upgrade/tables";
import { handleRunRequest } from "#/routes/api/cli/-handlers";
import { AuthLive } from "#/server/auth.server";
import { Database } from "#/server/database.server";
import { seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-000000002207";

/** Shop with a staging Environment, and `policy` setting its upgrade policy through `ployz` with a token of `grant`. */
const shop = Effect.fn(function* () {
  const userId = yield* seedStoreOrganization(ORGANIZATION);
  const store = yield* cloudStore;
  yield* Effect.promise(async () => {
    await store.write(ORGANIZATION, { command: "create_project", id: "00000000-0000-4000-8000-000000002208", name: "shop", default_environment: "00000000-0000-4000-8000-000000002209" });
    await store.write(ORGANIZATION, { command: "create_environment", id: "00000000-0000-4000-8000-000000002210", project: "shop", name: "staging" });
  });
  const policy = Effect.fn(function* (grant: TokenGrant, channel: "beta" | "stable") {
    const { secret } = yield* mintOrganizationTokenUnchecked({
      organizationId: ORGANIZATION, userId, name: "t", kind: "cli", expiresAt: new Date(Date.now() + 86_400_000), grant,
    });
    const response = yield* handleRunRequest(new Request("http://localhost/api/cli/run/server_upgrade_policy_set", {
      method: "POST",
      headers: { authorization: `Bearer ${secret}`, "content-type": "application/json" },
      body: JSON.stringify({ input: { automatic: false, channel } }),
    }), "server_upgrade_policy_set");
    const body = yield* Effect.promise(() => response.json() as Promise<{ error?: { message?: string } }>);
    return { status: response.status, message: body.error?.message };
  });
  const channels = Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const rows = yield* drizzle.select({ channel: organizationServerUpgrades.channel }).from(organizationServerUpgrades);
    return rows.map(({ channel }) => channel);
  });
  return { policy, channels };
});

it.live("refuses an Organization-wide command to a token held to Environments, even one whose grant holds its permission", () =>
  Effect.gen(function* () {
    const cloud = yield* storeTestCloud({ with: AuthLive });
    yield* Effect.gen(function* () {
      const { policy, channels } = yield* shop();
      const scoped = yield* policy({ permissions: ["servers:operate"], environments: ["staging"] }, "beta");
      assert.deepStrictEqual(scoped, { status: 403, message: "This token can deploy only to staging." });
      assert.deepStrictEqual(yield* channels, []);
    }).pipe(Effect.provide(cloud));
  }), 60_000);

it.live("lets a Full token set the upgrade policy, and refuses Read and Deploy tokens", () =>
  Effect.gen(function* () {
    const cloud = yield* storeTestCloud({ with: AuthLive });
    yield* Effect.gen(function* () {
      const { policy, channels } = yield* shop();
      assert.strictEqual((yield* policy(presetGrant({ preset: "full" }), "beta")).status, 200);
      assert.strictEqual((yield* policy(presetGrant({ preset: "read" }), "stable")).status, 403);
      assert.strictEqual((yield* policy(presetGrant({ preset: "deploy", environment: "staging" }), "stable")).status, 403);
      assert.deepStrictEqual(yield* channels, ["beta"]);
    }).pipe(Effect.provide(cloud));
  }), 60_000);
