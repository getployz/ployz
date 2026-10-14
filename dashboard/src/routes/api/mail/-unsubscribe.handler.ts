import { Effect, Option, Redacted } from "effect";
import { NOTIFICATION_TYPES } from "#/modules/notifications/notification-types";
import { unsubscribe } from "#/modules/notifications/preferences.server";
import { verifyUnsubscribeToken } from "#/modules/notifications/unsubscribe-token.server";
import { AppConfig } from "#/server/config.server";

const page = (status: number, title: string, line: string) =>
  new Response(
    `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="color-scheme" content="light dark"><title>${title}</title></head>`
      + `<body style="font-family:system-ui,sans-serif;max-width:32rem;margin:15vh auto;padding:0 24px;line-height:1.5">`
      + `<h1 style="font-size:20px;font-weight:600">${title}</h1><p>${line} <a href="/cloud/account">Notifications</a>.</p></body></html>`,
    { status, headers: { "content-type": "text/html; charset=utf-8", "cache-control": "no-store" } },
  );

/**
 * The footer link (`GET`) and the `List-Unsubscribe-Post` one-click (`POST`) both set the token's type to None for its
 * user, with no sign-in. A bad token writes nothing.
 */
export const handleUnsubscribeRequest = Effect.fn("Notifications.handleUnsubscribe")(function* (request: Request) {
  const config = yield* AppConfig;
  const token = new URL(request.url).searchParams.get("token") ?? "";
  const target = verifyUnsubscribeToken(Redacted.value(config.encryptionSecret), token);
  if (Option.isNone(target)) {
    return page(400, "This link doesn't work", "It may have been cut off or changed. You can turn emails off in");
  }
  yield* unsubscribe(target.value);
  if (request.method === "POST") return new Response(null, { status: 200 });
  return page(200, "Unsubscribed", `You won't get ${NOTIFICATION_TYPES[target.value.type].emails} anymore. Turn them back on in`);
});
