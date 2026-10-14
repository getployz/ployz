import { Effect, Option, Schema } from "effect";
import { NonRetriableError, RetryAfterError } from "inngest";
import { sendInngestEvent, type PloyzInngest, type PloyzStepTools } from "#/modules/inngest/client";
import { createMailSendRequestedEvent, mailSendRequestedEventType } from "#/modules/inngest/events";
import { sendMail, type MailMessage, type MailSendError } from "#/modules/mail/mailer.server";
import { runInngestEffect } from "#/server/run.server";

type StepTools = Pick<PloyzStepTools, "run">;
type EffectRunner = typeof runInngestEffect;

const NonEmptyString = Schema.String.check(Schema.isNonEmpty());
const MailSendRequestedData = Schema.Struct({
  to: NonEmptyString,
  subject: NonEmptyString,
  text: NonEmptyString,
  html: Schema.optionalKey(NonEmptyString),
});

/** Inngest decides retries, so the Mailer's verdict becomes Inngest's error here and nowhere else. */
const toInngestError = (error: MailSendError) => {
  if (error._tag === "MailRejected") return new NonRetriableError(error.message, { cause: error });
  if (error.retryAfter !== null) return new RetryAfterError(error.message, error.retryAfter, { cause: error });
  return error;
};

/** The event id keys the send, so every retry of this run delivers one mail. */
export async function executeSendMail(
  { event, step }: { event: { id?: string; data: unknown }; step: StepTools },
  runEffect: EffectRunner,
) {
  const decoded = Schema.decodeUnknownOption(MailSendRequestedData)(event.data);
  if (Option.isNone(decoded)) throw new NonRetriableError("The mail/send event carries no sendable mail.");
  const key = event.id;
  if (key === undefined) throw new NonRetriableError("The mail/send event has no id to key its send.");
  return step.run("send-mail", () =>
    runEffect(sendMail(decoded.value, key).pipe(Effect.mapError(toInngestError))),
  );
}

/** No row owns a mail: its outcome lives in the Inngest run and in PostShiba. */
export const createSendMail = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "send-mail",
      retries: 5,
      triggers: [{ event: mailSendRequestedEventType }],
      // PostShiba's cluster cap: every mail shares one hourly budget.
      throttle: { limit: 100, period: "1h" },
    },
    async ({ event, step }) => executeSendMail({ event, step }, runEffect),
  );

export const queueMail = (message: MailMessage, key: string) =>
  sendInngestEvent(createMailSendRequestedEvent(message, key));
