import { Effect, Option } from "effect";
import { NonRetriableError, RetryAfterError } from "inngest";
import type { PloyzInngest, StepTools } from "#/modules/inngest/client";
import { eventData } from "#/modules/inngest/define-event";
import { mailSendRequested } from "#/modules/mail/mail.events";
import { sendMail, type MailSendError } from "#/modules/mail/mailer.server";
import { type EffectRunner, runInngestEffect } from "#/modules/inngest/run.server";

export const SEND_MAIL_RETRIES = 5;

/** Doubling from an hour, the retries outlast PostShiba's 24-hour cap as well as its hourly one. */
export const throttledRetryHours = (attempt: number) => 2 ** attempt;

/** Inngest decides retries, so the Mailer's verdict becomes Inngest's error here and nowhere else. */
const toInngestError = (error: MailSendError, attempt: number) => {
  if (error._tag === "MailRejected") return new NonRetriableError(error.message, { cause: error });
  if (error.throttled) return new RetryAfterError(error.message, `${throttledRetryHours(attempt)}h`, { cause: error });
  return error;
};

/** The event id keys the send, so every retry of this run delivers one mail. */
export async function executeSendMail(
  { event, step, attempt }: { event: { id?: string; data: unknown }; step: StepTools<"run">; attempt: number },
  runEffect: EffectRunner,
) {
  const decoded = eventData(mailSendRequested)(event);
  if (Option.isNone(decoded)) throw new NonRetriableError("The mail/send event carries no sendable mail.");
  const key = event.id;
  if (key === undefined) throw new NonRetriableError("The mail/send event has no id to key its send.");
  return step.run("send-mail", () =>
    runEffect(sendMail(decoded.value, key).pipe(Effect.mapError((error) => toInngestError(error, attempt)))),
  );
}

/** No row owns a mail: its outcome lives in the Inngest run and in PostShiba. */
export const createSendMail = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "send-mail",
      retries: SEND_MAIL_RETRIES,
      triggers: [{ event: mailSendRequested.eventType }],
      // PostShiba's cluster cap: every mail shares one hourly budget.
      throttle: { limit: 100, period: "1h" },
    },
    async ({ event, step, attempt }) => executeSendMail({ event, step, attempt }, runEffect),
  );
