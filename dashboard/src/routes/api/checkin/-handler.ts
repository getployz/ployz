import { BlockList, isIP } from "node:net";
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

const proxyPeers = new BlockList();
for (const [network, prefix] of [["127.0.0.0", 8], ["10.0.0.0", 8], ["172.16.0.0", 12], ["192.168.0.0", 16], ["169.254.0.0", 16]] as const) {
  proxyPeers.addSubnet(network, prefix, "ipv4");
}
for (const [network, prefix] of [["::1", 128], ["fc00::", 7], ["fe80::", 10]] as const) proxyPeers.addSubnet(network, prefix, "ipv6");

function fromProxy(peer: string) {
  const address = peer.startsWith("::ffff:") && isIP(peer.slice(7)) === 4 ? peer.slice(7) : peer;
  return proxyPeers.check(address, isIP(address) === 4 ? "ipv4" : "ipv6");
}

/** Proxies append, so only the right end of the chain is trustworthy: the first hop that isn't a proxy is the client. */
function clientFromForwardedFor(value: string | null, peer: string) {
  let client = peer;
  for (const hop of (value ?? "").split(",").map((entry) => entry.trim()).reverse()) {
    if (isIP(hop) === 0) break;
    client = hop;
    if (!fromProxy(hop)) break;
  }
  return client;
}

export function observedIp(headers: Headers, clientIpHeaders: ReadonlyArray<string> | undefined, peer: string | undefined) {
  const validPeer = peer !== undefined && isIP(peer) !== 0 ? peer : undefined;
  if (clientIpHeaders !== undefined) {
    for (const name of clientIpHeaders) {
      const value = headers.get(name)?.split(",")[0]?.trim();
      if (value !== undefined && isIP(value) !== 0) return value;
    }
    return validPeer ?? "unknown";
  }
  if (validPeer === undefined) return "unknown";
  return fromProxy(validPeer) ? clientFromForwardedFor(headers.get("x-forwarded-for"), validPeer) : validPeer;
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
