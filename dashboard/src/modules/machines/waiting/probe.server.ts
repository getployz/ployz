import "@tanstack/react-start/server-only";
import { createHmac, randomBytes } from "node:crypto";
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
const GREETING_LENGTH = 22;
const MAX_ADDRESS_MOVES = 2;
const REOPEN_AFTER_SECONDS = PROBE_WINDOW_MS / 1000 + 1;

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

const isLiteralAddress = (ip: string) => isIP(ip) !== 0 && !ip.includes("%");

export function refusedTarget(ip: string, allow: BlockList = devOnlyProbeAllowList()): "no_address" | "private_range" | null {
  if (!isLiteralAddress(ip)) return "no_address";
  const address = ipaddr.process(ip);
  if (allow.check(address.toString(), address.kind() === "ipv4" ? "ipv4" : "ipv6")) return null;
  if (isIP(ip) === 6 && ipv4Compatible.check(ip, "ipv6")) return "private_range";
  return address.range() === "unicast" ? null : "private_range";
}

export const probeGreeting = (handout: string, checkinKey: string, port: number) =>
  createHmac("sha256", handout).update(`ployz-probe-tcp:${checkinKey}:${port}`).digest("base64url").slice(0, GREETING_LENGTH);

export type ConnectSeen = "answered" | "other_listener" | "refused" | "no_answer";

export function connectOnce(
  host: string,
  port: number,
  greeting: string,
  options: { readonly timeoutMs?: number; readonly greetingMs?: number } = {},
) {
  const expected = Buffer.from(greeting);
  return new Promise<{ readonly seen: ConnectSeen; readonly bytesRead: number }>((resolve) => {
    const received = Buffer.alloc(READ_CAP_BYTES);
    let bytesRead = 0;
    let connected = false;
    let refused = false;
    let done = false;
    let greetingTimer: NodeJS.Timeout | undefined;
    const finish = () => {
      if (done) return;
      done = true;
      clearTimeout(deadline);
      clearTimeout(greetingTimer);
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
    socket.once("error", (error: NodeJS.ErrnoException) => {
      refused = !connected && error.code === "ECONNREFUSED";
      finish();
    });
    socket.once("close", finish);
    socket.once("connect", () => {
      connected = true;
      greetingTimer = setTimeout(finish, options.greetingMs ?? WINDOW_SCHEDULE.greetingMs);
    });
    const deadline = setTimeout(finish, options.timeoutMs ?? WINDOW_SCHEDULE.timeoutMs);
  });
}

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

export async function probeThroughWindow(
  host: string,
  port: number,
  greeting: string,
  windowMs: number,
  schedule = WINDOW_SCHEDULE,
): Promise<ConnectSeen> {
  const start = Date.now();
  let refused = false;
  let unanswered = false;
  for (let at = schedule.firstAfterMs; at < windowMs; at += schedule.everyMs) {
    const wait = start + at - Date.now();
    if (wait < 0) continue;
    await sleep(wait);
    const timeoutMs = Math.min(schedule.timeoutMs, start + windowMs - Date.now());
    if (timeoutMs <= 0) break;
    const result = await connectOnce(host, port, greeting, { timeoutMs, greetingMs: schedule.greetingMs });
    if (result.seen === "answered" || result.seen === "other_listener") return result.seen;
    if (result.seen === "refused") refused = true;
    else unanswered = true;
  }
  return refused && !unanswered ? "refused" : "no_answer";
}

type Handout = { readonly handout: string; readonly checkinKey: string; readonly address: string };

const probePort = (handout: Handout, request: ProbeRequest, port: number) =>
  Effect.promise(() =>
    probeThroughWindow(probeAddress(handout.address), port, probeGreeting(handout.handout, handout.checkinKey, port), request.windowMs)
  ).pipe(
    Effect.tap((seen) => Effect.logInfo(`Probe ${handout.address}:${port} ${seen}`)),
    Effect.map((seen): TcpObservation => ({ seen })),
  );

const recordTcp = Effect.fn("Probe.recordTcp")(function* (id: string, handout: Handout, tcp: ProbeResults["tcp"]) {
  const { drizzle } = yield* Database;
  yield* drizzle
    .update(waitingServer)
    .set({ probe: sql`jsonb_set(${waitingServer.probe}, '{tcp}', ${JSON.stringify(tcp)}::jsonb)` })
    .where(and(eq(waitingServer.id, id), sql`${waitingServer.probe}->>'handout' = ${handout.handout}`));
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
  checkinKey: string,
) {
  if (Option.isNone(pending) || pending.value.handedAt !== undefined) return null;
  const refused = refusedTarget(observedIp);
  const notProbed = refused === null ? null : JSON.stringify({ "80": { seen: "not_probed", reason: refused }, "443": { seen: "not_probed", reason: refused } });
  const { drizzle } = yield* Database;
  const handout = randomBytes(16).toString("base64url");
  const handedOut = sql`${waitingServer.probe} || jsonb_build_object('handout', ${handout}::text, 'handedAt', now(), 'address', ${observedIp}::text)`;
  const [handed] = yield* drizzle
    .update(waitingServer)
    .set({ probe: notProbed === null ? handedOut : sql`jsonb_set(${handedOut}, '{tcp}', ${notProbed}::jsonb)` })
    .where(and(eq(waitingServer.id, id), sql`not ${waitingServer.probe} ? 'handedAt'`))
    .returning({ id: waitingServer.id });
  if (handed === undefined) return null;
  const request = pending.value.request;
  if (refused === null) {
    const target = { handout, checkinKey, address: observedIp };
    yield* afterDatabaseCommit(Effect.forkDetach(inboundProbe(id, target, request).pipe(
      Effect.catchCause((cause) => Effect.logWarning("A waiting server's inbound probe failed.", cause)),
    )));
  }
  return { ...request, handout };
});

const withHostReport = (report: NonNullable<CheckinRequest["probe"]>) => {
  const host = JSON.stringify({ heardFrom: report.heardFrom, bindFailed: report.bindFailed });
  return sql`case when ${waitingServer.probe}->>'handout' = ${report.handout}
    then jsonb_set(${waitingServer.probe}, '{host}', ${host}::jsonb) else ${waitingServer.probe} end`;
};

const PENDING_TCP = JSON.stringify({ "80": { seen: "pending" }, "443": { seen: "pending" } });
const UNSTABLE_TCP = JSON.stringify({
  "80": { seen: "not_probed", reason: "address_unstable" },
  "443": { seen: "not_probed", reason: "address_unstable" },
});

export const probeAfterCheckIn = (observedIp: string, report: CheckinRequest["probe"]) => {
  const reported = report === undefined ? sql`${waitingServer.probe}` : withHostReport(report);
  const moves = sql`coalesce((${waitingServer.probe}->>'moves')::int, 0)`;
  return sql`case
    when coalesce(${waitingServer.probe}->>'address' = ${observedIp}, true) then ${reported}
    when ${moves} < ${MAX_ADDRESS_MOVES} then jsonb_set((${reported}) - 'handout' - 'handedAt' - 'address' - 'host', '{tcp}', ${PENDING_TCP}::jsonb)
      || jsonb_build_object('moves', ${moves} + 1)
    else jsonb_set((${reported}) - 'handout' - 'host', '{tcp}', ${UNSTABLE_TCP}::jsonb) end`;
};

export function probePeers(hosts: ReadonlyArray<{ readonly id: string; readonly observedIp: string }>) {
  const allow = devOnlyProbeAllowList();
  return new Map(hosts.map((host, index) => {
    const targets = new Set<string>();
    for (let step = 1; step < hosts.length && targets.size < MAX_PEERS; step++) {
      const peer = hosts[(index + step) % hosts.length];
      if (peer !== undefined && refusedTarget(peer.observedIp, allow) === null && peer.observedIp !== host.observedIp) targets.add(peer.observedIp);
    }
    return [host.id, [...targets].map((target) => ({ target, port: 51820 as const }))];
  }));
}

export const openProbeSelection = Effect.fn("Probe.openSelection")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string; readonly ids: ReadonlyArray<string> },
) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "operate");
  const database = yield* Database;
  const selected = and(
    eq(waitingServer.organizationId, organization.id),
    inArray(waitingServer.id, [...input.ids]),
    isNull(waitingServer.rejectedAt),
  );
  const reopenable = sql<boolean>`coalesce((${waitingServer.probe}->>'handedAt')::timestamptz <= now() - make_interval(secs => ${REOPEN_AFTER_SECONDS}), true)`;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const hosts = yield* drizzle
      .select({ id: waitingServer.id, observedIp: waitingServer.observedIp, reopenable })
      .from(waitingServer)
      .where(selected)
      .orderBy(waitingServer.firstSeenAt, waitingServer.id)
      .for("update");
    if (hosts.length === 0 || hosts.some((host) => !host.reopenable)) return { probing: 0 };
    const nonce = randomBytes(16).toString("base64url");
    const pending: TcpObservation = { seen: "pending" };
    const probes = Object.fromEntries([...probePeers(hosts)].map(([id, send]) => {
      const request: ProbeRequest = { nonce, windowMs: PROBE_WINDOW_MS, listen: ["udp:51820", "tcp:80", "tcp:443"], send };
      return [id, { request, tcp: { "80": pending, "443": pending } } satisfies ProbeResults];
    }));
    const opened = yield* drizzle
      .update(waitingServer)
      .set({ probe: sql`${JSON.stringify(probes)}::jsonb -> (${waitingServer.id}::text)` })
      .where(and(selected, inArray(waitingServer.id, hosts.map((host) => host.id)), reopenable))
      .returning({ id: waitingServer.id });
    return { probing: opened.length };
  }));
});
