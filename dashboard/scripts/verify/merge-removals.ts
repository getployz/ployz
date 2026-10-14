// The Config Store side of core/scripts/test-verify-merge-removals.py: the fixture a fake GitHub App would leave and
// the pull request events Cloud would observe, told through the app's own entry points. MERGE_REMOVALS_PHASE picks
// setup, open or merge; MERGE_REMOVALS_ORGANIZATION is the verify organization's slug. Prints one line,
// `MERGE_REMOVALS <json>`, of ids only.
import { randomUUID } from "node:crypto";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import type { ConfigCommand, PullRequest } from "@ployz/sdk";
import { admittedEvents, callStore, storeSystem } from "#/modules/config-store/config-store.server";
import { cloudStore, storeTry } from "#/modules/config-store/store-sdk.server";
import { user } from "#/modules/identity/tables";
import { sendInngestEvent } from "#/modules/inngest/client";
import { createConfigDeploymentAdmittedEvent } from "#/modules/inngest/events";
import { organization } from "#/modules/organization/tables";
import { Database } from "#/server/database.server";
import { AppRuntime } from "#/server/runtime.server";

const PROJECT = "removals";
const REPOSITORY_ID = 5501;
const HEAD = "a1".repeat(20);
const MERGE = "b2".repeat(20);
const at = (environment: string | null) => ({ project: PROJECT, environment });
const pullRequest = (facts: Partial<PullRequest>): PullRequest => ({
  repository_id: REPOSITORY_ID, number: 501, title: "Retire web and db", author: "ada", bot: false,
  head_branch: "retire", head: HEAD, target_branch: "main", commits: 1, open: true,
  merge_commit: null, target_head: null, updated: "2026-10-12T10:00:00Z", ...facts,
});

const env = (name: string) => {
  const value = process.env[name];
  if (!value) throw new Error(`merge-removals.ts needs ${name}`);
  return value;
};

const drive = Effect.gen(function* () {
  const phase = env("MERGE_REMOVALS_PHASE");
  const { drizzle } = yield* Database;
  const [org] = yield* drizzle.select({ id: organization.id }).from(organization).where(eq(organization.slug, env("MERGE_REMOVALS_ORGANIZATION")));
  const [ada] = yield* drizzle.select({ id: user.id }).from(user).where(eq(user.email, "ada@example.com"));
  if (!org || !ada) return yield* Effect.die("The verify seed's organization is missing.");
  const write = (command: ConfigCommand) => callStore(org.id, ada.id, { operation: "write", command }).pipe(
    Effect.flatMap((result) => result.ok
      ? Effect.succeed(result.value)
      : Effect.die(`${command.command} refused: ${result.refusal.code} ${result.refusal.message}`)),
  );

  const setup = Effect.gen(function* () {
    yield* write({ command: "create_project", id: randomUUID(), name: PROJECT, default_environment: randomUUID() });
    for (const [name, image] of [["web", "nginx:1.27-alpine"], ["db", "postgres:16-alpine"], ["worker", "traefik/whoami:v1.10.3"]] as const) {
      yield* write({ command: "create_service", id: randomUUID(), environment: at(null), name, image });
    }
    yield* write({
      command: "create_volume", id: randomUUID(), environment: at(null), name: "pg-data",
      storage: { kind: "provisioned", maximumBytes: 2_000_000_000 }, mounts: [{ service: "db", path: "/var/lib/postgresql/data" }],
    });
    const store = yield* cloudStore;
    const repository = {
      repository: "acme/removals", repository_id: REPOSITORY_ID, access: { type: "github-installation" as const, installationId: 7 },
      default_branch: "main", branches: [],
    };
    yield* storeTry(() => store.write(org.id, {
      command: "create_git_service", id: randomUUID(), environment: at(null), name: "anchor", repository: "acme/removals", branch: null,
    }, { repositories: [repository], domains: { cluster_domain: null, certificates: null, ingress_addresses: [], lookups: [] } }));
    yield* write({
      command: "edit", environment: at("production"), expect: null, changes: [
        { op: "set", path: "db.env.POSTGRES_PASSWORD", value: "verify" },
        { op: "set", path: "db.env.PGDATA", value: "/var/lib/postgresql/data/pgdata" },
        { op: "set", path: "anchor.autoDeploy", value: "false" },
      ],
    });
    yield* write({ command: "publish", environment: at("production"), version: null });
    yield* write({
      command: "set_pr_plan", project: PROJECT, repository: "acme/removals", enabled: true, start_from: "production",
      copy: ["web", "db", "volumes.pg-data"], setup: null, remove_on_close: false, include_bots: null,
    });
    return { project: PROJECT };
  });
  const open = Effect.gen(function* () {
    const opened = yield* storeSystem(org.id, { event: "pull_request", ...pullRequest({}) });
    const previews = admittedEvents(org.id, opened, "github").map((event) => event.deploymentId);
    for (const deployment of previews) yield* write({ command: "cancel", deployment });
    return { cancelled: previews };
  });
  const merge = Effect.gen(function* () {
    const merged = yield* storeSystem(org.id, {
      event: "pull_request", ...pullRequest({ open: false, merge_commit: MERGE, target_head: MERGE, updated: "2026-10-12T10:05:00Z" }),
    });
    const pushed = yield* storeSystem(org.id, {
      event: "branch_head", repository_id: REPOSITORY_ID, branch: "main", base: null, head: MERGE, changed: [],
    });
    const admitted = [...admittedEvents(org.id, merged, "github"), ...admittedEvents(org.id, pushed, "github")]
      .filter((event, index, all) => all.findIndex((other) => other.deploymentId === event.deploymentId) === index);
    if (admitted.length) yield* sendInngestEvent(admitted.map(createConfigDeploymentAdmittedEvent));
    return { admitted: admitted.map((event) => event.deploymentId) };
  });
  if (phase === "setup") return yield* setup;
  if (phase === "open") return yield* open;
  if (phase === "merge") return yield* merge;
  return yield* Effect.die(`Unknown MERGE_REMOVALS_PHASE ${phase}`);
});

export async function run() {
  const result = await AppRuntime.runPromise(drive);
  console.log(`MERGE_REMOVALS ${JSON.stringify(result)}`);
}
