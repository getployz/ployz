import "@tanstack/react-start/server-only";
import { randomBytes } from "node:crypto";
import { BlockList, connect, isIP } from "node:net";
import { and, eq, inArray, isNull, sql } from "drizzle-orm";
import { Effect, Option, Schema } from "effect";
import ipaddr from "ipaddr.js";
import type { Actor } from "#/modules/identity/actor";
import { type CheckinRequest, PROBE_WINDOW_MS, type ProbeRequest } from "#/modules/machines/waiting/checkin-wire";
import { ProbeResults, type TcpObservation } from "#/modules/machines/waiting/probe";
import { waitingServer } from "#/modules/machines/waiting/tables";
import { requireAccess } from "#/modules/organization/access.server";
import { afterDatabaseCommit, Database } from "#/server/database.server";

const MAX_PEERS = 3;
const READ_CAP_BYTES = 64;

export const WINDOW_SCHEDULE = { firstAfterMs: 1_000, everyMs: 1_500, timeoutMs: 3_000, greetingMs: 500 };

const ipv4Compatible = new BlockList();
ipv4Compatible.addSubnet("::", 96, "ipv6");

function devOnlyProbeAllowList() {
  const cidrs = import.meta.env.DEV ? process.env["PLOYZ_VERIFY_PROBE_ALLOW"] ?? "" : "";
  const allow = new BlockList();
  for (const cidr of cidrs.split(",").map((entry) => entry.trim()).filter(ipaddr.isValidCIDR)) {
    const [network, prefix] = ipaddr.parseCIDR(cidr);
    allow.addSubnet(network.toString(), prefix, network.kind() === "ipv4" ? "ipv4" : "ipv6");
  }
  return allow;
}

const probeAddress = (ip: string) => ipaddr.process(ip).toString();

export function refusedTarget(ip: string, allow: BlockList = devOnlyProbeAllowList()): "no_address" | "private_range" | null {
  if (isIP(ip) === 0) return "no_address";
  const address = ipaddr.process(ip);
  if (allow.check(address.toString(), address.kind() === "ipv4" ? "ipv4" : "ipv6")) return null;
  if (isIP(ip) === 6 && ipv4Compatible.check(ip, "ipv6")) return "private_range";
  return address.range() === "unicast" ? null : "private_range";
}

export type ConnectSeen = "answered" | "other_listener" | "refused" | "no_answer";

export function connectOnce(
  host: string,
  port: number,
  nonce: string,
  options: { readonly timeoutMs?: number; readonly greetingMs?: number } = {},
) {
  const expected = Buffer.from(nonce);
  return new Promise<{ readonly seen: ConnectSeen; readonly bytesRead: number }>((resolve) => {
    const received = Buffer.alloc(READ_CAP_BYTES);
    let bytesRead = 0;
    let connected = false;
    let refused = false;
    let done = false;
    const finish = () => {
      if (done) return;
      done = true;
      socket.destroy();
      const answered = bytesRead >= expected.length && received.subarray(0, expected.length).equals(expected);
      const seen = connected ? (answered ? "answered" : "other_listener") : refused ? "refused" : "no_answer";
      resolve({ seen, bytesRead });
    };
    const socket = connect({
      host,
      port,
      onread: {
        buffer: Buffer.alloc(READ_CAP_BYTES),
        callback: (read, chunk) => {
          const take = Math.min(read, READ_CAP_BYTES - bytesRead);
          received.set(chunk.subarray(0, take), bytesRead);
          bytesRead += take;
          if (bytesRead >= expected.length || bytesRead === READ_CAP_BYTES) finish();
          return !done;
        },
      },
    });
    socket.setTimeout(options.timeoutMs ?? WINDOW_SCHEDULE.timeoutMs, finish);
    socket.once("error", (error: NodeJS.ErrnoException) => {
      refused = !connected && error.code === "ECONNREFUSED";
      finish();
    });
    socket.once("close", finish);
    socket.once("connect", () => {
      connected = true;
      socket.setTimeout(options.greetingMs ?? WINDOW_SCHEDULE.greetingMs, finish);
    });
  });
}

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

export async function probeThroughWindow(
  host: string,
  port: number,
  nonce: string,
  windowMs: number,
  schedule = WINDOW_SCHEDULE,
): Promise<ConnectSeen> {
  const start = Date.now();
  let seen: "refused" | "no_answer" = "refused";
  for (let at = schedule.firstAfterMs; at < windowMs; at += schedule.everyMs) {
    const wait = start + at - Date.now();
    if (wait < 0) continue;
    await sleep(wait);
    const timeoutMs = Math.min(schedule.timeoutMs, start + windowMs - Date.now());
    const result = await connectOnce(host, port, nonce, { timeoutMs, greetingMs: schedule.greetingMs });
    if (result.seen === "answered" || result.seen === "other_listener") return result.seen;
    if (result.seen === "no_answer") seen = "no_answer";
  }
  return seen;
}

type Handout = { readonly nonce: string; readonly address: string; readonly handedAt: string };

const probePort = (handout: Handout, request: ProbeRequest, port: number) =>
  Effect.promise(() => probeThroughWindow(probeAddress(handout.address), port, handout.nonce, request.windowMs)).pipe(
    Effect.tap((seen) => Effect.logInfo(`Probe ${handout.address}:${port} ${seen}`)),
    Effect.map((seen): TcpObservation => ({ seen })),
  );

const recordTcp = Effect.fn("Probe.recordTcp")(function* (id: string, handout: Handout, tcp: ProbeResults["tcp"]) {
  const { drizzle } = yield* Database;
  yield* drizzle
    .update(waitingServer)
    .set({ probe: sql`jsonb_set(${waitingServer.probe}, '{tcp}', ${JSON.stringify(tcp)}::jsonb)` })
    .where(and(
      eq(waitingServer.id, id),
      sql`${waitingServer.probe}->'request'->>'nonce' = ${handout.nonce}`,
      sql`${waitingServer.probe}->>'address' = ${handout.address}`,
      sql`${waitingServer.probe}->>'handedAt' = ${handout.handedAt}`,
    ));
});

const inboundProbe = Effect.fn("Probe.inbound")(function* (id: string, handout: Handout, request: ProbeRequest) {
  const tcp = yield* Effect.all(
    { "80": probePort(handout, request, 80), "443": probePort(handout, request, 443) },
    { concurrency: "unbounded" },
  );
  yield* recordTcp(id, handout, tcp);
});

export const decodeStoredProbe = Schema.decodeUnknownOption(ProbeResults);

export const handOutProbe = Effect.fn("Probe.handOut")(function* (
  id: string,
  pending: Option.Option<ProbeResults>,
  observedIp: string,
) {
  if (Option.isNone(pending) || pending.value.handedAt !== undefined) return null;
  const refused = refusedTarget(observedIp);
  const notProbed = refused === null ? null : JSON.stringify({ "80": { seen: "not_probed", reason: refused }, "443": { seen: "not_probed", reason: refused } });
  const { drizzle } = yield* Database;
  const handedOut = sql`${waitingServer.probe} || jsonb_build_object('handedAt', now(), 'address', ${observedIp}::text)`;
  const [handed] = yield* drizzle
    .update(waitingServer)
    .set({ probe: notProbed === null ? handedOut : sql`jsonb_set(${handedOut}, '{tcp}', ${notProbed}::jsonb)` })
    .where(and(eq(waitingServer.id, id), sql`not ${waitingServer.probe} ? 'handedAt'`))
    .returning({ handedAt: sql<string>`${waitingServer.probe}->>'handedAt'` });
  if (handed === undefined) return null;
  const request = pending.value.request;
  if (refused === null) {
    const handout = { nonce: request.nonce, address: observedIp, handedAt: handed.handedAt };
    yield* afterDatabaseCommit(Effect.forkDetach(inboundProbe(id, handout, request).pipe(
      Effect.catchCause((cause) => Effect.logWarning("A waiting server's inbound probe failed.", cause)),
    )));
  }
  return request;
});

const withHostReport = (report: NonNullable<CheckinRequest["probe"]>) => {
  const host = JSON.stringify({ heardFrom: report.heardFrom, bindFailed: report.bindFailed });
  return sql`case when ${waitingServer.probe}->'request'->>'nonce' = ${report.nonce}
    then jsonb_set(${waitingServer.probe}, '{host}', ${host}::jsonb) else ${waitingServer.probe} end`;
};

const PENDING_TCP = JSON.stringify({ "80": { seen: "pending" }, "443": { seen: "pending" } });

export const probeAfterCheckIn = (observedIp: string, report: CheckinRequest["probe"]) => {
  const reported = report === undefined ? sql`${waitingServer.probe}` : withHostReport(report);
  return sql`case when ${waitingServer.probe}->>'address' <> ${observedIp}
    then jsonb_set((${reported}) - 'handedAt' - 'address' - 'host', '{tcp}', ${PENDING_TCP}::jsonb) else ${reported} end`;
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
  const pending: TcpObservation = { seen: "pending" };
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
