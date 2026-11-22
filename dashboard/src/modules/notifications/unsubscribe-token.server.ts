import "@tanstack/react-start/server-only";
import { Option } from "effect";
import { openToken, signToken } from "#/server/signed-token.server";
import { isNotificationType, type NotificationType } from "#/modules/notifications/notification-types";

const UNSUBSCRIBE_KEY_PURPOSE = "ployz:unsubscribe:v1";

export type UnsubscribeTarget = { readonly userId: string; readonly type: NotificationType };

/** Never expires: an old email's link still turns that type off, and turning it off is all it can do. */
export const signUnsubscribeToken = (encryptionSecret: string, { userId, type }: UnsubscribeTarget) =>
  signToken(UNSUBSCRIBE_KEY_PURPOSE, encryptionSecret, `${userId}.${type}`);

export function verifyUnsubscribeToken(encryptionSecret: string, token: string): Option.Option<UnsubscribeTarget> {
  const message = openToken(UNSUBSCRIBE_KEY_PURPOSE, encryptionSecret, token);
  if (message === null) return Option.none();
  const [userId, type, ...rest] = message.split(".");
  if (userId === undefined || type === undefined || rest.length > 0) return Option.none();
  if (!isNotificationType(type)) return Option.none();
  return Option.some({ userId, type });
}
