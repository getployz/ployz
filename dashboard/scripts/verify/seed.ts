// Seeds a verification database through the app's own Config Store commands. up.sh runs it with runner.mjs, so
// `#/` imports resolve against this checkout. Typechecked with the app: a refactor that breaks it fails `pnpm typecheck`.
// Prints one line, `VERIFY_SEED <json>`: the organization slug, a session cookie and an Organization Token for the CLI per
// person (Ada's `cookie` and `cliToken`, `graceCookie` and `graceToken`, `linusCookie` and `linusToken`), every write's
// outcome and what was skipped.
//
// Ada Lovelace owns organization `ada`; Grace Hopper is an admin and Linus Torvalds a member there. It holds project
// `shop`, on public images so a real Server can run it:
//   fix-api     a Branch of production (api, web) with 2 changes to save; production moved on after it branched
//   staging     a kept Branch of production (api, worker) that includes search's changes, then sets one itself
//   search      a Branch of staging, synced into it and changed again since: Details offers Include newer changes
// and project `blog`, one Service from acme/blog with pull request previews, a PR Environment per pull request:
//   #140        open, with a secret production lacks
//   #141        open
//   #142        merged into main
//   #143        closed
//   #144        retargeted to dev and merged there
//   #146        open: its On merge list lands API_URL and CACHE_TTL (over production's own change since), excludes
//               LOG_LEVEL, and lands TOKEN Not set
// A fake Server is paired so the Store admits Deploys; nothing answers it, so production's Deploy stays queued and the
// seed writes its nodes into Applied State, as a finished one would. Its Sealing Key is a throwaway one no Server holds.
// VERIFY_REAL_SERVERS=1 leaves pairing to the real Servers that enroll next, so the queued Deploy and blog's pull
// requests are skipped: the Store admits none, and opens no PR Environment, before a Server joins. SECRET_KEY waits for
// their Sealing Key: up.sh writes it with seed-sealed.ts once they enroll.
import { createHash, createHmac, generateKeyPairSync, randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
import { eq, sql } from "drizzle-orm";
import { hashPassword } from "better-auth/crypto";
import { Effect } from "effect";
import type { Change, ConfigCommand, PullRequest, SystemEvent } from "@ployz/sdk";
import { callStore, storeSystem } from "#/modules/config-store/config-store.server";
import { cloudStore, storeTry } from "#/modules/config-store/store-sdk.server";
import { linkGithubInstallation, upsertCachedGithubRepositories, upsertGithubInstallation } from "#/modules/github/github.repository";
import { createOrganizationToken } from "#/modules/identity/organization-token.server";
import { InngestClient, type PloyzInngest } from "#/modules/inngest/client";
import { account, member, session, user } from "#/modules/identity/tables";
import { clusterSealingKey, organizationMachine } from "#/modules/machines/tables";
import { ensurePersonalOrganizationForUser } from "#/modules/organization/organization-state.server";
import { permissionsOf } from "#/modules/organization/permissions";
import { organization } from "#/modules/organization/tables";
import { organizationPairing } from "#/modules/runtime/tables";
import { Database } from "#/server/database.server";
import { AppRuntime } from "#/server/runtime.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import { sealed, secretKey } from "./seed-sealed";

const BOB_PASSWORD = "verify-bob-password";
const ADA_GITHUB_ID = 9001;
const MACHINE_ID = "0123456789abcdef0123456789abcdef";
const env = (name: string) => {
  const value = process.env[name];
  if (!value) throw new Error(`seed.ts needs ${name}`);
  return value;
};

type FakeGithub = {
  users: Array<{ id: number; login: string; email: string }>;
  installations: Array<{
    id: number; account: { login: string; type: string }; users: string[];
    repositories: Array<{ id: number; name: string; private: boolean; default_branch: string; awaitsGrant?: boolean }>;
  }>;
};

const linkFakeGithub = (signedIn: { id: string; email: string }, organizationId: string) => Effect.gen(function* () {
  const { drizzle } = yield* Database;
  const fixture = JSON.parse(readFileSync(".verify/github.json", "utf8")) as FakeGithub;
  const github = fixture.users.find((u) => u.email === signedIn.email);
  if (!github) return;
  yield* drizzle.insert(account).values({ userId: signedIn.id, providerId: "github", accountId: String(github.id) });
  const origin = env("PLOYZ_VERIFY_GITHUB_ORIGIN");
  const syncedAt = new Date();
  for (const installation of fixture.installations.filter((i) => i.users.includes(github.login))) {
    const owner = installation.account.login;
    yield* upsertGithubInstallation({
      installationId: installation.id, accountLogin: owner, accountType: installation.account.type, accountAvatarUrl: `${origin}/${owner}.png`,
    });
    yield* linkGithubInstallation({ organizationId, installationId: installation.id, connectedByUserId: signedIn.id, connectedGithubLogin: github.login });
    yield* upsertCachedGithubRepositories({
      installationId: installation.id, syncedAt,
      repositories: installation.repositories.filter((r) => !r.awaitsGrant).map((r) => ({
        id: r.id, name: r.name, full_name: `${owner}/${r.name}`, private: r.private, default_branch: r.default_branch,
        html_url: `${origin}/${owner}/${r.name}`, repo_updated_at: syncedAt.toISOString(),
      })),
    });
  }
});

const seedBob = Effect.gen(function* () {
  const { drizzle } = yield* Database;
  const [bob] = yield* drizzle.insert(user).values({ email: "bob@example.com", emailVerified: true, name: "Bob Bobson" }).returning();
  if (!bob) return yield* Effect.die("No user.");
  const organizationId = yield* ensurePersonalOrganizationForUser(bob);
  const password = yield* Effect.promise(() => hashPassword(BOB_PASSWORD));
  yield* drizzle.insert(account).values({ userId: bob.id, providerId: "credential", accountId: bob.id, password });
  yield* linkFakeGithub(bob, organizationId);
  return { email: bob.email, password: BOB_PASSWORD };
});

const seed = Effect.gen(function* () {
  const { drizzle } = yield* Database;

  // Ada signs in: the user row better-auth would write, then its create hook's personal organization.
  const [ada] = yield* drizzle.insert(user).values({ email: "ada@example.com", emailVerified: true, name: "Ada Lovelace" }).returning();
  if (!ada) return yield* Effect.die("No user.");
  const organizationId = yield* ensurePersonalOrganizationForUser(ada);
  const [org] = yield* drizzle.select({ slug: organization.slug }).from(organization).where(eq(organization.id, organizationId));
  const organizationSlug = org?.slug ?? "";
  const bob = process.env["PLOYZ_VERIFY_GITHUB_ORIGIN"] ? yield* linkFakeGithub(ada, organizationId).pipe(Effect.andThen(seedBob)) : null;
  // Ada signed in with GitHub, so her pull requests deploy on their own.
  if (!process.env["PLOYZ_VERIFY_GITHUB_ORIGIN"]) {
    yield* drizzle.insert(account).values({ userId: ada.id, providerId: "github", accountId: String(ADA_GITHUB_ID) });
  }

  // A session better-auth accepts in Ada's organization: its row plus the HMAC-signed `better-auth.session_token` cookie.
  const signIn = (userId: string, role: "owner" | "admin" | "member", token: string) => Effect.gen(function* () {
    const [signedIn] = yield* drizzle.insert(session).values({
      userId, token, expiresAt: new Date(Date.now() + 30 * 24 * 3600_000),
      activeOrganizationId: organizationId, activeOrganizationSlug: organizationSlug,
    }).returning({ id: session.id });
    if (!signedIn) return yield* Effect.die("No session.");
    const cookie = encodeURIComponent(`${token}.${createHmac("sha256", env("BETTER_AUTH_SECRET")).update(token).digest("base64")}`);
    const { secret } = yield* createOrganizationToken(
      { userId, organization: { id: organizationId, slug: organizationSlug }, credential: { kind: "session", id: signedIn.id }, role, support: null, via: "dashboard" },
      { name: "verify", expiresInDays: 30 },
    );
    return { cookie, cliToken: secret };
  });
  const token = env("VERIFY_SESSION_TOKEN");
  const { cookie, cliToken } = yield* signIn(ada.id, "owner", token);
  // Colleagues sign up with their own personal organization, then join Ada's. Their session tokens derive from Ada's, so
  // their cookies also keep their values across a reseed.
  const colleague = (name: string, email: string, role: "admin" | "member") => Effect.gen(function* () {
    const [person] = yield* drizzle.insert(user).values({ email, emailVerified: true, name }).returning();
    if (!person) return yield* Effect.die("No user.");
    yield* ensurePersonalOrganizationForUser(person);
    yield* drizzle.insert(member).values({ userId: person.id, organizationId, role });
    return yield* signIn(person.id, role, createHash("sha256").update(`${token}:${email}`).digest("hex"));
  });
  const grace = yield* colleague("Grace Hopper", "grace@example.com", "admin");
  const linus = yield* colleague("Linus Torvalds", "linus@example.com", "member");

  const writes: Record<string, string> = {};
  const skipped: string[] = [];
  const write = (label: string, command: ConfigCommand) =>
    callStore(organizationId, ada.id, { operation: "write", command }, { source: "dashboard" }, { approval: "not_required", permissions: permissionsOf("owner") }).pipe(
      Effect.tap((r) => Effect.sync(() => { writes[label] = r.ok ? "ok" : `refused: ${r.refusal.code} ${r.refusal.message}`; })),
    );
  const at = (environment: string | null) => ({ project: "shop", environment });
  const edit = (label: string, environment: string, changes: Change[]) =>
    write(label, { command: "edit", environment: at(environment), expect: null, changes });

  if (process.env["VERIFY_REAL_SERVERS"] !== "1") {
    // The fake Server: a pairing, one Machine row and its Cluster's Sealing Key, so the Store admits Deploys.
    const encryption = yield* SecretEncryption;
    const pairingSecret = "ppair_verify";
    const clusterKey = createHash("sha256").update(pairingSecret).digest("hex");
    yield* drizzle.insert(organizationPairing).values({
      organizationId, encryptedPairingSecret: encryption.encrypt(pairingSecret),
      founderClaimMachineId: MACHINE_ID as never, founderMachineId: MACHINE_ID as never,
    });
    yield* drizzle.insert(organizationMachine).values({
      organizationId, machineId: MACHINE_ID as never, clusterKey, encryptedCapability: encryption.encrypt("cap"),
    });
    const publicKey = Buffer.from(generateKeyPairSync("x25519").publicKey.export({ format: "jwk" }).x ?? "", "base64url").toString("base64");
    yield* drizzle.insert(clusterSealingKey).values({ organizationId, clusterKey, publicKey });
  }

  yield* write("project shop", { command: "create_project", id: randomUUID(), name: "shop", default_environment: randomUUID() });
  for (const [name, image] of [["web", "nginx:1.27-alpine"], ["api", "traefik/whoami:v1.10.3"], ["postgres", "postgres:16"], ["worker", "traefik/whoami:v1.10.3"], ["redis", "redis:7-alpine"]] as const) {
    yield* write(`service ${name}`, { command: "create_service", id: randomUUID(), environment: at(null), name, image });
  }
  yield* write("volume pg-data", {
    command: "create_volume", id: randomUUID(), environment: at(null), name: "pg-data",
    storage: { kind: "provisioned", maximumBytes: 5_000_000_000 }, mounts: [{ service: "postgres", path: "/var/lib/postgresql/data" }],
  });
  // nginx listens on 80, not PORT's 8080.
  yield* write("generated domain web", { command: "add_domain", environment: at(null), service: "web", hostname: null, port: 80 });
  yield* write("domain web", { command: "add_domain", environment: at(null), service: "web", hostname: "acme.com", port: null });
  yield* edit("production env", "production", [
    { op: "set", path: "api.env.LOG_LEVEL", value: "warn" },
    { op: "set", path: "api.env.DATABASE_URL", value: "postgres://postgres@postgres:5432/shop" },
    { op: "set", path: "postgres.env.POSTGRES_PASSWORD", value: "postgres" },
    { op: "set", path: "redis.healthcheck", value: { command: "redis-cli -h 127.0.0.1 ping | grep -q PONG", timeoutSeconds: 30 } },
    // whoami answers every path on 80, so the path passes on a Server while seed-uptime's 404 shows the Monitoring tab's field.
    { op: "set", path: "worker.env.PORT", value: "80" },
    { op: "set", path: "worker.healthcheck", value: { path: "/health", timeoutSeconds: 30 } },
    ...(yield* secretKey(organizationId, ada.id)) ?? [],
  ]);
  if (process.env["VERIFY_REAL_SERVERS"] === "1") skipped.push("api.env.SECRET_KEY: sealed once the Servers enroll (seed-sealed.ts)");
  yield* write("config sentry", {
    command: "create_config", id: randomUUID(), environment: at("production"), name: "sentry",
    mounts: [{ service: "web", dir: "/etc/sentry" }, { service: "worker", dir: "/etc/sentry" }],
  });
  for (const [file, content] of [
    ["config.yml", "redis:\n  host: ${{ redis.PLOYZ_PRIVATE_DOMAIN }}\n  port: ${{ redis.PORT }}\n"],
    ["sentry.conf.py", "SENTRY_OPTIONS = {\n    \"system.url-prefix\": \"https://sentry.acme.com\",\n}\n"],
  ] as const) {
    yield* write(`config sentry ${file}`, { command: "put_config_file", environment: at("production"), config: "sentry", file, content });
  }

  if (process.env["VERIFY_REAL_SERVERS"] === "1") {
    skipped.push("deploy production: no Server has enrolled yet");
  } else {
    const deployment = randomUUID();
    yield* write("deploy production", {
      command: "admit", admit: "deploy", id: deployment, environment: at("production"),
      services: [], version: null, accept_volume_loss: [], message: "Ship everything",
    });
    // Nothing runs the Deploy, so its nodes land in Applied State here as the Store's advance writes them for a finished
    // one: each target node frozen at admission, as its Saved Service, Volume or Config. Without it every Service reads
    // as undeployed. No Store command records an outcome without a Server answering, hence the statement.
    yield* drizzle.execute(sql`
      insert into config_applied (environment_id, node_id, organization_id, deployment_id, node_type, node)
      select d.environment_id, target->>'id', d.organization_id, d.id, target->>'type', saved.node::text
      from config_deployment d
      join config_saved s on s.environment_id = d.environment_id and s.revision = d.saved_revision
      cross join jsonb_array_elements(d.nodes::jsonb) target
      cross join lateral (
        select node from jsonb_array_elements(s.intent::jsonb->(target->>'type' || 's')) node
        where node->>(case target->>'type' when 'service' then 'id' else 'resourceId' end) = target->>'id'
      ) saved
      where d.id = ${deployment}`);
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

  // blog: pull requests, each with its own PR Environment and changes.
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
    repository_id: 4242, number, title: `Change ${number}`, author: "ada", author_id: ADA_GITHUB_ID, bot: false,
    head_repository_id: 4242, head_branch: `change-${number}`,
    head: String(number).repeat(14).slice(0, 40), target_branch: "main", commits: 1, open: true,
    merge_commit: null, target_head: null, updated: `2026-10-01T10:00:${String(second).padStart(2, "0")}Z`, ...facts,
  });
  const observe = (label: string, facts: PullRequest) => storeSystem(organizationId, { event: "pull_request", ...facts }).pipe(
    Effect.as("ok"), Effect.catchTag("StoreRefused", (refused) => Effect.succeed(`refused: ${refused.code} ${refused.message}`)),
    Effect.tap((outcome) => Effect.sync(() => { writes[label] = outcome; })),
  );
  // The Store opens a PR Environment only when a Server could run it.
  if (process.env["VERIFY_REAL_SERVERS"] === "1") {
    skipped.push("blog pull requests: no Server has enrolled yet, so no PR Environment opens");
  } else {
    const token = (environment: string) => sealed(organizationId, ada.id, blog(environment), "web", "TOKEN", "preview-token").pipe(
      Effect.flatMap((value) => value === null ? Effect.die("the fake Server's Cluster has no Sealing Key") : Effect.succeed(value)),
    );
    const numbers = [140, 141, 142, 143, 144] as const;
    for (const number of numbers) {
      yield* observe(`blog #${number} opens`, pullRequest(number, 0));
      const changes: Change[] = [{ op: "set", path: `web.env.CHANGE_${number}`, value: "on" }];
      if (number === 140) changes.push({ op: "set", path: "web.env.TOKEN", value: yield* token("pr-140") });
      yield* write(`blog pr-${number} edits`, { command: "edit", environment: blog(`pr-${number}`), expect: null, changes });
    }
    yield* observe("blog #142 merges", pullRequest(142, 1, { open: false, merge_commit: "4".repeat(40) }));
    yield* observe("blog #143 closes", pullRequest(143, 1, { open: false }));
    yield* observe("blog #144 retargets", pullRequest(144, 1, { target_branch: "dev" }));
    yield* observe("blog #144 merges into dev", pullRequest(144, 2, { open: false, target_branch: "dev", merge_commit: "5".repeat(40) }));

    // #146: production's values, then the pull request, then production moves on under it.
    const production = (label: string, changes: Change[]) => Effect.gen(function* () {
      yield* write(`blog production ${label}`, { command: "edit", environment: blog("production"), expect: null, changes });
      yield* write(`blog production ${label} publish`, { command: "publish", environment: blog("production"), version: null });
    });
    yield* production("values", [
      { op: "set", path: "web.env.API_URL", value: "http://a" },
      { op: "set", path: "web.env.CACHE_TTL", value: "30" },
      { op: "set", path: "web.env.LOG_LEVEL", value: "info" },
    ]);
    yield* observe("blog #146 opens", pullRequest(146, 0));
    yield* production("CACHE_TTL", [{ op: "set", path: "web.env.CACHE_TTL", value: "45" }]);
    yield* write("blog pr-146 edits", { command: "edit", environment: blog("pr-146"), expect: null, changes: [
      { op: "set", path: "web.env.API_URL", value: "http://b" },
      { op: "set", path: "web.env.CACHE_TTL", value: "60" },
      { op: "set", path: "web.env.LOG_LEVEL", value: "debug" },
      { op: "set", path: "web.env.TOKEN", value: yield* token("pr-146") },
    ] });
    yield* write("blog pr-146 excludes LOG_LEVEL", { command: "never_sync", environment: blog("pr-146"), rows: ["web.env.LOG_LEVEL"] });

    // Wait for CI: production holds a push to main whose checks haven't reported, pr-141 one whose checks failed.
    const system = (label: string, event: SystemEvent) => storeSystem(organizationId, event).pipe(
      Effect.as("ok"), Effect.catchTag("StoreRefused", (refused) => Effect.succeed(`refused: ${refused.code} ${refused.message}`)),
      Effect.tap((outcome) => Effect.sync(() => { writes[label] = outcome; })),
    );
    // Compared from the head the Store holds, which the conflict hands back when it isn't none.
    const push = (label: string, branch: string, head: string) => {
      const pushed = (base: string | null): SystemEvent => ({ event: "branch_head", repository_id: 4242, branch, base, head, changed: null });
      return storeSystem(organizationId, pushed(null)).pipe(
        Effect.catchTag("StoreRefused", (refused) => {
          const stored = (refused.details as { head?: unknown } | null)?.head;
          return refused.code === "conflict" && typeof stored === "string" ? storeSystem(organizationId, pushed(stored)) : Effect.fail(refused);
        }),
        Effect.as("ok"), Effect.catchTag("StoreRefused", (refused) => Effect.succeed(`refused: ${refused.code} ${refused.message}`)),
        Effect.tap((outcome) => Effect.sync(() => { writes[label] = outcome; })),
      );
    };
    for (const environment of ["production", "pr-141"]) {
      yield* write(`blog ${environment} waits for CI`, {
        command: "edit", environment: blog(environment), expect: null, changes: [{ op: "set", path: "web.waitForCi", value: true }],
      });
    }
    yield* push("blog main push waits for CI", "main", "a1b2c3d".repeat(6).slice(0, 40));
    const failing = "f00dfeed".repeat(5);
    yield* push("blog change-141 push waits for CI", "change-141", failing);
    yield* system("blog change-141 CI fails", {
      event: "check_suite", repository_id: 4242, suite: 9001, head: failing, status: "completed", conclusion: "failure",
      updated: "2026-10-01T11:00:00Z",
    });
  }

  return {
    organizationSlug, cookie, cliToken, graceCookie: grace.cookie, graceToken: grace.cliToken,
    linusCookie: linus.cookie, linusToken: linus.cliToken, bob, writes, skipped,
  };
});

// Without real Servers no Inngest answers, and the SDK retries each refused send for about 1.8 s. Refuse it at once.
const refuseSends = (inngest: PloyzInngest): PloyzInngest =>
  Object.assign(Object.create(inngest), { send: () => Promise.reject(new Error("no Inngest runs without real Servers")) });

export async function run() {
  const program = process.env["VERIFY_REAL_SERVERS"] === "1" ? seed : seed.pipe(Effect.updateService(InngestClient, refuseSends));
  const result = await AppRuntime.runPromise(program);
  console.log(`VERIFY_SEED ${JSON.stringify(result)}`);
  const refused = Object.entries(result.writes).filter(([, outcome]) => outcome !== "ok");
  if (refused.length) throw new Error(`seed writes refused: ${JSON.stringify(Object.fromEntries(refused))}`);
}
