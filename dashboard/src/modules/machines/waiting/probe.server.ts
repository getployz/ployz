import "@tanstack/react-start/server-only";
import { randomBytes } from "node:crypto";
import { BlockList, connect, isIP } from "node:net";
import { and, eq, inArray, isNull, sql } from "drizzle-orm";
import { Effect, Option, Schema } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { type CheckinRequest, PROBE_WINDOW_MS, type ProbeRequest } from "#/modules/machines/waiting/checkin-wire";
import { type PortProbe, ProbeResults } from "#/modules/machines/waiting/probe";
import { waitingServer } from "#/modules/machines/waiting/tables";
import { requireAccess } from "#/modules/organization/access.server";
import { afterDatabaseCommit, Database } from "#/server/database.server";

const MAX_PEERS = 3;
const READ_CAP_BYTES = 64;
const CONNECT_DELAY = "1 second";
const CONNECT_TIMEOUT_MS = 3_000;
const GREETING_WAIT_MS = 500;

const notPublic = new BlockList();
for (const [network, prefix] of [
  ["0.0.0.0", 8], ["10.0.0.0", 8], ["100.64.0.0", 10], ["127.0.0.0", 8], ["169.254.0.0", 16], ["172.16.0.0", 12],
  ["192.0.0.0", 24], ["192.0.2.0", 24], ["192.168.0.0", 16], ["198.18.0.0", 15], ["198.51.100.0", 24],
  ["203.0.113.0", 24], ["224.0.0.0", 4], ["240.0.0.0", 4],
] as const) notPublic.addSubnet(network, prefix, "ipv4");
for (const [network, prefix] of [
  ["::", 128], ["::1", 128], ["64:ff9b::", 96], ["64:ff9b:1::", 48], ["100::", 64],
  ["2001:db8::", 32], ["fc00::", 7], ["fe80::", 10], ["ff00::", 8],
] as const) notPublic.addSubnet(network, prefix, "ipv6");

const family = (ip: string) => (isIP(ip) === 4 ? "ipv4" : "ipv6");
const unmapped = (ip: string) => (ip.toLowerCase().startsWith("::ffff:") && isIP(ip.slice(7)) === 4 ? ip.slice(7) : ip);

function devOnlyProbeAllowList() {
  const cidrs = import.meta.env.DEV ? process.env["PLOYZ_VERIFY_PROBE_ALLOW"] ?? "" : "";
  const allow = new BlockList();
  for (const cidr of cidrs.split(",").map((entry) => entry.trim()).filter(Boolean)) {
    const [network = "", prefix = ""] = cidr.split("/");
    if (isIP(network) !== 0) allow.addSubnet(network, Number(prefix), family(network));
  }
  return allow;
}

export function refusedTarget(ip: string, allow: BlockList = devOnlyProbeAllowList()): "no_address" | "private_range" | null {
  if (isIP(ip) === 0) return "no_address";
  const address = unmapped(ip);
  if (allow.check(address, family(address))) return null;
  return notPublic.check(address, family(address)) ? "private_range" : null;
}

export function connectOnce(host: string, port: number, options: { readonly timeoutMs?: number; readonly greetingMs?: number } = {}) {
  return new Promise<{ readonly state: "open" | "closed" | "unknown"; readonly bytesRead: number }>((resolve) => {
    let bytesRead = 0;
    let state: "open" | "closed" | "unknown" = "unknown";
    const finish = () => {
      socket.destroy();
      resolve({ state, bytesRead });
    };
    const socket = connect({
      host,
      port,
      onread: {
        buffer: Buffer.alloc(READ_CAP_BYTES),
        callback: (read) => {
          bytesRead = read;
          finish();
          return false;
        },
      },
    });
    socket.setTimeout(options.timeoutMs ?? CONNECT_TIMEOUT_MS, finish);
    socket.once("error", (error: NodeJS.ErrnoException) => {
      if (state === "unknown" && error.code === "ECONNREFUSED") state = "closed";
      finish();
    });
    socket.once("close", finish);
    socket.once("connect", () => {
      state = "open";
      socket.setTimeout(options.greetingMs ?? GREETING_WAIT_MS, finish);
    });
  });
}

const probePort = (ip: string, port: number) =>
  Effect.promise(() => connectOnce(unmapped(ip), port)).pipe(
    Effect.tap(({ state, bytesRead }) => Effect.logInfo(`Probe ${ip}:${port} ${state}, read ${bytesRead} bytes`)),
    Effect.map(({ state }): PortProbe => (state === "unknown" ? { state, reason: "no_answer" } : { state })),
  );

const recordTcp = Effect.fn("Probe.recordTcp")(function* (id: string, nonce: string, tcp: ProbeResults["tcp"]) {
  const { drizzle } = yield* Database;
  yield* drizzle
    .update(waitingServer)
    .set({ probe: sql`jsonb_set(${waitingServer.probe}, '{tcp}', ${JSON.stringify(tcp)}::jsonb)` })
    .where(and(eq(waitingServer.id, id), sql`${waitingServer.probe}->'request'->>'nonce' = ${nonce}`));
});

const inboundProbe = Effect.fn("Probe.inbound")(function* (id: string, nonce: string, observedIp: string) {
  const refused = refusedTarget(observedIp);
  if (refused !== null) {
    const unknown: PortProbe = { state: "unknown", reason: refused };
    return yield* recordTcp(id, nonce, { "80": unknown, "443": unknown });
  }
  yield* Effect.sleep(CONNECT_DELAY);
  const tcp = yield* Effect.all({ "80": probePort(observedIp, 80), "443": probePort(observedIp, 443) }, { concurrency: "unbounded" });
  yield* recordTcp(id, nonce, tcp);
});

export const decodeStoredProbe = Schema.decodeUnknownOption(ProbeResults);

export const handOutProbe = Effect.fn("Probe.handOut")(function* (
  id: string,
  pending: Option.Option<ProbeResults>,
  observedIp: string,
) {
  if (Option.isNone(pending) || pending.value.handedAt !== undefined) return null;
  const { drizzle } = yield* Database;
  const [handed] = yield* drizzle
    .update(waitingServer)
    .set({ probe: sql`jsonb_set(${waitingServer.probe}, '{handedAt}', to_jsonb(now()))` })
    .where(and(eq(waitingServer.id, id), sql`not ${waitingServer.probe} ? 'handedAt'`))
    .returning({ id: waitingServer.id });
  if (handed === undefined) return null;
  const request = pending.value.request;
  yield* afterDatabaseCommit(Effect.forkDetach(inboundProbe(id, request.nonce, observedIp).pipe(
    Effect.catchCause((cause) => Effect.logWarning("A waiting server's inbound probe failed.", cause)),
  )));
  return request;
});

export const withHostReport = (report: NonNullable<CheckinRequest["probe"]>) => {
  const host = JSON.stringify({ heardFrom: report.heardFrom, bindFailed: report.bindFailed });
  return sql`case when ${waitingServer.probe}->'request'->>'nonce' = ${report.nonce}
    then jsonb_set(${waitingServer.probe}, '{host}', ${host}::jsonb) else ${waitingServer.probe} end`;
};

export function probePeers(hosts: ReadonlyArray<{ readonly id: string; readonly observedIp: string }>) {
  return new Map(hosts.map((host, index) => {
    const targets = new Set<string>();
    for (let step = 1; step < hosts.length && targets.size < MAX_PEERS; step++) {
      const peer = hosts[(index + step) % hosts.length];
      if (peer !== undefined && isIP(peer.observedIp) !== 0 && peer.observedIp !== host.observedIp) targets.add(peer.observedIp);
    }
    return [host.id, [...targets].map((target) => ({ target, port: 51820 as const }))];
  }));
}

export const openProbeSelection = Effect.fn("Probe.openSelection")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string; readonly ids: ReadonlyArray<string> },
) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "operate");
  const { drizzle } = yield* Database;
  const hosts = yield* drizzle
    .select({ id: waitingServer.id, observedIp: waitingServer.observedIp })
    .from(waitingServer)
    .where(and(
      eq(waitingServer.organizationId, organization.id),
      inArray(waitingServer.id, [...input.ids]),
      isNull(waitingServer.rejectedAt),
    ))
    .orderBy(waitingServer.firstSeenAt, waitingServer.id);
  const nonce = randomBytes(16).toString("base64url");
  if (hosts.length === 0) return { nonce, probing: 0 };
  const pending: PortProbe = { state: "unknown", reason: "pending" };
  const probes = Object.fromEntries([...probePeers(hosts)].map(([id, send]) => {
    const request: ProbeRequest = { nonce, windowMs: PROBE_WINDOW_MS, listen: ["udp:51820", "tcp:80", "tcp:443"], send };
    return [id, { request, tcp: { "80": pending, "443": pending } } satisfies ProbeResults];
  }));
  yield* drizzle
    .update(waitingServer)
    .set({ probe: sql`${JSON.stringify(probes)}::jsonb -> (${waitingServer.id}::text)` })
    .where(and(eq(waitingServer.organizationId, organization.id), inArray(waitingServer.id, hosts.map((host) => host.id))));
  return { nonce, probing: hosts.length };
});
