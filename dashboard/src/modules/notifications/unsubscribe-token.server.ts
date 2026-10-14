import "@tanstack/react-start/server-only";
import crypto from "node:crypto";
import { Option } from "effect";
import { isNotificationType, type NotificationType } from "#/modules/notifications/notification-types";

const UNSUBSCRIBE_KEY_PURPOSE = "ployz:unsubscribe:v1";

export type UnsubscribeTarget = { readonly userId: string; readonly type: NotificationType };

const signature = (encryptionSecret: string, userId: string, type: string) =>
  crypto
    .createHmac("sha256", crypto.createHash("sha256").update(UNSUBSCRIBE_KEY_PURPOSE).update("\0").update(encryptionSecret).digest())
    .update(`${userId}.${type}`)
    .digest("base64url");

/** Never expires: an old email's link still turns that type off, and turning it off is all it can do. */
export const signUnsubscribeToken = (encryptionSecret: string, { userId, type }: UnsubscribeTarget) =>
  `${userId}.${type}.${signature(encryptionSecret, userId, type)}`;

export function verifyUnsubscribeToken(encryptionSecret: string, token: string): Option.Option<UnsubscribeTarget> {
  const [userId, type, given, ...rest] = token.split(".");
  if (userId === undefined || type === undefined || given === undefined || rest.length > 0) return Option.none();
  if (!isNotificationType(type)) return Option.none();
  const expected = Buffer.from(signature(encryptionSecret, userId, type));
  const actual = Buffer.from(given);
  if (actual.length !== expected.length || !crypto.timingSafeEqual(actual, expected)) return Option.none();
  return Option.some({ userId, type });
}
