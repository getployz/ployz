import { sendInngestEvent } from "#/modules/inngest/client";
import { createMailSendRequestedEvent, type MailMessage } from "#/modules/mail/mail.events";

/** Queue `message` for the send-mail function. `key` names what the mail is for, so queueing it again sends one mail. */
export const queueMail = (message: MailMessage, key: string) => sendInngestEvent(createMailSendRequestedEvent(message, key));
