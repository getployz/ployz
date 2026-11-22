import "@tanstack/react-start/server-only";
import { lookup as resolve, type LookupAddress } from "node:dns";
import { request } from "node:https";
import { BlockList, isIP, type LookupFunction } from "node:net";
import type { TunnelAnswer } from "@ployz/sdk";
import { Effect } from "effect";

/** Every Server answers it with its Machine id, behind any door (core `HOSTNAME_VERIFY_PATH`). */
const VERIFY_PATH = "/.well-known/acme-challenge/ployz-verify";
const REACH_TIMEOUT_MS = 3_000;
/** A Machine id is 32 characters; anything much longer is some other page. */
const BODY_LIMIT = 128;

/** Where a hostname a user typed must never send Cloud: its own network and every non-public range. */
const NON_PUBLIC = new BlockList();
for (const [address, prefix] of [
  ["0.0.0.0", 8], ["10.0.0.0", 8], ["100.64.0.0", 10], ["127.0.0.0", 8], ["169.254.0.0", 16], ["172.16.0.0", 12],
  ["192.0.0.0", 24], ["192.168.0.0", 16], ["198.18.0.0", 15], ["224.0.0.0", 3],
] as const) NON_PUBLIC.addSubnet(address, prefix, "ipv4");
// No `::ffff:0:0/96` here: Node matches every IPv4 address against it, and an IPv4-mapped address is judged by the IPv4 ranges above.
for (const [address, prefix] of [["::", 128], ["::1", 128], ["64:ff9b::", 96], ["fc00::", 7], ["fe80::", 10], ["ff00::", 8]] as const) {
  NON_PUBLIC.addSubnet(address, prefix, "ipv6");
}

export const isPublicAddress = (address: string) => {
  const version = isIP(address);
  return version !== 0 && !NON_PUBLIC.check(address, version === 4 ? "ipv4" : "ipv6");
};

/** Resolves like the system does, but hands the connection only public addresses: a name resolving inward is refused. */
const publicLookup: LookupFunction = (hostname, options, done) => {
  resolve(hostname, { ...options, all: true }, (error, found: LookupAddress[]) => {
    if (error) return done(error, "", 0);
    const addresses = found.filter(({ address }) => isPublicAddress(address));
    const [first] = addresses;
    if (first === undefined) {
      return done(Object.assign(new Error(`${hostname} resolves to no public address`), { code: "ENOTPUBLIC" }), "", 0);
    }
    return options.all ? done(null, addresses) : done(null, first.address, first.family);
  });
};

const NOT_FOUND = new Set(["ENOTFOUND", "ENODATA", "ENOTPUBLIC"]);

/**
 * What a fetch that failed with `code` says: a name DNS sends nowhere, or only inward, answers from elsewhere; a
 * refused connection, a failed TLS handshake or a timeout is nobody answering.
 */
export const failedAnswer = (code: string | undefined): TunnelAnswer => code !== undefined && NOT_FOUND.has(code) ? "elsewhere" : "unanswered";

/**
 * Who answers through `hostname` behind Cloudflare: fetch the verify path over https and read a Machine id of the
 * Cluster from the answer. DNS can't say, since Cloudflare hides a proxied CNAME behind its own addresses. A Server's
 * id means the Cluster; a name that doesn't resolve, resolves only inward, or answers with anything else means
 * elsewhere; a refused connection, a failed TLS handshake or a timeout means nobody answered, which a user fixes the
 * same way and so is told about rather than left unobserved.
 */
export const reachesCluster = (hostname: string, machineIds: ReadonlySet<string>): Effect.Effect<TunnelAnswer> => isIP(hostname) !== 0 ? Effect.succeed("elsewhere") : Effect.tryPromise({
  try: (signal) => new Promise<boolean>((done, fail) => {
    const asked = request({
      host: hostname, path: VERIFY_PATH, method: "GET", lookup: publicLookup, signal, timeout: REACH_TIMEOUT_MS,
      headers: { accept: "text/plain" },
    }, (response) => {
      const status = response.statusCode ?? 0;
      let body = "";
      response.setEncoding("utf8");
      response.on("data", (chunk: string) => {
        body += chunk;
        if (body.length > BODY_LIMIT) response.destroy();
      });
      const finish = () => done(status >= 200 && status < 300 && body.length <= BODY_LIMIT && machineIds.has(body.trim()));
      response.on("end", finish);
      response.on("close", finish);
    });
    asked.on("timeout", () => asked.destroy(Object.assign(new Error("timed out"), { code: "ETIMEDOUT" })));
    asked.on("error", fail);
    asked.end();
  }),
  // SAFETY: the promise rejects only with what `request` emits, a Node system error, which may carry a `code`.
  catch: (error) => error as NodeJS.ErrnoException,
}).pipe(
  Effect.map((cluster): TunnelAnswer => cluster ? "cluster" : "elsewhere"),
  Effect.timeout(REACH_TIMEOUT_MS + 1_000),
  Effect.catch((error) => {
    const code = "code" in error ? error.code : undefined;
    const answer = failedAnswer(code);
    return answer === "unanswered" ? Effect.logInfo("A fetch through a hostname got no answer.", { hostname, code }).pipe(Effect.as(answer)) : Effect.succeed(answer);
  }),
);
