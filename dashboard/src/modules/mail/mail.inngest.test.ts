import { InngestTestEngine } from "@inngest/test";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { describe, expect, it } from "vitest";
import { createSendMail } from "#/modules/mail/mail.inngest";
import {
  MailRejected,
  MailUnavailable,
  Mailer,
  type MailMessage,
  type MailSendError,
  type MailSendResult,
} from "#/modules/mail/mailer.server";
import { makeInngestEffectRunner, type runInngestEffect } from "#/server/run.server";

type Send = { readonly message: MailMessage; readonly idempotencyKey: string };

function runWithMailer(sends: Array<Send>, answer: Effect.Effect<MailSendResult, MailSendError>) {
  return makeInngestEffectRunner(<A, E>(effect: Effect.Effect<A, E, Mailer>) =>
    Effect.runPromise(
      effect.pipe(
        Effect.provideService(Mailer, {
          send: (message, idempotencyKey) =>
            Effect.sync(() => sends.push({ message, idempotencyKey })).pipe(Effect.andThen(answer)),
        }),
      ),
    ),
  ) as typeof runInngestEffect;
}

const message = { to: "nick@example.com", subject: "Hello", text: "Hi there." };

const run = (
  sends: Array<Send>,
  answer: Effect.Effect<MailSendResult, MailSendError>,
  data: Partial<MailMessage> = message,
) =>
  new InngestTestEngine({
    function: createSendMail(new Inngest({ id: "test" }), runWithMailer(sends, answer)),
    events: [{ id: "mail-invite-1", name: "mail/send", data }],
  }).execute();

describe("send-mail", () => {
  it("sends the queued mail keyed by its event id", async () => {
    const sends: Array<Send> = [];
    const output = await run(sends, Effect.succeed({ status: "sent", messageId: "m-1@postshiba" }));

    expect(output.result).toEqual({ status: "sent", messageId: "m-1@postshiba" });
    expect(sends).toEqual([{ message, idempotencyKey: "mail-invite-1" }]);
  });

  it("stops retrying a mail PostShiba rejected", async () => {
    const output = await run(
      [],
      Effect.fail(new MailRejected({ status: 403, code: "suppressed", message: "Recipient is suppressed." })),
    );

    expect(output.error).toEqual(
      expect.objectContaining({
        message: "Recipient is suppressed.",
        stack: expect.stringContaining("NonRetriableError"),
      }),
    );
  });

  it("waits an hour when the cluster is throttled", async () => {
    const output = await run(
      [],
      Effect.fail(new MailUnavailable({ status: 429, code: "throttled", message: "Cap reached.", retryAfter: "1h" })),
    );

    expect(output.error).toEqual(
      expect.objectContaining({ message: "Cap reached.", stack: expect.stringContaining("RetryAfterError") }),
    );
  });

  it("retries a mail PostShiba could not take yet", async () => {
    const output = await run(
      [],
      Effect.fail(new MailUnavailable({ status: 503, code: "inject_unavailable", message: "Busy.", retryAfter: null })),
    );

    expect(output.error).toEqual(expect.objectContaining({ message: "Busy." }));
    expect(output.error).not.toEqual(expect.objectContaining({ stack: expect.stringMatching(/NonRetriableError|RetryAfterError/) }));
  });

  it("sends nothing for an event without a sendable mail", async () => {
    const sends: Array<Send> = [];
    const output = await run(sends, Effect.succeed({ status: "skipped" }), { to: "nick@example.com" });

    expect(output.error).toEqual(expect.objectContaining({ stack: expect.stringContaining("NonRetriableError") }));
    expect(sends).toEqual([]);
  });
});
