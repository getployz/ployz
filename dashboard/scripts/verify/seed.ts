// Seeds a verification database through the app's own Config Store commands. up.sh runs it with runner.mjs, so
// `#/` imports resolve against this checkout. Typechecked with the app: a refactor that breaks it fails `pnpm typecheck`.
// Prints one line, `VERIFY_SEED <json>`: the organization slug, the session cookie, an Organization Token for the CLI
// (`cliToken`), every write's outcome and what was skipped.
//
// Ada Lovelace's organization holds project `shop`, on public images so a real Server can run it:
//   fix-api     a Branch of production (api, web) with 2 changes to save; production moved on after it branched
// A fake Server is paired so the Store admits Deploys; nothing answers it. VERIFY_REAL_SERVERS=1 leaves pairing to the
// real Servers that enroll next, so the queued Deploy is skipped: the Store admits none before a Server joins.
import { createHash, createHmac, randomUUID } from "node:crypto";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import type { Change, ConfigCommand } from "@ployz/sdk";
import { callStore } from "#/modules/config-store/config-store.server";
import { createOrganizationToken } from "#/modules/identity/organization-token.server";
import { session, user } from "#/modules/identity/tables";
import { organizationMachine } from "#/modules/machines/tables";
import { ensurePersonalOrganizationForUser } from "#/modules/organization/organization-state.server";
import { organization } from "#/modules/organization/tables";
import { organizationPairing } from "#/modules/runtime/tables";
import { Database } from "#/server/database.server";
import { AppRuntime } from "#/server/runtime.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";

const MACHINE_ID = "0123456789abcdef0123456789abcdef";
const env = (name: string) => {
  const value = process.env[name];
  if (!value) throw new Error(`seed.ts needs ${name}`);
  return value;
};

const seed = Effect.gen(function* () {
  const { drizzle } = yield* Database;

  // Ada signs in: the user row better-auth would write, then its create hook's personal organization.
  const [ada] = yield* drizzle.insert(user).values({ email: "ada@example.com", emailVerified: true, name: "Ada Lovelace" }).returning();
  if (!ada) return yield* Effect.die("No user.");
  const organizationId = yield* ensurePersonalOrganizationForUser(ada);
  const [org] = yield* drizzle.select({ slug: organization.slug }).from(organization).where(eq(organization.id, organizationId));
  const organizationSlug = org?.slug ?? "";

  // A session better-auth accepts: its row plus the HMAC-signed `better-auth.session_token` cookie.
  const token = env("VERIFY_SESSION_TOKEN");
  const [signedIn] = yield* drizzle.insert(session).values({
    userId: ada.id, token, expiresAt: new Date(Date.now() + 30 * 24 * 3600_000),
    activeOrganizationId: organizationId, activeOrganizationSlug: organizationSlug,
  }).returning({ id: session.id });
  if (!signedIn) return yield* Effect.die("No session.");
  const cookie = encodeURIComponent(`${token}.${createHmac("sha256", env("BETTER_AUTH_SECRET")).update(token).digest("base64")}`);
  const { secret: cliToken } = yield* createOrganizationToken(
    { userId: ada.id, organization: { id: organizationId, slug: organizationSlug }, credential: { kind: "session", id: signedIn.id } },
    { name: "verify", expiresInDays: 30 },
  );

  const writes: Record<string, string> = {};
  const skipped: string[] = [];
  const write = (label: string, command: ConfigCommand) =>
    callStore(organizationId, ada.id, { operation: "write", command }).pipe(
      Effect.tap((r) => Effect.sync(() => { writes[label] = r.ok ? "ok" : `refused: ${r.refusal.code} ${r.refusal.message}`; })),
    );
  const at = (environment: string | null) => ({ project: "shop", environment });
  const edit = (label: string, environment: string, changes: Change[]) =>
    write(label, { command: "edit", environment: at(environment), expect: null, changes });

  yield* write("project shop", { command: "create_project", id: randomUUID(), name: "shop", default_environment: randomUUID() });
  for (const [name, image] of [["web", "nginx:1.27-alpine"], ["api", "traefik/whoami:v1.10.3"], ["postgres", "postgres:16"], ["worker", "traefik/whoami:v1.10.3"], ["redis", "redis:7-alpine"]] as const) {
    yield* write(`service ${name}`, { command: "create_service", id: randomUUID(), environment: at(null), name, image });
  }
  yield* write("volume pg-data", {
    command: "create_volume", id: randomUUID(), environment: at(null), name: "pg-data",
    storage: { kind: "provisioned", maximumBytes: 5_000_000_000 }, mounts: [{ service: "postgres", path: "/var/lib/postgresql/data" }],
  });
  yield* write("domain web", { command: "add_domain", environment: at(null), service: "web", hostname: "acme.com", port: null });
  yield* edit("production env", "production", [
    { op: "set", path: "api.env.LOG_LEVEL", value: "warn" },
    { op: "set", path: "api.env.DATABASE_URL", value: "postgres://postgres@postgres:5432/shop" },
    { op: "set", path: "postgres.env.POSTGRES_PASSWORD", value: "postgres" },
    { op: "set", path: "api.env.SECRET_KEY", value: { secret: "verify-secret-key" } },
    { op: "set", path: "api.env.SECRET_KEY.exported", value: true },
  ]);
  yield* write("config sentry", {
    command: "create_config", id: randomUUID(), environment: at("production"), name: "sentry",
    mounts: [{ service: "web", dir: "/etc/sentry" }, { service: "worker", dir: "/etc/sentry" }],
  });
  for (const [file, content] of [
    ["config.yml", "redis:\n  host: ${{ redis.PLOYZ_PRIVATE_DOMAIN }}\n  port: ${{ redis.PORT }}\nsecret_key: ${{ api.SECRET_KEY }}\n"],
    ["sentry.conf.py", "SENTRY_OPTIONS = {\n    \"system.url-prefix\": \"https://sentry.acme.com\",\n}\n"],
  ] as const) {
    yield* write(`config sentry ${file}`, { command: "put_config_file", environment: at("production"), config: "sentry", file, content });
  }

  if (process.env["VERIFY_REAL_SERVERS"] === "1") {
    skipped.push("deploy production: no Server has enrolled yet");
  } else {
    // The fake Server: a pairing and one Machine row, so the Store admits Deploys.
    const encryption = yield* SecretEncryption;
    const pairingSecret = "ppair_verify";
    yield* drizzle.insert(organizationPairing).values({
      organizationId, encryptedPairingSecret: encryption.encrypt(pairingSecret),
      founderClaimMachineId: MACHINE_ID as never, founderMachineId: MACHINE_ID as never,
    });
    yield* drizzle.insert(organizationMachine).values({
      organizationId, machineId: MACHINE_ID as never, clusterKey: createHash("sha256").update(pairingSecret).digest("hex"),
      encryptedCapability: encryption.encrypt("cap"),
    });
    yield* write("deploy production", {
      command: "admit", admit: "deploy", id: randomUUID(), environment: at("production"),
      services: [], version: null, accept_volume_loss: [], message: "Ship everything",
    });
  }

  // A Branch with its own edits to save back; production then moves on, leaving unpublished edits on its canvas.
  yield* write("branch fix-api", {
    command: "create_branch", id: randomUUID(), from: at("production"), name: "fix-api",
    copy: ["api", "web"], live: [], setup: [], keep: false,
  });
  yield* edit("fix-api edits", "fix-api", [
    { op: "set", path: "api.env.LOG_LEVEL", value: "debug" },
    { op: "set", path: "web.startCommand", value: "npm run serve" },
  ]);
  yield* edit("production moves on", "production", [
    { op: "set", path: "api.image", value: "traefik/whoami:v1.11.0" },
    { op: "set", path: "api.env.FEATURE_SEARCH", value: "on" },
  ]);

  return { organizationSlug, cookie, cliToken, writes, skipped };
});

export async function run() {
  const result = await AppRuntime.runPromise(seed);
  console.log(`VERIFY_SEED ${JSON.stringify(result)}`);
  const refused = Object.entries(result.writes).filter(([, outcome]) => outcome !== "ok");
  if (refused.length) throw new Error(`seed writes refused: ${JSON.stringify(Object.fromEntries(refused))}`);
}
