import { it } from "@effect/vitest";
import { InngestTestEngine } from "@inngest/test";
import type { ConfigCommand, JsonValue, ConfigTrusted } from "@ployz/sdk";
import { Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { expect, vi } from "vitest";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { requestChecks } from "#/modules/config-store/config-store.server";
import { createStorePullRequest } from "#/modules/config-store/store-github.inngest";
import { githubPullRequestReceivedEvent } from "#/modules/inngest/events";
import { makeInngestEffectRunner } from "#/server/run.server";
import { fakeGithubApiBy } from "#/test/fake-github";
import { enrollStoreServer, seedStoreOrganization, seedStoreGitService, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000c001";
const PROJECT = "00000000-0000-4000-8000-00000000c002";
const ENVIRONMENT = "00000000-0000-4000-8000-00000000c003";
const SERVICE = "00000000-0000-4000-8000-00000000c004";
const HEAD = "1".repeat(40);
const here = { project: null, environment: null };

type PullState = { state: "open" | "closed"; updated_at: string };

/** GitHub with `acme/web` (42) through installation 7: pull request 5 as `pull` says; check runs land in `posted`. */
function github(pull: PullState, posted: unknown[]) {
  return fakeGithubApiBy(({ url, body }): JsonValue => {
    if (url.endsWith("/check-runs")) posted.push(body);
    return url.endsWith("/pulls/5")
      ? {
        number: 5, title: "Add search", user: { login: "ada", type: "User" }, head: { ref: "search", sha: HEAD },
        base: { ref: "main" }, merged: false, merge_commit_sha: null, commits: 1, ...pull,
      }
      : url.includes("/check-runs?")
        ? { check_runs: [] }
        : url.endsWith("/check-runs") ? { id: 99 } : { id: 42, full_name: "acme/web", private: true };
  }).service;
}

const delivery = (id: string) => ({
  name: githubPullRequestReceivedEvent,
  data: {
    kind: "pull_request", action: "opened", installationId: 7, repositoryId: 42, number: 5, headRepositoryId: 42,
    headSha: HEAD, deliveryId: id, pullRequestKey: "7:42:5",
  },
});

it.live(
  "pull requests reach the Store: a PR Environment deploys on open, its check is published, a late open never reopens it",
  () =>
    Effect.gen(function* () {
      const pull: PullState = { state: "open", updated_at: "2026-09-29T10:00:00Z" };
      const posted: unknown[] = [];
      const checks = new Inngest({ id: "store-pull-request-checks" });
      const requested: unknown[] = [];
      vi.spyOn(checks, "send").mockImplementation(async (event) => {
        requested.push(event);
        return { ids: [] };
      });
      const services = yield* Layer.build(yield* storeTestCloud({ github: github(pull, posted), inngest: checks }));
      yield* Effect.all([seedStoreOrganization(ORGANIZATION), enrollStoreServer(ORGANIZATION)]).pipe(Effect.provide(services));
      const store = yield* cloudStore.pipe(Effect.provide(services));
      const write = (command: ConfigCommand, trusted?: ConfigTrusted) =>
        Effect.promise(() => store.write(ORGANIZATION, command, trusted));
      yield* seedStoreGitService(ORGANIZATION, { project: PROJECT, environment: ENVIRONMENT, service: SERVICE }).pipe(Effect.provide(services));
      yield* write({ command: "publish", environment: here, version: null, accept_volume_loss: [] });
      yield* write({
        command: "set_pr_plan", project: null, repository: "acme/web", enabled: true, start_from: "production",
        copy: null, setup: null, remove_on_close: null, include_bots: null,
      });

      const runner: Parameters<typeof createStorePullRequest>[1] = makeInngestEffectRunner((program) => Effect.runPromise(program.pipe(
        Effect.provide(services))));
      const inngest = new Inngest({ id: "store-pull-request-test" });
      const dispatched: string[] = [];
      const run = (id: string) => Effect.promise(async () =>
        (await new InngestTestEngine({
          function: createStorePullRequest(inngest, runner),
          events: [delivery(id)],
          steps: [
            { id: "dispatch", handler: () => dispatched.push(id) },
          ],
        }).execute()).result as { admitted: string[]; removals: string[]; check: string });
      const environments = () => Effect.promise(async () => {
        const view = await store.read(ORGANIZATION, { query: "environments", project: null });
        return view.environments.map((listing) => listing.name);
      });

      // Opened: the PR Environment deploys, and the check says nothing waits to be saved.
      const opened = yield* run("open");
      expect(opened.admitted).toHaveLength(1);
      expect(opened.check).toBe("posted");
      expect(dispatched).toEqual(["open"]);
      expect(yield* environments()).toEqual(["pr-5", "production"]);
      expect(posted).toMatchObject([{ head_sha: HEAD, conclusion: "success", output: { title: "No changes for production" } }]);

      // Any later write may move the check: Cloud asks for it again.
      yield* requestChecks(ORGANIZATION).pipe(Effect.provide(services));
      expect(requested).toEqual([[{ name: "config/pr-check.requested", data: {
        organizationId: ORGANIZATION, repositoryId: 42, number: 5, pullRequestKey: "42:5",
      } }]]);

      // Closed: never deployed, so it goes at once; no check for a closed pull request.
      Object.assign(pull, { state: "closed", updated_at: "2026-09-29T11:00:00Z" });
      expect((yield* run("close")).check).toBe("skipped");
      expect(yield* environments()).toEqual(["production"]);

      // GitHub's older open, read late, never brings it back.
      Object.assign(pull, { state: "open", updated_at: "2026-09-29T10:30:00Z" });
      expect((yield* run("late")).admitted).toEqual([]);
      expect(yield* environments()).toEqual(["production"]);

      // Reopened, it runs (and fails: its source can't be read); closed again, Cloud takes it off the Servers first.
      Object.assign(pull, { state: "open", updated_at: "2026-09-29T12:00:00Z" });
      const reopened = yield* run("reopen");
      yield* Effect.promise(() => store.runDeployment(ORGANIZATION, reopened.admitted[0] ?? "", "cloud-test", [], { failure: "No source." }));
      Object.assign(pull, { state: "closed", updated_at: "2026-09-29T13:00:00Z" });
      const closed = yield* run("close-again");
      expect(closed.removals).toHaveLength(1);
      // Admitted like any removal, so Cloud hands it to its worker.
      expect(requested).toContainEqual(expect.objectContaining({ data: expect.objectContaining({ deploymentId: closed.removals[0] }) }));
      expect(yield* Effect.promise(() => store.read(ORGANIZATION, { query: "deployment", id: closed.removals[0] ?? "" })))
        .toMatchObject({ remove: true, status: "queued" });
    }),
  60_000,
);
