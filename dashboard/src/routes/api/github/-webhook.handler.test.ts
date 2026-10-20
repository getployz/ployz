import { testConfigEnvironment } from "#/test/config-environment";
import { expect, it } from "@effect/vitest";
import { ConfigProvider, Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { InngestClient } from "#/modules/inngest/client";
import { AppConfig } from "#/server/config.server";
import { handleGithubWebhookRequest } from "./-webhook.handler";

const GITHUB_MAX_PAYLOAD_BYTES = 25 * 1024 * 1024;
const MiB = 1024 * 1024;

const layer = Layer.mergeAll(
  AppConfig.layer.pipe(
    Layer.provide(ConfigProvider.layer(ConfigProvider.fromEnv({ env: { ...testConfigEnvironment(), DATABASE_URL: "postgres://unused.invalid/ployz" } }))),
  ),
  Layer.succeed(InngestClient, new Inngest({ id: "github-webhook-cap" })),
);

/** A body that hands out `chunk` up to `limit` times, only when read, and counts the reads. */
const countedStream = (chunk: Uint8Array, limit: number) => {
  let pulled = 0;
  const body = new ReadableStream<Uint8Array>({
    pull(controller) {
      if (pulled >= limit) return controller.close();
      pulled += 1;
      controller.enqueue(chunk);
    },
  }, { highWaterMark: 0 });
  return { body, pulled: () => pulled };
};

const unsignedPush = (body: ReadableStream<Uint8Array>, headers: Record<string, string> = {}) =>
  new Request("http://localhost/api/github/webhook", {
    method: "POST",
    headers: { "x-github-event": "push", "x-hub-signature-256": "sha256=invalid", ...headers },
    body,
    duplex: "half",
  } as RequestInit);

it.effect("an unsigned webhook whose Content-Length is over 25 MiB answers 413 without being read", () =>
  Effect.gen(function* () {
    const stream = countedStream(Buffer.alloc(MiB, 0x20), 1);
    const answer = yield* handleGithubWebhookRequest(
      unsignedPush(stream.body, { "content-length": String(GITHUB_MAX_PAYLOAD_BYTES + 1) }),
    );
    expect(answer.status).toBe(413);
    expect(stream.pulled()).toBe(0);
  }).pipe(Effect.provide(layer)));

it.effect("an unsigned webhook body stops being read at 25 MiB and answers 413", () =>
  Effect.gen(function* () {
    const stream = countedStream(Buffer.alloc(MiB, 0x20), 50);
    const answer = yield* handleGithubWebhookRequest(unsignedPush(stream.body));
    expect(answer.status).toBe(413);
    expect(stream.pulled()).toBe(GITHUB_MAX_PAYLOAD_BYTES / MiB + 1);
  }).pipe(Effect.provide(layer)));
