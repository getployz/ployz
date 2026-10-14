import "@tanstack/react-start/server-only";
import { Context, Data, Effect, Layer, Option, Redacted, Schema } from "effect";
import { AppConfig, type MailConfiguration } from "#/server/config.server";

export type MailMessage = {
  readonly to: string;
  readonly subject: string;
  readonly text: string;
  readonly html?: string;
};

export type MailSendResult =
  | { readonly status: "sent"; readonly messageId: string }
  | { readonly status: "skipped" };

/** PostShiba refused the message; sending it again gets the same answer. */
export class MailRejected extends Data.TaggedError("MailRejected")<{
  readonly status: number;
  readonly code: string;
  readonly message: string;
}> {}

/** Nothing was sent, or the key holds the outcome: sending again with the same key is safe. */
export class MailUnavailable extends Data.TaggedError("MailUnavailable")<{
  readonly status: number | null;
  readonly code: string;
  readonly message: string;
  /** At the cluster's hourly or 24-hour cap, which PostShiba does not tell apart: retrying soon hits it again. */
  readonly throttled: boolean;
}> {}

export type MailSendError = MailRejected | MailUnavailable;

export type MailerService = {
  /** The key makes a retried send deliver once; PostShiba holds it for 24 hours. */
  readonly send: (
    message: MailMessage,
    idempotencyKey: string,
  ) => Effect.Effect<MailSendResult, MailSendError>;
};

export class Mailer extends Context.Service<Mailer, MailerService>()("ployz/Mailer") {}

const POSTSHIBA_ORIGIN = "https://app.postshiba.com";
const REQUEST_TIMEOUT_MS = 15_000;

const SendAccepted = Schema.Struct({ message_id: Schema.String });
const SendFailure = Schema.Struct({
  error: Schema.optionalKey(Schema.String),
  message: Schema.optionalKey(Schema.String),
  retryable: Schema.optionalKey(Schema.Boolean),
});

/** 409 idempotency_in_progress: the first request with this key is still running. */
const RETRYABLE_CODES = new Set(["throttled", "rate_limited", "idempotency_in_progress"]);

const failureOf = (status: number, failure: typeof SendFailure.Type): MailSendError => {
  const code = failure.error ?? `http_${status}`;
  const message = failure.message ?? `PostShiba answered ${status} (${code}).`;
  // inject errors carry their own verdict: inject_failed can be final.
  const retryable = RETRYABLE_CODES.has(code) || (failure.retryable ?? status >= 500);
  return retryable
    ? new MailUnavailable({ status, code, message, throttled: code === "throttled" })
    : new MailRejected({ status, code, message });
};

export const makePostShibaMailer = (
  config: NonNullable<MailConfiguration>,
  fetch: typeof globalThis.fetch,
  timeoutMs = REQUEST_TIMEOUT_MS,
): MailerService => ({
  send: Effect.fn("Mailer.send")(function* (message, idempotencyKey) {
    const url = `${POSTSHIBA_ORIGIN}/api/v1/teams/${encodeURIComponent(config.teamId)}/clusters/${encodeURIComponent(config.clusterId)}/sends`;
    // JSON.stringify drops an undefined html.
    const payload = {
      from: config.from,
      to: [message.to],
      subject: message.subject,
      text: message.text,
      html: message.html,
    };
    const { response, body } = yield* Effect.tryPromise({
      try: async (signal) => {
        const response = await fetch(url, {
          method: "POST",
          signal: AbortSignal.any([signal, AbortSignal.timeout(timeoutMs)]),
          headers: {
            authorization: `Bearer ${Redacted.value(config.apiKey)}`,
            accept: "application/json",
            "content-type": "application/json",
            "idempotency-key": idempotencyKey,
          },
          body: JSON.stringify(payload),
        });
        // A stalled or unreadable body falls through to the status, and a 2xx without one retries.
        const body: unknown = await response.json().catch(() => null);
        return { response, body };
      },
      catch: (cause) =>
        new MailUnavailable({
          status: null,
          code: "network",
          message: `PostShiba could not be reached: ${cause instanceof Error ? cause.message : String(cause)}`,
          throttled: false,
        }),
    });
    if (!response.ok) {
      const failure = Option.getOrElse(Schema.decodeUnknownOption(SendFailure)(body), () => ({}));
      return yield* failureOf(response.status, failure);
    }
    const accepted = Schema.decodeUnknownOption(SendAccepted)(body);
    if (Option.isNone(accepted)) {
      // The key holds whatever PostShiba did, so the retry returns the first answer.
      return yield* new MailUnavailable({
        status: response.status,
        code: "unreadable_response",
        message: `PostShiba answered ${response.status} without a message id.`,
        throttled: false,
      });
    }
    return { status: "sent", messageId: accepted.value.message_id } as const;
  }),
});

/** Off: Cloud runs without email. The body stays out of the log. */
export const offMailer: MailerService = {
  send: (message, idempotencyKey) =>
    Effect.logInfo("Mail is off; skipped a send.", { subject: message.subject, idempotencyKey }).pipe(
      Effect.as({ status: "skipped" } as const),
    ),
};

export const MailerLive = Layer.effect(
  Mailer,
  Effect.map(AppConfig, ({ mail }) =>
    mail === null ? offMailer : makePostShibaMailer(mail, globalThis.fetch)),
);

export const sendMail = Effect.fn("Mail.send")(function* (message: MailMessage, idempotencyKey: string) {
  const mailer = yield* Mailer;
  return yield* mailer.send(message, idempotencyKey);
});
