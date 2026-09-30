import { assert, it } from "@effect/vitest";
import { expect, vi } from "vitest";
import type { ConfigCommand, ConfigQuery } from "@ployz/sdk";
import { mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { eq, sql } from "drizzle-orm";
import * as tar from "tar";
import { Cause, Effect, Exit, Layer } from "effect";
import { Inngest } from "inngest";
import { organizationBillingState } from "#/modules/billing/tables";
import type { PolarService } from "#/modules/billing/polar-provider.server";
import { startFakeHostedDns } from "#/modules/cluster-domain/hosted-dns.test-fixture";
import { callStoreAsMember } from "#/modules/config-store/config-store.server";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { enrollStoreServer, storeTestCloud } from "#/test/store-cloud";
import { handleConfigRequest } from "#/routes/api/config/-config.handler";
import { runStoreDeployment } from "#/modules/config-store/store-deployment.server";
import { uploadChunk } from "#/modules/config-store/tables";
import { receiveUpload, releaseUpload } from "#/modules/config-store/upload.server";
import { resolveCaller } from "#/modules/identity/caller.server";
import { Auth, AuthLive } from "#/server/auth.server";
import { githubInstallation, githubRepositoryCache } from "#/modules/github/tables";
import { member, user } from "#/modules/identity/tables";
import { organizationMachine } from "#/modules/machines/tables";
import { Database } from "#/server/database.server";
import { fakeGithubApi } from "#/test/fake-github";
import { encodePublicError, NotFound, statusForPublicError } from "#/server/public-error";

const origin = "http://localhost:3000";

/**
 * GitHub as these tests see it: `acme/web` is private (read through installation 7) with a `dev` branch, `acme/docs`
 * is public, `acme/secret` is someone else's private repository, and `acme/down` finds GitHub unreachable.
 */
const github = fakeGithubApi({
  "https://api.github.com/repos/acme/web/git/ref/heads%2Fdev": {
    ref: "refs/heads/dev", object: { type: "commit", sha: "a".repeat(40) },
  },
  "https://api.github.com/repos/acme/docs": { id: 12, full_name: "acme/docs", private: false, default_branch: "main" },
  "https://api.github.com/repos/acme/secret": { id: 13, full_name: "acme/secret", private: true, default_branch: "main" },
  "https://api.github.com/repos/acme/down": "down",
});

/** Cloud with the Config Store in its database. */
const cloudLayer = Effect.fn(function* (
  overrides: { readonly polar?: PolarService; readonly hostedDnsUrl?: string } = {},
  inngest = new Inngest({ id: "config-store-test" }),
) {
  const services = yield* storeTestCloud({
    github: github.service,
    inngest,
    // No Hosted DNS answers unless a test starts one.
    env: { PLOYZ_HOSTED_DNS_URL: overrides.hostedDnsUrl ?? "http://127.0.0.1:9/" },
    polar: overrides.polar ?? { mode: "self_hosted" },
  });
  return Layer.merge(AuthLive.pipe(Layer.provide(services)), services);
});

/** The fields these tests read from Store results and refusals. */
type Reply = {
  readonly written?: string;
  readonly view?: string;
  readonly staged?: ReadonlyArray<string>;
  readonly settings?: ReadonlyArray<{ readonly path: string; readonly value: number; readonly default: number; readonly apply: string }>;
  readonly values?: Readonly<Record<string, string | number>>;
  readonly error?: { readonly code: string; readonly details: { readonly revision?: number; readonly next?: string } | null };
  readonly domain?: { readonly kind: string; readonly prefix?: string; readonly hostname: string | null };
  readonly domains?: ReadonlyArray<{ readonly hostname: string | null; readonly status: string; readonly action: { readonly type: string } | null }>;
};

/** One CLI request; `json` is the Store's result, or its refusal under `error`. */
const request = Effect.fn(function* (path: string, cookie: string | undefined, body: Body | undefined, method = "POST") {
  const headers = new Headers({ "content-type": "application/json" });
  if (cookie !== undefined) headers.set("cookie", cookie);
  const init: RequestInit = { method, headers };
  if (method === "POST") init.body = JSON.stringify(body);
  const exit = yield* Effect.exit(handleConfigRequest(new Request(`${origin}/api/config/${path}`, init)));
  if (Exit.isFailure(exit)) {
    const empty: Reply = {};
    return { status: statusForPublicError(encodePublicError(Cause.squash(exit.cause))), json: empty };
  }
  const text = yield* Effect.promise(() => exit.value.text());
  // SAFETY: test-only view of the Store's JSON; assertions check every field read.
  return { status: exit.value.status, json: JSON.parse(text) as Reply };
});

const signUp = Effect.fn(function* (name: string) {
  const auth = yield* Auth;
  const response = yield* auth.handler(new Request(`${origin}/api/auth/sign-up/email`, {
    method: "POST",
    headers: { "content-type": "application/json", "user-agent": "ployz-cli/0.0.0" },
    body: JSON.stringify({ email: `${name}@example.test`, name, password: "correct-horse-battery-staple" }),
  }));
  assert.strictEqual(response.status, 200);
  return response.headers.get("set-cookie")?.split(";", 1)[0] ?? assert.fail("no session cookie");
});

/** What the CLI sends, plus one command the Store never takes over HTTPS. */
type Body = ConfigQuery | ConfigCommand | { readonly command: "claim"; readonly deployment: string }
  | { readonly command: "create_git_service"; readonly installation_id: number };

const PROJECT = "00000000-0000-4000-8000-000000000001";
const ENVIRONMENT = "00000000-0000-4000-8000-000000000002";
const SERVICE = "00000000-0000-4000-8000-000000000003";
const here = { project: null, environment: null };
const shop: ConfigCommand = { command: "create_project", id: PROJECT, name: "shop", default_environment: ENVIRONMENT };
const web: ConfigCommand = {
  command: "create_service", id: SERVICE, environment: here, name: "web", image: "nginx:1",
};
const write = (command: ConfigCommand) => ({ operation: "write" as const, command });
const get = (path: string | null): ConfigQuery => ({ query: "environment", environment: here, path, all: false });

it.live(
  "the CLI reads and writes its own Organization's Config Store over HTTPS",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        const bob = yield* signUp("bob");

        assert.strictEqual((yield* request("write", undefined, shop)).status, 401);

        const created = yield* request("write", alice, shop);
        assert.strictEqual(created.status, 200);
        assert.strictEqual(created.json.written, "project");
        // A retried create replays; the same ID with another body conflicts.
        assert.deepStrictEqual((yield* request("write", alice, shop)).json, created.json);
        const reused = yield* request("write", alice, { ...shop, name: "blog" });
        assert.strictEqual(reused.status, 409);
        assert.strictEqual(reused.json.error?.code, "conflict");

        assert.strictEqual((yield* request("write", alice, web)).status, 200);
        const edited = yield* request("write", alice, {
          command: "edit", environment: here, expect: 2, changes: [{ op: "set", path: "web.replicas", value: "3" }],
        });
        assert.deepStrictEqual(edited.json.staged, ["web.replicas"]);
        const replicas = yield* request("read", alice, get("web.replicas"));
        assert.deepInclude(replicas.json, { view: "environment" });
        assert.deepStrictEqual(replicas.json.settings, [
          { path: "web.replicas", value: 3, default: 1, apply: "staged" },
        ]);

        const stale = yield* request("write", alice, {
          command: "edit", environment: here, expect: 2, changes: [{ op: "unset", path: "web.replicas" }],
        });
        assert.strictEqual(stale.status, 409);
        assert.strictEqual(stale.json.error?.code, "conflict");
        assert.deepStrictEqual(stale.json.error?.details, { revision: 3 });

        // Another Organization sees none of it.
        const foreign = yield* request("read", bob, get(null));
        assert.strictEqual(foreign.status, 404);
        assert.strictEqual(foreign.json.error?.code, "not_found");

        const invalid = yield* request("write", alice, { command: "claim", deployment: "d1" });
        assert.strictEqual(invalid.status, 422);
        assert.strictEqual(invalid.json.error?.code, "invalid_argument");
        assert.strictEqual((yield* request("claim", alice, get(null))).status, 404);
        assert.strictEqual((yield* request("read", alice, undefined, "GET")).status, 404);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "the dashboard reaches the Store as a member of the Organization it names, refusals intact",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const alice = yield* resolveCaller(new Headers({ cookie: yield* signUp("alice") }));
        const bob = yield* resolveCaller(new Headers({ cookie: yield* signUp("bob") }));
        const slug = alice.organization.slug;

        assert.isTrue((yield* callStoreAsMember(alice, slug, write(shop))).ok);
        assert.isTrue((yield* callStoreAsMember(alice, slug, write(web))).ok);
        const stale = yield* callStoreAsMember(alice, slug, write({
          command: "edit", environment: here, expect: 1, changes: [{ op: "set", path: "web.replicas", value: 3 }],
        }));
        assert.isFalse(stale.ok);
        if (!stale.ok) {
          assert.strictEqual(stale.refusal.code, "conflict");
          assert.deepStrictEqual(stale.refusal.details, { revision: 2 });
        }
        const view = yield* callStoreAsMember(alice, slug, { operation: "read", query: get("web.replicas") });
        assert.deepInclude(view, { ok: true });

        // Naming an Organization you're not a member of is refused before the Store is asked.
        const foreign = yield* Effect.exit(callStoreAsMember(bob, slug, { operation: "read", query: get(null) }));
        assert.isTrue(Exit.isFailure(foreign) && Cause.squash(foreign.cause) instanceof NotFound);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "Store writes reach Cloud's change log once committed, and a refused write logs nothing",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        const { drizzle } = yield* Database;
        let seen = 0;
        /** The Store changes logged since the last call, as `table: keys`; each statement logs once. */
        const logged = Effect.fn(function* () {
          const rows = yield* drizzle.execute<{ seq: string; entry: string }>(sql`
            select seq::text, source_table || ': ' || array_to_string(changed_ids, ',') as entry
            from organization_change
            where source_table like 'config_%' and seq > ${seen}
              and organization_id = (select organization_id::uuid from config_project limit 1)
            order by seq
          `, "objects");
          seen = Math.max(seen, ...rows.map((row) => Number(row.seq)));
          return [...new Set(rows.map((row) => row.entry))];
        });

        assert.strictEqual((yield* request("write", alice, shop)).status, 200);
        assert.deepStrictEqual(yield* logged(), [`config_project: ${PROJECT}`, `config_environment: ${ENVIRONMENT}`]);
        assert.strictEqual((yield* request("write", alice, web)).status, 200);
        assert.sameMembers(yield* logged(), [`config_environment: ${ENVIRONMENT}`, `config_node_introduction: ${ENVIRONMENT}`]);

        // The edit locks and rewrites the Environment before refusing the value, and rolls it all back.
        const refused = yield* request("write", alice, {
          command: "edit", environment: here, expect: null, changes: [{ op: "set", path: "web.replicas", value: "many" }],
        });
        assert.strictEqual(refused.json.error?.code, "invalid_argument");
        assert.deepStrictEqual(yield* logged(), []);

        assert.strictEqual((yield* request("write", alice, { command: "publish", environment: here, version: null, accept_volume_loss: [] })).status, 200);
        // Every write locks its Environment's row, which logs the Environment too.
        assert.sameMembers(yield* logged(), [`config_environment: ${ENVIRONMENT}`, `config_saved: ${ENVIRONMENT}`]);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "an admission over HTTPS hands its Deployment to Cloud's worker, once",
  () =>
    Effect.gen(function* () {
      const inngest = new Inngest({ id: "config-store-test" });
      const sent: unknown[] = [];
      let down = false;
      vi.spyOn(inngest, "send").mockImplementation(async (event) => {
        if (down) throw new Error("Inngest is down");
        sent.push(event);
        return { ids: [] };
      });
      const layer = yield* cloudLayer({}, inngest);
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        yield* enrollStoreServer((yield* resolveCaller(new Headers({ cookie: alice }))).organization.id);
        yield* request("write", alice, shop);
        yield* request("write", alice, web);
        const admit: ConfigCommand = {
          command: "admit", admit: "deploy", id: "00000000-0000-4000-8000-000000000101", environment: here, services: [], version: null, accept_volume_loss: [],
        };
        assert.strictEqual((yield* request("write", alice, admit)).status, 200);
        // A retried request replays the admission and sends the same event, which Inngest drops.
        assert.strictEqual((yield* request("write", alice, admit)).status, 200);
        const event = {
          id: "config-deployment-admitted-00000000-0000-4000-8000-000000000101",
          name: "config/deployment.admitted",
          data: { organizationId: expect.any(String), environmentId: ENVIRONMENT, deploymentId: admit.id },
        };
        expect(sent).toEqual([event, event]);

        // Inngest down: the admission stands and says so; the sweep, or Deploy now, hands it over later.
        down = true;
        const stranded = yield* request("write", alice, { ...admit, id: "00000000-0000-4000-8000-000000000102" });
        assert.strictEqual(stranded.status, 200);
        assert.deepInclude(stranded.json, { written: "deployment" });

        // Deploy now hands the stranded admission to the worker again, unkeyed; a refused start sends nothing.
        down = false;
        const start: ConfigCommand = { command: "start", deployment: "00000000-0000-4000-8000-000000000102" };
        assert.strictEqual((yield* request("write", alice, start)).status, 200);
        expect(sent.at(-1)).toEqual({
          name: "config/deployment.admitted",
          data: { organizationId: expect.any(String), environmentId: ENVIRONMENT, deploymentId: start.deployment },
        });
        const superseded = yield* request("write", alice, { ...start, deployment: admit.id });
        assert.strictEqual(superseded.status, 409);
        assert.strictEqual(sent.length, 3);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "a Git-backed Service connects only repositories and branches Cloud checked for the Organization",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        assert.strictEqual((yield* request("write", alice, shop)).status, 200);
        // Alice installed the GitHub App on acme with acme/web; her Organization reads it through that installation.
        const database = yield* Database;
        const [row] = yield* database.drizzle.select({ id: user.id, organization: member.organizationId }).from(user)
          .innerJoin(member, eq(member.userId, user.id)).where(eq(user.email, "alice@example.test"));
        const userId = row?.id ?? assert.fail("no user");
        yield* database.drizzle.insert(githubInstallation).values({ userId, installationId: 7, accountLogin: "acme", accountType: "Organization" });
        yield* database.drizzle.insert(githubRepositoryCache).values({
          userId, installationId: 7, repositoryId: 11, name: "web", fullName: "acme/web", defaultBranch: "main",
          private: true, htmlUrl: "https://github.com/acme/web", repoUpdatedAt: new Date(),
        });
        let services = 10;
        const git = (name: string, repository: string, branch?: string): ConfigCommand => ({
          command: "create_git_service", id: `00000000-0000-4000-8000-0000000000${(services += 1)}`,
          environment: here, name, repository, branch: branch ?? null,
        });

        const created = yield* request("write", alice, git("web", "Acme/Web", "dev"));
        assert.strictEqual(created.status, 200);
        assert.include(created.json.staged ?? [], "web.branch");
        assert.deepInclude(github.calls, { url: "https://api.github.com/repos/acme/web/git/ref/heads%2Fdev", installationId: 7 });
        const web = yield* request("read", alice, get("web"));
        assert.deepInclude(web.json.values, { repository: "acme/web", branch: "dev" });

        const secret = yield* request("write", alice, git("docs", "acme/secret"));
        assert.strictEqual(secret.status, 404);
        assert.strictEqual(secret.json.error?.details?.next, "ployz github connect");
        const gone = yield* request("write", alice, {
          command: "edit", environment: here, expect: null, changes: [{ op: "set", path: "web.branch", value: "gone" }],
        });
        assert.strictEqual(gone.status, 404);
        assert.strictEqual(gone.json.error?.details?.next, "ployz github ls acme/web");

        // A public repository needs no installation; acme/docs has no dev branch, so web moves to its default.
        const moved = yield* request("write", alice, {
          command: "edit", environment: here, expect: null, changes: [{ op: "set", path: "web.repository", value: "acme/docs" }],
        });
        assert.strictEqual(moved.status, 200);
        assert.deepInclude((yield* request("read", alice, get("web"))).json.values, { repository: "acme/docs", branch: "main" });

        const forged = yield* request("write", alice, { command: "create_git_service", installation_id: 7 });
        assert.strictEqual(forged.status, 422);
        const down = yield* request("write", alice, git("api", "acme/down"));
        assert.strictEqual(down.status, 503);
        assert.strictEqual(down.json.error?.code, "unavailable");
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "domains: custom ones need Pro, and a generated one reserves the Cluster Domain when it is first admitted",
  () =>
    Effect.gen(function* () {
      const hostedDns = yield* Effect.acquireRelease(Effect.promise(startFakeHostedDns), (fake) => Effect.promise(() => fake.close()));
      const inngest = new Inngest({ id: "config-store-test" });
      const sent: Array<{ readonly name: string }> = [];
      vi.spyOn(inngest, "send").mockImplementation(async (event) => {
        sent.push(...[event].flat());
        return { ids: [] };
      });
      const hosted: PolarService = {
        mode: "hosted",
        productId: "pro",
        listActiveSubscriptions: () => Effect.die("the capability reads the cached row"),
        createCheckout: () => Effect.die("not used"),
        createCustomerPortal: () => Effect.die("not used"),
      };
      const layer = yield* cloudLayer({ polar: hosted, hostedDnsUrl: hostedDns.url }, inngest);
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        yield* enrollStoreServer((yield* resolveCaller(new Headers({ cookie: alice }))).organization.id);
        yield* request("write", alice, shop);
        yield* request("write", alice, web);
        const custom: ConfigCommand = { command: "add_domain", environment: here, service: "web", hostname: "app.example.com", port: null };
        const refused = yield* request("write", alice, custom);
        assert.strictEqual(refused.status, 501);
        assert.strictEqual(refused.json.error?.code, "unsupported");
        assert.strictEqual(refused.json.error?.details?.next, "ployz billing upgrade");

        const database = yield* Database;
        const [owner] = yield* database.drizzle.select({ organization: member.organizationId }).from(member);
        yield* database.drizzle.insert(organizationBillingState).values({
          organizationId: owner?.organization ?? assert.fail("no Organization"),
          hasActiveSubscription: true,
          activeSubscriptionId: "sub",
          currentPeriodEnd: new Date(Date.now() + 86_400_000),
          syncedAt: new Date(),
        });
        assert.strictEqual((yield* request("write", alice, custom)).status, 200);

        const generated = yield* request("write", alice, { ...custom, hostname: null });
        // No Cluster Domain yet: only admission reserves one.
        assert.deepInclude(generated.json.domain, { kind: "generated", prefix: "web", hostname: null });
        assert.lengthOf(hostedDns.requests, 0);
        const listed = yield* request("read", alice, { query: "domains", environment: here, service: null });
        assert.deepStrictEqual(listed.json.domains?.map((domain) => [domain.status, domain.action?.type]), [
          ["setting_up", "deploy"],
          ["setting_up", "deploy"],
        ]);

        const admit: ConfigCommand = {
          command: "admit", admit: "deploy", id: "00000000-0000-4000-8000-000000000101", environment: here, services: [], version: null,
          accept_volume_loss: [],
        };
        assert.strictEqual((yield* request("write", alice, admit)).status, 200);
        assert.lengthOf(hostedDns.requests.filter((call) => call.method === "POST"), 1);
        const deploying = yield* request("read", alice, { query: "domains", environment: here, service: null });
        const hostname = deploying.json.domains?.[1]?.hostname ?? "";
        assert.match(hostname, /^web\.[a-z0-9-]+\.ployz\.test$/);

        // A check asks for a Cluster Domain sync before it reads the domain afresh.
        sent.length = 0;
        const checked = yield* request("read", alice, { query: "domain", environment: here, domain: "web" });
        assert.strictEqual(checked.json.domain?.hostname, hostname);
        assert.deepStrictEqual(sent.map((event) => event.name), ["cluster-domain/sync.requested"]);
      }).pipe(Effect.provide(layer));
    }).pipe(Effect.scoped),
  60_000,
);

/** A gzipped tar of a source directory, as `ployz deploy --upload` sends it: everything under `source/`. */
const sourceArchive = Effect.promise(async () => {
  const root = await mkdtemp(path.join(tmpdir(), "upload-test-"));
  await mkdir(path.join(root, "source"));
  await writeFile(path.join(root, "source", "Dockerfile"), "FROM scratch\n");
  const chunks: Buffer[] = [];
  for await (const chunk of tar.c({ gzip: true, cwd: root, portable: true }, ["source"])) chunks.push(Buffer.from(chunk));
  return Buffer.concat(chunks);
});

const upload = Effect.fn(function* (deploymentId: string, cookie: string, body: Buffer) {
  const response = yield* handleConfigRequest(new Request(`${origin}/api/config/upload/${deploymentId}`, {
    method: "POST", headers: { cookie, "content-type": "application/gzip" }, body: new Blob([new Uint8Array(body)]),
  }));
  // SAFETY: test-only view of Cloud's JSON; assertions check every field read.
  return { status: response.status, json: (yield* Effect.promise(() => response.json())) as Reply };
});

it.live(
  "an upload belongs to the one Deployment it was sent for, names its uploader, and goes once that Deployment ends",
  () =>
    Effect.gen(function* () {
      const inngest = new Inngest({ id: "config-store-test" });
      vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
      const layer = yield* cloudLayer({}, inngest);
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        yield* enrollStoreServer((yield* resolveCaller(new Headers({ cookie: alice }))).organization.id);
        const bob = yield* signUp("bob");
        yield* request("write", alice, shop);
        yield* request("write", alice, {
          command: "create_service", id: SERVICE, environment: here, name: "app", image: null,
        });
        const [first, second] = ["00000000-0000-4000-8000-000000000201", "00000000-0000-4000-8000-000000000202"];
        const archive = yield* sourceArchive;
        assert.strictEqual((yield* upload(first, alice, archive)).status, 200);
        assert.strictEqual((yield* upload(second, alice, archive)).status, 200);
        // Written once, and never shared: not again, and not by another Organization.
        const again = yield* upload(first, alice, archive);
        assert.strictEqual(again.status, 409);
        assert.strictEqual(again.json.error?.code, "conflict");
        assert.strictEqual((yield* upload(first, bob, archive)).status, 409);
        const { id: organizationId } = (yield* resolveCaller(new Headers({ cookie: alice }))).organization;
        const store = yield* cloudStore;
        const tooBig = yield* receiveUpload(store, organizationId, "00000000-0000-4000-8000-000000000203",
          new Blob([archive]).stream(), archive.length - 1);
        assert.strictEqual(tooBig?.code, "invalid_argument");

        // Cloud, not the caller, names who uploaded it.
        const admitted = yield* request("write", alice, {
          command: "admit", admit: "deploy", id: first, environment: here, services: [], version: null, accept_volume_loss: [],
          upload: { digest: "d".repeat(64), base: null, uploader: "mallory" },
        });
        assert.strictEqual(admitted.status, 200);
        const view = yield* Effect.promise(() => store.read(organizationId, { query: "deployment", id: first }));
        expect(view).toMatchObject({ upload: { digest: "d".repeat(64), uploader: "alice" } });
        assert.strictEqual((yield* upload(first, alice, archive)).status, 409);

        const { drizzle } = yield* Database;
        const held = (deploymentId: string) => drizzle.select().from(uploadChunk).where(eq(uploadChunk.deploymentId, deploymentId))
          .pipe(Effect.map((rows) => rows.length));
        // Kept while it may still run, so a replaced worker finds it.
        yield* releaseUpload(store, organizationId, first);
        assert.strictEqual(yield* held(first), 1);
        // Its Server left: its upload was read, and nothing ran.
        yield* drizzle.delete(organizationMachine);
        const ran = yield* runStoreDeployment({ organizationId, environmentId: ENVIRONMENT, deploymentId: first }, "cloud-test");
        expect(ran).toMatchObject({ ran: { id: first, status: "failed" } });
        assert.strictEqual(yield* held(first), 0);
        assert.strictEqual(yield* held(second), 1);
      }).pipe(Effect.provide(layer));
    }).pipe(Effect.scoped),
  60_000,
);
