import type { AccessChanged, ContainerObservation, RuntimeWatchView } from "@ployz/sdk";
import { Schema } from "effect";
import { asRecord } from "#/lib/json";
import type { Claim, ClaimOutcome } from "#/modules/approvals/approvals";
import { offlineServers } from "#/modules/machines/server-status";

export const TAILSCALE = "ployz-system/tailscale";

export const PRIVATE_ACCESS_WANTS = ["on", "off"] as const;
export type PrivateAccessWant = (typeof PRIVATE_ACCESS_WANTS)[number];

export const TAILSCALE_MACHINES_URL = "https://login.tailscale.com/admin/machines";

const AUTH_KEY = /^tskey-auth-[A-Za-z0-9-]+$/u;

export function authKeyProblem(key: string): string | null {
  return AUTH_KEY.test(key.trim()) ? null : "Keys start with tskey-auth-.";
}

const AuthKey = Schema.String.check(
  Schema.isTrimmed(),
  Schema.isMaxLength(512),
  Schema.makeFilter((key: string) => authKeyProblem(key) === null || "Keys start with tskey-auth-."),
);

const Slug = Schema.String.check(Schema.isNonEmpty());

export const SetPrivateAccessInput = Schema.Struct({ organizationSlug: Slug, to: Schema.Literals(PRIVATE_ACCESS_WANTS), authKey: Schema.optional(AuthKey) });
export const ApprovePrivateAccessInput = Schema.Struct({ organizationSlug: Slug, id: Schema.String.check(Schema.isUUID()), digest: Schema.NonEmptyString, authKey: AuthKey });

export type RouterRow = { readonly id: string; readonly name: string; readonly router: boolean };

/** A stopped router was placed but isn't running, such as one Tailscale refused the key for. */
export type RouterState = "running" | "stopped" | "none";

export type ObservedServer = { readonly id: string; readonly name: string; readonly router: RouterState };

export type PrivateAccessObservation = { readonly servers: readonly ObservedServer[]; readonly partial: boolean };

/** Why a Server's router didn't take: Tailscale refused the key, the router didn't run, or the Server didn't answer. */
export type RouterProblem = "key_refused" | "not_running" | "unreachable";

export type RouterFailure = { readonly id: string; readonly name: string; readonly message: string; readonly problem: RouterProblem };

export type PrivateAccessChanged = { readonly servers: readonly RouterRow[]; readonly failures: readonly RouterFailure[] };

/** The Engine names a router's problem in `details.problem`; a Server that didn't answer names none. */
export function routerProblem(error: AccessChanged["failures"][number]["error"]): RouterProblem {
  const problem = asRecord(error.details)?.["problem"];
  if (problem === undefined) return "unreachable";
  return problem === "key_refused" ? "key_refused" : "not_running";
}

type RouterFrame = Pick<RuntimeWatchView, "services">;

const routerContainers = (frame: RouterFrame): readonly ContainerObservation[] =>
  frame.services.find((service) => service.identity === TAILSCALE)?.containers ?? [];

/** Every router container the frame shows, so a claim tells the routers its key placed from those already there. */
export const routerIds = (frame: RouterFrame) => routerContainers(frame).map((container) => container.container_id);

export function routersOn(frame: RouterFrame): Map<string, RouterState> {
  const states = new Map<string, RouterState>();
  for (const container of routerContainers(frame)) {
    const { state } = container.runtime;
    if (state === "running") states.set(container.machine_id, "running");
    else if (state !== "removing" && state !== "dead" && !states.has(container.machine_id)) states.set(container.machine_id, "stopped");
  }
  return states;
}

export const privateAccessOn = (observed: PrivateAccessObservation) => observed.servers.some((server) => server.router === "running");

export const routerCount = (servers: readonly RouterRow[]) => servers.filter((server) => server.router).length;

type ObservedFrame = Pick<RuntimeWatchView, "services" | "machines" | "incomplete_ids" | "unanswered">;

/** Each Server's router, and whether the frame leaves some Server or container unseen. */
export function privateAccessObservation(frame: ObservedFrame): PrivateAccessObservation {
  const routers = routersOn(frame);
  const offline = offlineServers(frame.machines.map(({ machine, membership }) =>
    ({ id: machine.id, membership, runningBuilds: machine.runtime.running_builds, unanswered: frame.unanswered.includes(machine.id) })));
  return {
    servers: frame.machines.map(({ machine }) => ({ id: machine.id, name: machine.name, router: routers.get(machine.id) ?? "none" })),
    partial: frame.incomplete_ids.machines.length > 0 || frame.incomplete_ids.containers.length > 0 || offline.size > 0,
  };
}

/** A router Tailscale refused keeps restarting for about two minutes before it stays exited. */
export const REFUSED_RESTARTS_MS = 3 * 60_000;

/** How long a key's image pull and router settle may take before a Server that placed no router counts as failed. */
export const CLAIM_WORK_MS = 5 * 60_000;

type ServerAnswer = "took" | "failed" | "settling" | "none";

const answerOn = (machineId: string, placed: readonly ContainerObservation[], now: number): ServerAnswer => {
  const routers = placed.filter((router) => router.machine_id === machineId);
  if (routers.length === 0) return "none";
  if (routers.some((router) => now - router.created_at_unix_nanos / 1e6 < REFUSED_RESTARTS_MS)) return "settling";
  return routers.some((router) => router.runtime.state === "running") ? "took" : "failed";
};

/**
 * What the Servers did with a claim's key, from the routers Cloud sees now. A router counts only when the claim didn't
 * see it before. The key took once every Server runs one for longer than a refused one keeps restarting. It failed once
 * every Server has shown its answer and one stopped, or once the work's bound passed with a Server unseen or showing no
 * router. Until then, unknown: the Servers may still be pulling or settling.
 */
export function claimOutcome(claim: Pick<Claim, "at" | "before">, frame: ObservedFrame | null, now: number): ClaimOutcome {
  const lapsed = now - Date.parse(claim.at) > CLAIM_WORK_MS;
  if (frame === null) return lapsed ? "failed" : "unknown";
  const before = new Set(claim.before);
  const placed = routerContainers(frame).filter((router) =>
    !before.has(router.container_id) && router.runtime.state !== "removing" && router.runtime.state !== "dead");
  const answers = frame.machines.map(({ machine }) => answerOn(machine.id, placed, now));
  const { partial } = privateAccessObservation(frame);
  if (answers.includes("settling")) return "unknown";
  if (partial || answers.length === 0 || answers.includes("none")) return lapsed ? "failed" : "unknown";
  return answers.every((answer) => answer === "took") ? "took" : "failed";
}
