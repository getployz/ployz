import { isIP } from "node:net";
import { Effect } from "effect";
import { CHECKIN_MAX_BYTES } from "#/modules/machines/waiting/checkin-wire";
import { checkIn, CheckinRefused } from "#/modules/machines/waiting/checkin.server";
import { encodePublicError, statusForPublicError, Unauthorized } from "#/server/public-error";

const noStore = { "cache-control": "no-store" };

async function readCappedBody(request: Request) {
  if (Number(request.headers.get("content-length")) > CHECKIN_MAX_BYTES) return null;
  if (request.body === null) return new Uint8Array(0);
  const reader = request.body.getReader();
  const chunks: Uint8Array[] = [];
  let size = 0;
  for (let read = await reader.read(); !read.done; read = await reader.read()) {
    size += read.value.byteLength;
    if (size > CHECKIN_MAX_BYTES) {
      await reader.cancel();
      return null;
    }
    chunks.push(read.value);
  }
  return Buffer.concat(chunks);
}

/** The address the check-in came from: the first valid entry of the proxy's client header, else the socket peer. */
export function observedIp(headers: Headers, clientIpHeaders: ReadonlyArray<string> | undefined, peer: string | undefined) {
  for (const name of clientIpHeaders ?? ["x-forwarded-for"]) {
    const value = headers.get(name)?.split(",")[0]?.trim();
    if (value !== undefined && isIP(value) !== 0) return value;
  }
  return peer !== undefined && isIP(peer) !== 0 ? peer : "unknown";
}

const refused = (status: number, error: string, headers: Record<string, string> = {}) =>
  Response.json({ error }, { status, headers: { ...noStore, ...headers } });

export const handleCheckin = Effect.fn("Checkin.handle")(function* (request: Request, observedIp: string) {
  const body = yield* Effect.tryPromise(() => readCappedBody(request)).pipe(Effect.orElseSucceed(() => new Uint8Array(0)));
  if (body === null) return refused(413, "Check-in is over 64 KiB.");
  return yield* checkIn({ headers: request.headers, body, observedIp }).pipe(
    Effect.map((answer) => Response.json(answer, { headers: noStore })),
    Effect.catchTag("CheckinRefused", (error: CheckinRefused) =>
      Effect.succeed(refused(error.status, error.message, error.status === 429 ? { "retry-after": "1" } : {}))),
    Effect.catchTag("Unauthorized", (error: Unauthorized) => {
      const encoded = encodePublicError(error);
      return Effect.succeed(refused(statusForPublicError(encoded), encoded.message));
    }),
  );
});
