import { createHmac } from "node:crypto";
import { assert, it } from "@effect/vitest";
import { ConfigProvider, Effect, Layer } from "effect";
import { eq } from "drizzle-orm";
import { Inngest } from "inngest";
import { schema } from "#/db";
import type { JsonValue } from "#/db/schema";
import { InngestClient } from "#/modules/inngest/client";
import { AppConfig } from "#/server/config.server";
import { Database, DatabaseLive } from "#/server/database.server";
import { migrateTestDatabase, postgresTestContainer } from "#/test/postgres";
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
      const container = yield* postgresTestContainer;
      yield* migrateTestDatabase(container.url);
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
                DATABASE_URL: container.url.href,
                ELECTRIC_URL: "http://localhost:30000",
                APP_URL: "http://localhost:3000",
                BETTER_AUTH_SECRET: "better-auth-secret",
                GITHUB_CLIENT_ID: "github-client-id",
                GITHUB_CLIENT_SECRET: "github-client-secret",
                GITHUB_APP_WEBHOOK_SECRET: webhookSecret,
                PLOYZ_RELAY_URL: "https://relay.example.test",
                APP_ENCRYPTION_SECRET:
                  "app-encryption-secret-at-least-32-characters",
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
