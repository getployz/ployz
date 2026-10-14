import { assert, describe, it } from "@effect/vitest";
import { Effect, Layer, Redacted } from "effect";
import { asTestDouble } from "#/lib/test-double";
import {
  MailRejected,
  MailUnavailable,
  MailerLive,
  makePostShibaMailer,
  sendMail,
} from "#/modules/mail/mailer.server";
import { AppConfig, type MailConfiguration } from "#/server/config.server";

const config: NonNullable<MailConfiguration> = {
  apiKey: Redacted.make("test-key"),
  teamId: "team-1",
  clusterId: "cluster-1",
  from: "Ployz <noreply@example.com>",
};

const message = { to: "nick@example.com", subject: "Hello", text: "Hi there." };

type Request = { readonly url: string; readonly init: RequestInit };
type Answer = string | Readonly<Record<string, string | boolean>>;

function fakeFetch(status: number, body: Answer) {
  const requests: Array<Request> = [];
  const fetch = asTestDouble<typeof globalThis.fetch>()(
    (url: string, init: RequestInit) => {
      requests.push({ url, init });
      return Promise.resolve(Response.json(body, { status }));
    },
  );
  return { fetch, requests };
}

const sendWith = (status: number, body: Answer) =>
  makePostShibaMailer(config, fakeFetch(status, body).fetch).send(message, "key-1");

describe("PostShiba Mailer", () => {
  it.effect("sends to the configured cluster under the idempotency key", () =>
    Effect.gen(function* () {
      const { fetch, requests } = fakeFetch(201, { queued: true, message_id: "m-1@postshiba" });
      const result = yield* makePostShibaMailer(config, fetch).send(message, "key-1");

      assert.deepStrictEqual(result, { status: "sent", messageId: "m-1@postshiba" });
      const [request] = requests;
      assert.strictEqual(requests.length, 1);
      if (request === undefined) return;
      const { url, init } = request;
      assert.strictEqual(url, "https://app.postshiba.com/api/v1/teams/team-1/clusters/cluster-1/sends");
      assert.strictEqual(init.method, "POST");
      const headers = new Headers(init.headers);
      assert.strictEqual(headers.get("idempotency-key"), "key-1");
      assert.strictEqual(headers.get("authorization"), "Bearer test-key");
      assert.deepStrictEqual(JSON.parse(String(init.body)), {
        from: "Ployz <noreply@example.com>",
        to: ["nick@example.com"],
        subject: "Hello",
        text: "Hi there.",
      });
    }),
  );

  it.effect("reports a throttled cluster", () =>
    Effect.gen(function* () {
      const failure = yield* Effect.flip(sendWith(429, { error: "throttled", retryable: true }));
      assert.instanceOf(failure, MailUnavailable);
      assert.strictEqual(failure.throttled, true);
    }),
  );

  it.effect("retries what sent nothing or may be retried with the same key", () =>
    Effect.gen(function* () {
      const answers: ReadonlyArray<readonly [number, Answer]> = [
        [429, { error: "rate_limited" }],
        [409, { error: "idempotency_in_progress" }],
        [503, { error: "inject_unavailable", retryable: true }],
        [503, { error: "idempotency_unavailable" }],
        [500, "not json"],
        [429, "not json"],
      ];
      for (const [status, body] of answers) {
        const failure = yield* Effect.flip(sendWith(status, body));
        assert.instanceOf(failure, MailUnavailable, `${status} ${JSON.stringify(body)}`);
        assert.strictEqual(failure.throttled, false);
      }
    }),
  );

  it.effect("rejects what PostShiba will refuse again", () =>
    Effect.gen(function* () {
      const answers: ReadonlyArray<readonly [number, Answer]> = [
        [401, { error: "Invalid token or no user signed in" }],
        [403, { error: "kyc_required", field: "to", message: "Pending approval" }],
        [409, { error: "idempotency_conflict" }],
        [409, { error: "idempotency_outcome_unknown" }],
        [422, { error: "invalid", field: "from" }],
        [502, { error: "inject_failed", retryable: false }],
      ];
      for (const [status, body] of answers) {
        const failure = yield* Effect.flip(sendWith(status, body));
        assert.instanceOf(failure, MailRejected, `${status} ${JSON.stringify(body)}`);
        assert.strictEqual(failure.status, status);
      }
    }),
  );

  it.effect("retries when PostShiba cannot be reached", () =>
    Effect.gen(function* () {
      const fetch = asTestDouble<typeof globalThis.fetch>()(() => Promise.reject(new TypeError("fetch failed")));
      const failure = yield* Effect.flip(makePostShibaMailer(config, fetch).send(message, "key-1"));
      assert.instanceOf(failure, MailUnavailable);
      assert.strictEqual(failure.code, "network");
    }),
  );

  it.effect("retries when PostShiba stalls on the response body", () =>
    Effect.gen(function* () {
      const fetch = asTestDouble<typeof globalThis.fetch>()(
        (_url: string, init: RequestInit) => {
          const body = new ReadableStream({
            start: (controller) => init.signal?.addEventListener("abort", () => controller.error(init.signal?.reason)),
          });
          return Promise.resolve(new Response(body, { status: 201 }));
        },
      );
      const error = yield* Effect.flip(makePostShibaMailer(config, fetch, 50).send(message, "key-1"));

      assert.instanceOf(error, MailUnavailable);
    }),
  );
});

describe("MailerLive", () => {
  const withMail = (mail: MailConfiguration) =>
    Layer.provide(MailerLive, Layer.succeed(AppConfig, asTestDouble<AppConfig["Service"]>()({ mail })));

  it.effect("skips and sends nothing when mail is off", () =>
    Effect.gen(function* () {
      const { fetch, requests } = fakeFetch(201, { message_id: "never" });
      const original = globalThis.fetch;
      globalThis.fetch = fetch;
      const result = yield* sendMail(message, "key-1").pipe(
        Effect.provide(withMail(null)),
        Effect.ensuring(Effect.sync(() => { globalThis.fetch = original; })),
      );

      assert.deepStrictEqual(result, { status: "skipped" });
      assert.strictEqual(requests.length, 0);
    }),
  );
});
