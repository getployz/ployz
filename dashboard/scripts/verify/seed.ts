// Seeds a verification database through the app's own Config Store commands. up.sh runs it with runner.mjs, so
// `#/` imports resolve against this checkout. Typechecked with the app: a refactor that breaks it fails `pnpm typecheck`.
// Prints one line, `VERIFY_SEED <json>`: the organization slug, the session cookie, an Organization Token for the CLI
// (`cliToken`), every write's outcome and what was skipped.
//
// Ada Lovelace's organization holds project `shop`, on public images so a real Server can run it:
//   fix-api     a Branch of production (api, web) with 2 changes to save; production moved on after it branched
//   staging     a kept Branch of production (api, worker) that includes search's changes, then sets one itself
//   search      a Branch of staging, synced into it and changed again since: Details offers Include newer changes
// and project `blog`, one Service from acme/blog with pull request previews, whose production Details list:
//   #140        an offer, with a secret production lacks: Include asks to Set value
//   #141        included, still open: Awaits #141, so Save and Deploy wait with Needs #141
//   #142        included, merged into main: Ready
//   #143        included, closed: Closed
//   #144        included, retargeted to dev and merged there: Elsewhere
// A fake Server is paired so the Store admits Deploys; nothing answers it. VERIFY_REAL_SERVERS=1 leaves pairing to the
// real Servers that enroll next, so the queued Deploy and blog's pull requests are skipped: the Store admits none, and
// opens no PR Environment, before a Server joins.
import { createHash, createHmac, randomUUID } from "node:crypto";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import type { Change, ConfigCommand, PullRequest } from "@ployz/sdk";
import { callStore, storeSystem } from "#/modules/config-store/config-store.server";
import { cloudStore, storeTry } from "#/modules/config-store/store-sdk.server";
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

  // staging includes search's changes, overrides one of them, and search moves on.
  yield* write("branch staging", {
    command: "create_branch", id: randomUUID(), from: at("production"), name: "staging",
    copy: ["api", "worker"], live: [], setup: [], keep: true,
  });
  yield* write("branch search", {
    command: "create_branch", id: randomUUID(), from: at("staging"), name: "search",
    copy: ["api", "worker"], live: [], setup: [], keep: false,
  });
  yield* edit("search edits", "search", [
    { op: "set", path: "api.env.FEATURE_SEARCH", value: "on" },
    { op: "set", path: "api.env.SEARCH_URL", value: "http://search:7700" },
    { op: "set", path: "worker.env.MODE", value: "index" },
  ]);
  const review = yield* callStore(organizationId, ada.id, {
    operation: "read", query: { query: "sync", from: at("search"), into: at("staging") },
  });
  if (review.ok && review.value.view === "sync") {
    yield* write("sync search into staging", {
      command: "sync", from: at("search"), into: at("staging"), version: review.value.version, id: randomUUID(),
    });
  } else {
    writes["sync search into staging"] = review.ok ? `unexpected view ${review.value.view}` : `refused: ${review.refusal.code} ${review.refusal.message}`;
  }
  yield* edit("staging overrides search", "staging", [{ op: "set", path: "worker.env.MODE", value: "index-slow" }]);
  yield* edit("search moves on", "search", [{ op: "set", path: "api.env.SEARCH_URL", value: "http://search:7701" }]);

  // blog: pull requests offered to production and included there, one per readiness.
  const blog = (environment: string | null) => ({ project: "blog", environment });
  yield* write("project blog", { command: "create_project", id: randomUUID(), name: "blog", default_environment: randomUUID() });
  // GitHub isn't asked: the repository's evidence is handed to the Store as Cloud would observe it.
  const store = yield* cloudStore;
  const repository = { repository: "acme/blog", repository_id: 4242, access: { type: "github-installation" as const, installationId: 7 }, default_branch: "main", branches: ["dev"] };
  yield* storeTry(() => store.write(organizationId, {
    command: "create_git_service", id: randomUUID(), environment: blog(null), name: "web", repository: "acme/blog", branch: null,
  }, { repositories: [repository], domains: { cluster_domain: null, certificates: null, ingress_addresses: [], lookups: [] } })).pipe(
    Effect.as("ok"), Effect.catchTag("StoreRefused", (refused) => Effect.succeed(`refused: ${refused.code} ${refused.message}`)),
    Effect.tap((outcome) => Effect.sync(() => { writes["blog service web"] = outcome; })),
  );
  yield* write("blog publish", { command: "publish", environment: blog("production"), version: null });
  yield* write("blog previews", {
    command: "set_pr_plan", project: "blog", repository: "acme/blog", enabled: true, start_from: "production",
    copy: null, setup: null, remove_on_close: null, include_bots: null,
  });
  const pullRequest = (number: number, second: number, facts: Partial<PullRequest> = {}): PullRequest => ({
    repository_id: 4242, number, title: `Change ${number}`, author: "ada", bot: false, head_branch: `change-${number}`,
    head: String(number).repeat(14).slice(0, 40), target_branch: "main", commits: 1, open: true,
    merge_commit: null, updated: `2026-10-01T10:00:${String(second).padStart(2, "0")}Z`, ...facts,
  });
  const observe = (label: string, facts: PullRequest) => storeSystem(organizationId, { event: "pull_request", ...facts }).pipe(
    Effect.as("ok"), Effect.catchTag("StoreRefused", (refused) => Effect.succeed(`refused: ${refused.code} ${refused.message}`)),
    Effect.tap((outcome) => Effect.sync(() => { writes[label] = outcome; })),
  );
  // The Store opens a PR Environment only when a Server could run it.
  if (process.env["VERIFY_REAL_SERVERS"] === "1") {
    skipped.push("blog pull requests: no Server has enrolled yet, so no PR Environment opens");
  } else {
    const numbers = [140, 141, 142, 143, 144] as const;
    for (const number of numbers) {
      yield* observe(`blog #${number} opens`, pullRequest(number, 0));
      const changes: Change[] = [{ op: "set", path: `web.env.CHANGE_${number}`, value: "on" }];
      if (number === 140) changes.push({ op: "set", path: "web.env.TOKEN", value: { secret: "preview-token" } });
      yield* write(`blog pr-${number} edits`, { command: "edit", environment: blog(`pr-${number}`), expect: null, changes });
      // A Merge-menu Sync: from a PR Environment into its Destination, it's offered there.
      const review = yield* callStore(organizationId, ada.id, { operation: "read", query: { query: "sync", from: blog(`pr-${number}`), into: null } });
      if (review.ok && review.value.view === "sync") {
        yield* write(`blog #${number} offered`, { command: "sync", from: blog(`pr-${number}`), version: review.value.version, id: randomUUID() });
      } else {
        writes[`blog #${number} offered`] = review.ok ? `unexpected view ${review.value.view}` : `refused: ${review.refusal.code} ${review.refusal.message}`;
      }
    }
    for (const number of numbers.filter((number) => number !== 140)) {
      const diff = yield* callStore(organizationId, ada.id, { operation: "read", query: { query: "diff", environment: blog("production") } });
      const offer = diff.ok && diff.value.view === "diff"
        ? diff.value.included.find(({ source }) => source.kind === "pull_request" && source.number === number) : undefined;
      if (!diff.ok || diff.value.view !== "diff" || offer === undefined) {
        writes[`blog #${number} included`] = diff.ok ? `no offer for #${number}` : `refused: ${diff.refusal.code} ${diff.refusal.message}`;
        continue;
      }
      yield* write(`blog #${number} included`, {
        command: "include_proposal", environment: blog("production"), proposal: offer.proposal, version: diff.value.version,
      });
    }
    yield* observe("blog #142 merges", pullRequest(142, 1, { open: false, merge_commit: "4".repeat(40) }));
    yield* observe("blog #143 closes", pullRequest(143, 1, { open: false }));
    yield* observe("blog #144 retargets", pullRequest(144, 1, { target_branch: "dev" }));
    yield* observe("blog #144 merges into dev", pullRequest(144, 2, { open: false, target_branch: "dev", merge_commit: "5".repeat(40) }));

  }

  return { organizationSlug, cookie, cliToken, writes, skipped };
});

export async function run() {
  const result = await AppRuntime.runPromise(seed);
  console.log(`VERIFY_SEED ${JSON.stringify(result)}`);
  const refused = Object.entries(result.writes).filter(([, outcome]) => outcome !== "ok");
  if (refused.length) throw new Error(`seed writes refused: ${JSON.stringify(Object.fromEntries(refused))}`);
}
