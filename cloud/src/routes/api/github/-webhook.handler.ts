import { Data, Effect, Option, Result as EffectResult, Schema } from "effect";
import {
  decodeGithubCheckSuitePayload,
  decodeGithubInstallationPayload,
  decodeGithubInstallationRepositoriesPayload,
  decodeGithubPushPayload,
} from "#/modules/github/github-webhook-contracts";
import { rejectMalformedGithubDelivery } from "#/modules/github/github-ingestion.repository";
import type { GithubMalformedDeliveryInput } from "#/modules/github/github-ingestion.repository.types";
import { verifyWebhookSignature } from "#/modules/github/github.server";
import {
  createGithubCheckSuiteReceivedEvent,
  createGithubInstallationReceivedEvent,
  createGithubInstallationRepositoriesReceivedEvent,
  createGithubPushReceivedEvent,
} from "#/modules/inngest/events";
import { sendInngestEvent } from "#/modules/inngest/client";
import { publicErrorResponse } from "#/server/public-error";

class GithubWebhookReadError extends Data.TaggedError(
  "GithubWebhookReadError",
)<{ readonly cause: unknown }> {}

function rejectDelivery(input: GithubMalformedDeliveryInput) {
  return rejectMalformedGithubDelivery(input).pipe(
    Effect.as(new Response("Rejected webhook", { status: 422 })),
    Effect.catch((cause) => {
      console.error("[github:webhook] failed to persist rejected delivery", {
        deliveryId: input.deliveryId,
        eventKind: input.eventKind,
        rejection: input.rejection,
      });
      const conflict =
        "code" in cause &&
        cause.code === "delivery_conflict" &&
        "retriable" in cause &&
        cause.retriable === false;
      return Effect.succeed(
        publicErrorResponse(cause, { status: conflict ? 409 : 503 }),
      );
    }),
  );
}

export const handleGithubWebhookRequest = Effect.fn(
  "Github.handleWebhookRequest",
)(function* (request: Request) {
  const body = yield* Effect.tryPromise({
    try: () => request.text(),
    catch: (cause) => new GithubWebhookReadError({ cause }),
  });
  const signature = request.headers.get("x-hub-signature-256");
  const event = request.headers.get("x-github-event");
  const deliveryId = request.headers.get("x-github-delivery");

  const valid = yield* verifyWebhookSignature(body, signature);
  if (!valid) {
    console.warn("[github:webhook] invalid signature", { deliveryId, event });
    return new Response("Invalid signature", { status: 401 });
  }
  if (!deliveryId) {
    console.warn("[github:webhook] missing delivery id", { event });
    return new Response("Missing delivery id", { status: 400 });
  }

  const payload = Schema.decodeUnknownOption(
    Schema.fromJsonString(Schema.Unknown),
  )(body);
  if (Option.isNone(payload)) {
    return event === "push" || event === "check_suite"
      ? yield* rejectDelivery({ deliveryId, eventKind: event, rejection: "malformed" })
      : new Response("Malformed webhook", { status: 400 });
  }
  if (event === "ping") return new Response("OK", { status: 200 });

  if (event === "push") {
    const decoded = decodeGithubPushPayload(payload.value);
    if (EffectResult.isFailure(decoded)) {
      return yield* rejectDelivery({
        deliveryId,
        eventKind: "push",
        rejection: "malformed",
      });
    }
    yield* sendInngestEvent(
      createGithubPushReceivedEvent({
        deliveryId,
        ...decoded.success,
      }),
    );
    return new Response("OK", { status: 200 });
  }

  if (event === "check_suite") {
    const decoded = decodeGithubCheckSuitePayload(payload.value);
    if (EffectResult.isFailure(decoded)) {
      return yield* rejectDelivery({
        deliveryId,
        eventKind: "check_suite",
        rejection:
          decoded.failure.code === "unsupported_action"
            ? "unsupported_action"
            : "malformed",
      });
    }
    yield* sendInngestEvent(
      createGithubCheckSuiteReceivedEvent({
        deliveryId,
        ...decoded.success,
      }),
    );
    return new Response("OK", { status: 200 });
  }

  if (event === "installation") {
    const decoded = decodeGithubInstallationPayload(payload.value);
    if (EffectResult.isFailure(decoded)) {
      return new Response("Rejected webhook", { status: 422 });
    }
    yield* sendInngestEvent(
      createGithubInstallationReceivedEvent({
        deliveryId,
        ...decoded.success,
      }),
    );
    return new Response("OK", { status: 200 });
  }

  if (event === "installation_repositories") {
    const decoded = decodeGithubInstallationRepositoriesPayload(payload.value);
    if (EffectResult.isFailure(decoded)) {
      return new Response("Rejected webhook", { status: 422 });
    }
    yield* sendInngestEvent(
      createGithubInstallationRepositoriesReceivedEvent({
        deliveryId,
        ...decoded.success,
      }),
    );
    return new Response("OK", { status: 200 });
  }

  console.warn("[github:webhook] unsupported event", { deliveryId, event });
  return new Response("Unsupported webhook event", { status: 422 });
});
