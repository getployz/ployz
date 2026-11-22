import { Schema } from "effect";
import { defineEvent } from "#/modules/inngest/define-event";

const NonEmptyString = Schema.String.check(Schema.isNonEmpty());

export const mailSendRequested = defineEvent({
  name: "mail/send",
  schema: Schema.Struct({
    to: NonEmptyString,
    subject: NonEmptyString,
    text: NonEmptyString,
    html: Schema.optionalKey(NonEmptyString),
    headers: Schema.optionalKey(Schema.Record(Schema.String, Schema.String)),
  }),
});

/** The key names what this mail is for: queueing it again sends one mail. */
export const createMailSendRequestedEvent = (message: MailMessage, key: string) =>
  ({ id: `mail-${key}`, ...mailSendRequested.create(message) }) as const;

export type MailMessage = typeof mailSendRequested.schema.Type;
