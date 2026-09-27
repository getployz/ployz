import { testConfigEnvironment } from "#/test/config-environment";
import { createHmac } from "node:crypto";
import { assert, expect, it } from "@effect/vitest";
import { ConfigProvider, Effect, Layer } from "effect";
import { eq } from "drizzle-orm";
import { Inngest } from "inngest";
import { schema } from "#/db";
import type { JsonValue } from "#/db/schema";
import { InngestClient } from "#/modules/inngest/client";
import { AppConfig } from "#/server/config.server";
import { Database, DatabaseLive } from "#/server/database.server";
import { postgresTestDatabase } from "#/test/postgres";
import { executeProcessGithubPullRequestReceived } from "#/modules/github/inngest-ingestion/process";
import type { PullRequestEffectRunner } from "#/modules/pr-environments/pr-lifecycle.server";
import { handleGithubWebhookRequest } from "./-webhook.handler";

const webhookSecret = "github-webhook-secret";

function request(event: string, deliveryId: string, payload: JsonValue) {
  const body = JSON.stringify(payload);
  const signature = createHmac("sha256", webhookSecret).update(body).digest("hex");
  return new Request("http://localhost/api/github/webhook", {
    method: "POST",
    headers: {
      "x-hub-signature-256": `sha256=${signature}`,
      "x-github-event": event,
      "x-github-delivery": deliveryId,
    },
    body,
  });
}

it.live(
  "authenticates, decodes, dispatches, and durably rejects GitHub ingress",
  () =>
    Effect.gen(function* () {
      const testDatabase = yield* postgresTestDatabase;
      const sent: unknown[] = [];
      const inngest = new Inngest({ id: "github-webhook-contract" });
      inngest.send = async (input) => {
        sent.push(input);
        return { ids: ["event-1"] };
      };
      const config = AppConfig.layer.pipe(
        Layer.provide(
          ConfigProvider.layer(
            ConfigProvider.fromEnv({
              env: {
                ...testConfigEnvironment(),
                DATABASE_URL: testDatabase.url.href,
                GITHUB_APP_WEBHOOK_SECRET: webhookSecret,
              },
            }),
          ),
        ),
      );
      const layer = Layer.mergeAll(
        config,
        DatabaseLive.pipe(Layer.provide(config)),
        Layer.succeed(InngestClient, inngest),
      );

      yield* Effect.gen(function* () {
        const invalid = yield* handleGithubWebhookRequest(
          new Request("http://localhost/api/github/webhook", {
            method: "POST",
            headers: { "x-hub-signature-256": "sha256=invalid" },
            body: "{}",
          }),
        );
        assert.strictEqual(invalid.status, 401);
        assert.strictEqual(sent.length, 0);

        const accepted = yield* handleGithubWebhookRequest(
          request("installation", "delivery-installation", {
            action: "created",
            installation: {
              id: 99,
              account: {
                login: "acme",
                type: "Organization",
                avatar_url: "https://example.test/avatar.png",
              },
            },
            sender: { id: 123, login: "nick" },
            hook: { secret: "must-not-leave-boundary" },
          }),
        );
        assert.strictEqual(accepted.status, 200);
        assert.strictEqual(sent.length, 1);
        const serialized = JSON.stringify(sent[0]);
        assert.match(serialized, /delivery-installation/);
        assert.notMatch(serialized, /must-not-leave-boundary/);

        const workflowRun = yield* handleGithubWebhookRequest(
          request("workflow_run", "delivery-workflow-run", { action: "completed" }),
        );
        assert.strictEqual(workflowRun.status, 200);
        assert.strictEqual(sent.length, 1);

        const rejected = yield* handleGithubWebhookRequest(
          request("push", "delivery-malformed", {
            installation: { id: 17 },
            repository: { id: 42 },
          }),
        );
        assert.strictEqual(rejected.status, 422);
        const database = yield* Database;
        const rows = yield* database.drizzle
          .select({
            processingState: schema.githubWebhookDelivery.processingState,
            outcome: schema.githubWebhookDelivery.outcome,
            failureCode: schema.githubWebhookDelivery.failureCode,
          })
          .from(schema.githubWebhookDelivery)
          .where(eq(schema.githubWebhookDelivery.deliveryId, "delivery-malformed"));
        assert.deepStrictEqual(rows, [
          {
            processingState: "rejected",
            outcome: "malformed",
            failureCode: "malformed_payload",
          },
        ]);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

function pullRequest(action: string, number: number, headRepo: { id: number } | null, extra = {}) {
  return {
    action,
    ...extra,
    installation: { id: 17 },
    repository: { id: 42 },
    pull_request: {
      number,
      title: "Add billing",
      user: { login: "maya", type: "User" },
      head: { ref: "feature/billing", sha: "c".repeat(40), repo: headRepo },
      base: { ref: "main" },
      draft: false,
      merged: false,
      merge_commit_sha: null,
      commits: 3,
    },
  };
}

it.live(
  "records handled pull request deliveries and completes them as ignored",
  () =>
    Effect.gen(function* () {
      const testDatabase = yield* postgresTestDatabase;
      const sent: unknown[] = [];
      const inngest = new Inngest({ id: "github-webhook-pull-request" });
      inngest.send = async (input) => {
        sent.push(input);
        return { ids: ["event-1"] };
      };
      const config = AppConfig.layer.pipe(
        Layer.provide(
          ConfigProvider.layer(
            ConfigProvider.fromEnv({
              env: {
                ...testConfigEnvironment(),
                DATABASE_URL: testDatabase.url.href,
                GITHUB_APP_WEBHOOK_SECRET: webhookSecret,
              },
            }),
          ),
        ),
      );
      const layer = Layer.mergeAll(
        config,
        DatabaseLive.pipe(Layer.provide(config)),
        Layer.succeed(InngestClient, inngest),
      );

      yield* Effect.gen(function* () {
        const answers = [];
        for (const [deliveryId, payload] of [
          ["delivery-pr-labeled", pullRequest("labeled", 1, { id: 42 })],
          ["delivery-pr-title-edit", pullRequest("edited", 1, { id: 42 }, { changes: { title: { from: "Old" } } })],
          ["delivery-pr-opened", pullRequest("opened", 1, { id: 42 })],
          ["delivery-pr-retarget", pullRequest("edited", 1, { id: 42 }, { changes: { base: { ref: { from: "dev" } } } })],
          ["delivery-pr-fork", pullRequest("synchronize", 2, { id: 7 })],
          ["delivery-pr-deleted-fork", pullRequest("closed", 3, null)],
          ["delivery-pr-malformed", { action: "opened", installation: { id: 17 } }],
        ] as const) {
          const response = yield* handleGithubWebhookRequest(
            request("pull_request", deliveryId, payload),
          );
          answers.push([deliveryId, response.status]);
        }
        assert.deepStrictEqual(answers, [
          ["delivery-pr-labeled", 200],
          ["delivery-pr-title-edit", 200],
          ["delivery-pr-opened", 200],
          ["delivery-pr-retarget", 200],
          ["delivery-pr-fork", 200],
          ["delivery-pr-deleted-fork", 200],
          ["delivery-pr-malformed", 422],
        ]);
        const badSignature = yield* handleGithubWebhookRequest(
          new Request("http://localhost/api/github/webhook", {
            method: "POST",
            headers: {
              "x-hub-signature-256": "sha256=invalid",
              "x-github-event": "pull_request",
              "x-github-delivery": "delivery-pr-forged",
            },
            body: JSON.stringify(pullRequest("opened", 4, { id: 42 })),
          }),
        );
        assert.strictEqual(badSignature.status, 401);
        expect(
          sent.map((event) => (event as { data: { deliveryId: string; pullRequestKey: string } }).data),
        ).toEqual([
            expect.objectContaining({ deliveryId: "delivery-pr-opened", pullRequestKey: "17:42:1" }),
            expect.objectContaining({ deliveryId: "delivery-pr-retarget", pullRequestKey: "17:42:1" }),
            expect.objectContaining({ deliveryId: "delivery-pr-fork", pullRequestKey: "17:42:2" }),
          expect.objectContaining({ deliveryId: "delivery-pr-deleted-fork", pullRequestKey: "17:42:3" }),
        ]);

        // No project has a plan for this repository, so only the database is reached.
        const runEffect = Effect.runPromiseWith(yield* Effect.context<Database>()) as PullRequestEffectRunner;
        for (const [index, event] of sent.entries()) {
          const input = {
            event: event as { name: string; data: unknown },
            step: { run: <T,>(_id: string, fn: () => T) => fn() } as never,
            runId: `run-pr-${index}`,
          };
          yield* Effect.promise(() => executeProcessGithubPullRequestReceived(input, runEffect));
          // Inngest retries replay the same delivery; it stays completed once.
          yield* Effect.promise(() => executeProcessGithubPullRequestReceived(input, runEffect));
        }

        const database = yield* Database;
        const rows = yield* database.drizzle
          .select({
            deliveryId: schema.githubWebhookDelivery.deliveryId,
            eventKind: schema.githubWebhookDelivery.eventKind,
            processingState: schema.githubWebhookDelivery.processingState,
            outcome: schema.githubWebhookDelivery.outcome,
            pullRequestNumber: schema.githubWebhookDelivery.pullRequestNumber,
            pullRequestAction: schema.githubWebhookDelivery.pullRequestAction,
          })
          .from(schema.githubWebhookDelivery)
          .orderBy(schema.githubWebhookDelivery.receiptSequence);
        assert.deepStrictEqual(rows, [
          { deliveryId: "delivery-pr-malformed", eventKind: "pull_request", processingState: "rejected", outcome: "malformed", pullRequestNumber: null, pullRequestAction: null },
          { deliveryId: "delivery-pr-opened", eventKind: "pull_request", processingState: "processed", outcome: "ignored_pull_request", pullRequestNumber: 1, pullRequestAction: "opened" },
          { deliveryId: "delivery-pr-retarget", eventKind: "pull_request", processingState: "processed", outcome: "ignored_pull_request", pullRequestNumber: 1, pullRequestAction: "edited" },
          { deliveryId: "delivery-pr-fork", eventKind: "pull_request", processingState: "processed", outcome: "ignored_fork", pullRequestNumber: 2, pullRequestAction: "synchronize" },
          { deliveryId: "delivery-pr-deleted-fork", eventKind: "pull_request", processingState: "processed", outcome: "ignored_fork", pullRequestNumber: 3, pullRequestAction: "closed" },
        ]);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);
