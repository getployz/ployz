import "@tanstack/react-start/server-only";
import crypto from "node:crypto";

/** The HMAC key for one use of the app secret, so a signature made for one purpose never verifies for another. */
const keyFor = (purpose: string, secret: string) =>
  crypto.createHash("sha256").update(purpose).update("\0").update(secret).digest();

/** The base64url HMAC-SHA256 of `message` under the key `purpose` derives from `secret`. */
export const signature = (purpose: string, secret: string, message: string) =>
  crypto.createHmac("sha256", keyFor(purpose, secret)).update(message).digest("base64url");

/** `message.signature`. The message may hold dots; the signature never does. */
export const signToken = (purpose: string, secret: string, message: string) =>
  `${message}.${signature(purpose, secret, message)}`;

/** The message of a token `signToken` made with this purpose and secret, or null when its signature doesn't hold. */
export function openToken(purpose: string, secret: string, token: string): string | null {
  const dot = token.lastIndexOf(".");
  if (dot < 0) return null;
  const message = token.slice(0, dot);
  const expected = Buffer.from(signature(purpose, secret, message));
  const actual = Buffer.from(token.slice(dot + 1));
  return actual.length === expected.length && crypto.timingSafeEqual(actual, expected) ? message : null;
}
