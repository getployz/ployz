import { assert, it } from "@effect/vitest";
import type { ConfigCommand, JsonValue, SealingTargetsQuery } from "@ployz/sdk";
import { createHash, generateKeyPairSync } from "node:crypto";
import { sql } from "drizzle-orm";
import { Effect, Schema } from "effect";
import { viewOf } from "#/modules/config-store/store.contract";
import { mintOrganizationTokenUnchecked } from "#/modules/identity/organization-token.server";
import { recordSealingPublicKey } from "#/modules/sealing/sealing-key.server";
import { handleConfigRequest } from "#/routes/api/config/-config.handler";
import { AuthLive } from "#/modules/identity/auth.server";
import { Database } from "#/server/database.server";
import { sealedSecret } from "#/test/sealed-secret";
import { acmeWeb, enrollStoreServer, seedStoreGitService, seedStoreOrganization, storeTestCloudWith } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-000000005e01";
const PROJECT = "00000000-0000-4000-8000-000000005e02";
const ENVIRONMENT = "00000000-0000-4000-8000-000000005e03";
const SERVICE = "00000000-0000-4000-8000-000000005e04";
const here = { project: null, environment: null };
const DEPLOYMENT = "00000000-0000-4000-8000-000000005e05";
const PAIRING_SECRET = "ppair_fixture_store";
const PUBLIC_KEY = `${"A".repeat(43)}=`;
const UPDATE = "curl -fsSL https://ployz.sh | sh";

/** What `/api/config/sealing` answers, and how a write is refused. */
const Answer = Schema.Union([
  Schema.Struct({ state: Schema.Literal("no_server") }),
  Schema.Struct({
    state: Schema.Literal("ready"),
    publicKey: Schema.String,
    targets: Schema.Struct({
      view: Schema.String,
      target: Schema.String,
      organization: Schema.String,
      environment: Schema.Struct({ id: Schema.String, project: Schema.String, name: Schema.String }),
      services: Schema.Array(Schema.Struct({ name: Schema.String, service: Schema.String })),
    }),
  }),
  Schema.Struct({ error: Schema.Struct({ code: Schema.String, message: Schema.String, details: Schema.Unknown }) }),
  Schema.Struct({ written: Schema.String, staged: Schema.Array(Schema.String) }),
  Schema.Struct({ created: Schema.Boolean }),
]);
/** Checks the fields the tests read and keeps the rest, so whole-reply assertions still see every field. */
const decodeAnswer = (text: string) => Schema.decodeUnknownSync(Answer)(JSON.parse(text), { onExcessProperty: "preserve" });

/** What the CLI sends, plus one body no operation takes. */
type Body = ConfigCommand | SealingTargetsQuery | { readonly query: "environment" };

const set = (key: string, value: JsonValue): ConfigCommand => ({
  command: "edit", environment: here, expect: null, changes: [{ op: "set", path: `web.env.${key}`, value }],
});

/** Shop with Git Service `web`, reached as a CLI token of its owner, or with `kind` an agent token. */
const shop = Effect.fn(function* (kind: "cli" | "agent" = "cli") {
  const userId = yield* seedStoreOrganization(ORGANIZATION);
  const store = yield* seedStoreGitService(ORGANIZATION, { project: PROJECT, environment: ENVIRONMENT, service: SERVICE });
  const { secret } = yield* mintOrganizationTokenUnchecked({
    organizationId: ORGANIZATION, userId, name: "t", kind, expiresAt: new Date(Date.now() + 86_400_000),
  });
  const send = Effect.fn(function* (operation: string, body: Body, approval?: string) {
    const headers = new Headers({ authorization: `Bearer ${secret}`, "content-type": "application/json" });
    if (approval !== undefined) headers.set("x-ployz-approval", approval);
    const response = yield* handleConfigRequest(new Request(`http://localhost:3000/api/config/${operation}`, {
      method: "POST", headers, body: JSON.stringify(body),
    }));
    const text = yield* Effect.promise(() => response.text());
    return { status: response.status, cache: response.headers.get("cache-control"), json: decodeAnswer(text) };
  });
  const serverReported = (publicKey = PUBLIC_KEY) => Effect.gen(function* () {
    yield* enrollStoreServer(ORGANIZATION);
    yield* recordSealingPublicKey(ORGANIZATION, createHash("sha256").update(PAIRING_SECRET).digest("hex"), publicKey);
  });
  /** The default Environment's settings, by path, as the Store shows them. */
  const settings = Effect.promise(async () => {
    const query = { query: "environment", environment: here, path: null, all: false } as const;
    const view = viewOf(await store.read(ORGANIZATION, query), query) ?? assert.fail("no environment view");
    return Object.fromEntries(view.settings.map((setting) => [setting.path, setting.value]));
  });
  /** `web` deployed, then removed from the Config, so the next Save stops it. */
  const webDeployedThenRemoved = Effect.gen(function* () {
    yield* enrollStoreServer(ORGANIZATION);
    yield* Effect.promise(() => store.write(ORGANIZATION, {
      command: "admit", admit: "deploy", id: DEPLOYMENT, environment: here, services: [], version: null, accept_volume_loss: [],
    }, acmeWeb()));
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`update config_deployment set status = 'applied' where id = ${DEPLOYMENT}`);
    yield* drizzle.execute(sql`insert into config_applied (environment_id, node_id, organization_id, deployment_id, node_type, node)
      select environment_id, service->>'id', organization_id, ${DEPLOYMENT}, 'service', service::text
      from config_saved, lateral jsonb_array_elements(intent::jsonb->'services') service where environment_id = ${ENVIRONMENT}`);
    yield* Effect.promise(() => store.write(ORGANIZATION, { command: "remove_service", environment: here, service: "web" }));
  });
  /** The ids of the Services the Environment's last Save holds. */
  const saved = Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const rows = yield* drizzle.execute<{ id: string }>(sql`select service->>'id' as id
      from config_saved s, lateral jsonb_array_elements(s.intent::jsonb->'services') service
      where s.environment_id = ${ENVIRONMENT} and s.revision = (select max(revision) from config_saved where environment_id = ${ENVIRONMENT})`, "objects");
    return rows.map((row) => row.id);
  });
  /** Put `web` back as Head has it, so the next Save stops nothing. */
  const webKept = Effect.promise(() => store.write(ORGANIZATION, { command: "discard", environment: here, path: "web", target: "head", version: null }));
  return { send, serverReported, settings, webDeployedThenRemoved, webKept, saved, userId };
});

const cloud = storeTestCloudWith(AuthLive);
const edit: Body = { target: "edit", environment: here };

it.live("tells the CLI whether to seal: no server, then ready with where each secret lands", () =>
  Effect.gen(function* () {
    yield* Effect.gen(function* () {
      const { send, serverReported } = yield* shop();
      const waiting = yield* send("sealing", edit);
      assert.deepStrictEqual([waiting.status, waiting.cache, waiting.json], [200, "no-store", { state: "no_server" }]);
      yield* serverReported();
      const ready = yield* send("sealing", edit);
      assert.strictEqual(ready.status, 200);
      if (!("state" in ready.json) || ready.json.state !== "ready") assert.fail(`not ready: ${JSON.stringify(ready.json)}`);
      assert.strictEqual(ready.json.publicKey, PUBLIC_KEY);
      const { targets } = ready.json;
      assert.deepStrictEqual([targets.view, targets.target, targets.organization], ["sealing_targets", "edit", ORGANIZATION]);
      assert.deepStrictEqual([targets.environment.id, targets.environment.project, targets.environment.name], [ENVIRONMENT, "shop", "production"]);
      assert.deepStrictEqual(targets.services, [{ name: "web", service: SERVICE }]);
      const refused = yield* send("sealing", { query: "environment" });
      assert.deepStrictEqual([refused.status, refused.json], [422, { error: { code: "invalid_argument", message: "Expected a sealing target.", details: null } }]);
    }).pipe(Effect.provide(yield* cloud));
  }), 60_000);

it.live("a value the CLI sealed is saved and its plain text is refused unread, with the update to run", () =>
  Effect.gen(function* () {
    yield* Effect.gen(function* () {
      const { send, serverReported, settings } = yield* shop();
      yield* serverReported();
      const saved = yield* send("write", set("TOKEN", sealedSecret({ environment: ENVIRONMENT, service: SERVICE }, "TOKEN")));
      assert.deepStrictEqual([saved.status, "staged" in saved.json && saved.json.staged], [200, ["web.env.TOKEN"]]);
      const refused = yield* send("write", set("OTHER", { secret: "plain-s3cr3t" }));
      assert.deepStrictEqual([refused.status, refused.json], [501, {
        error: { code: "unsupported", message: "OTHER: update ployz to set a Sealed value.", details: { variable: "OTHER", next: UPDATE, follow_up: { kind: "update_client", beta: false } } },
      }]);
      const after = yield* settings;
      assert.deepStrictEqual(after["web.env.TOKEN"], { secret: true });
      assert.notProperty(after, "web.env.OTHER");
      assert.notInclude(JSON.stringify(after), "plain-s3cr3t");
    }).pipe(Effect.provide(yield* cloud));
  }), 60_000);

it.live("a generated secret waits for a Server, then is sealed to the Cluster's key", () =>
  Effect.gen(function* () {
    yield* Effect.gen(function* () {
      const { send, serverReported, settings } = yield* shop();
      const generate = set("PASSWORD", { secret: { generate: true } });
      const waiting = yield* send("write", generate);
      assert.deepStrictEqual([waiting.status, waiting.json], [409, {
        error: { code: "conflict", message: "Add a server to set sealed values.", details: { variable: "PASSWORD" } },
      }]);
      assert.notProperty(yield* settings, "web.env.PASSWORD");

      const jwk = generateKeyPairSync("x25519").publicKey.export({ format: "jwk" });
      yield* serverReported(Buffer.from(jwk.x ?? "", "base64url").toString("base64"));
      const saved = yield* send("write", generate);
      assert.deepStrictEqual([saved.status, "staged" in saved.json && saved.json.staged], [200, ["web.env.PASSWORD"]]);
      assert.deepStrictEqual((yield* settings)["web.env.PASSWORD"], { secret: true });
    }).pipe(Effect.provide(yield* cloud));
  }), 60_000);

const save: ConfigCommand = { command: "publish", environment: here, version: null, accept_volume_loss: [] };

// Catches an agent token running a Save that stops web by resending the digest the Store asked a `ployz` user to say yes to.
it.live("an agent's Save that stops a Service is refused with where a person does it, and a person's digest changes nothing", () =>
  Effect.gen(function* () {
    yield* Effect.gen(function* () {
      const { send, webDeployedThenRemoved, saved, userId } = yield* shop("agent");
      yield* webDeployedThenRemoved;
      const remedy = { error: { code: "forbidden", message: "This save needs a person. Do it in Ployz Cloud (or with ployz).", details: null } };
      const asked = yield* send("write", save);
      assert.deepStrictEqual([asked.status, asked.json], [403, remedy]);

      const { secret } = yield* mintOrganizationTokenUnchecked({
        organizationId: ORGANIZATION, userId, name: "person", kind: "cli", expiresAt: new Date(Date.now() + 86_400_000),
      });
      const person = (approval?: string) => Effect.gen(function* () {
        const headers = new Headers({ authorization: `Bearer ${secret}`, "content-type": "application/json" });
        if (approval !== undefined) headers.set("x-ployz-approval", approval);
        const response = yield* handleConfigRequest(new Request("http://localhost:3000/api/config/write", { method: "POST", headers, body: JSON.stringify(save) }));
        return { status: response.status, text: yield* Effect.promise(() => response.text()) };
      });
      const ask = yield* person().pipe(Effect.map(({ text }) => ({ json: decodeAnswer(text) })));
      if (!("error" in ask.json) || ask.json.error.code !== "approval_required") assert.fail(`no ask: ${JSON.stringify(ask.json)}`);
      assert.isTrue(ask.json.error.message.endsWith(". If ployz didn't ask yes or no, update it."), ask.json.error.message);
      const digest = Schema.decodeUnknownSync(Schema.Struct({ approval: Schema.String }))(ask.json.error.details).approval;

      const resent = yield* send("write", save, digest);
      assert.deepStrictEqual([resent.status, resent.json], [403, {
        error: { code: "forbidden", message: "An agent can't say yes. Do it in Ployz Cloud (or with ployz).", details: null },
      }]);
      assert.deepStrictEqual(yield* saved, [SERVICE], "nothing the agent sent was saved");

      const yes = yield* person(digest);
      assert.strictEqual(yes.status, 200, yes.text);
      assert.deepStrictEqual(yield* saved, [], "the person's yes saves it");
    }).pipe(Effect.provide(yield* cloud));
  }), 60_000);

// Catches a yes to stopping web publishing whatever was staged after the ask, once that plan stops nothing.
it.live("a person's yes to a Save that has changed since the ask asks again and saves nothing", () =>
  Effect.gen(function* () {
    yield* Effect.gen(function* () {
      const { send, webDeployedThenRemoved, webKept, saved } = yield* shop();
      yield* webDeployedThenRemoved;
      const ask = yield* send("write", save);
      if (!("error" in ask.json) || ask.json.error.code !== "approval_required") assert.fail(`no ask: ${JSON.stringify(ask.json)}`);
      const digest = Schema.decodeUnknownSync(Schema.Struct({ approval: Schema.String }))(ask.json.error.details).approval;

      yield* webKept;
      const staged = yield* send("write", set("MODE", "fast"));
      assert.strictEqual(staged.status, 200);
      const stale = yield* send("write", save, digest);
      if (!("error" in stale.json) || stale.json.error.code !== "approval_required") assert.fail(`no new ask: ${JSON.stringify(stale.json)}`);
      const fresh = Schema.decodeUnknownSync(Schema.Struct({ approval: Schema.String, effects: Schema.Array(Schema.Unknown) }))(stale.json.error.details);
      assert.notStrictEqual(fresh.approval, digest);
      assert.deepStrictEqual(fresh.effects, []);
      assert.deepStrictEqual(yield* saved, [SERVICE], "the changed plan was not saved");

      const yes = yield* send("write", save, fresh.approval);
      assert.deepStrictEqual([yes.status, "created" in yes.json && yes.json.created], [200, true]);
    }).pipe(Effect.provide(yield* cloud));
  }), 60_000);
