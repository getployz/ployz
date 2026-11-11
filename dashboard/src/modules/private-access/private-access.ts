import type { AccessChanged, ContainerObservation, RuntimeWatchView } from "@ployz/sdk";
import { Schema } from "effect";
import { asRecord } from "#/lib/json";
import { PasskeyAnswerInput } from "#/modules/approval-session/approvals-input";
import { offlineServers } from "#/modules/machines/server-status";

export const TAILSCALE = "ployz-system/tailscale";

export const PRIVATE_ACCESS_WANTS = ["on", "off"] as const;
export type PrivateAccessWant = (typeof PRIVATE_ACCESS_WANTS)[number];

export const TAILSCALE_MACHINES_URL = "https://login.tailscale.com/admin/machines";

const AUTH_KEY = /^tskey-auth-[A-Za-z0-9-]+$/u;

export function authKeyProblem(key: string): string | null {
  return AUTH_KEY.test(key.trim()) ? null : "Keys start with tskey-auth-.";
}

const Slug = Schema.String.check(Schema.isNonEmpty());

/** The auth key as the browser sealed it to the Cluster, with the passkey's approval of exactly that ciphertext when a Server enforces. */
const PrivateAccessOnInput = Schema.Struct({
  sealed_auth_key: Schema.String.check(Schema.isPattern(/^[A-Za-z0-9+/]{1,2048}={0,2}$/)),
  approval: Schema.optional(PasskeyAnswerInput),
});

export const SetPrivateAccessInput = Schema.Struct({ organizationSlug: Slug, to: Schema.Literals(PRIVATE_ACCESS_WANTS), on: Schema.optional(PrivateAccessOnInput) });
export const ApprovePrivateAccessInput = Schema.Struct({ organizationSlug: Slug, id: Schema.String.check(Schema.isUUID()), digest: Schema.NonEmptyString, on: PrivateAccessOnInput });

export type RouterRow = { readonly id: string; readonly name: string; readonly router: boolean };

/** A stopped router was placed but isn't running, such as one Tailscale refused the key for. */
export type RouterState = "running" | "stopped" | "none";

export type ObservedServer = { readonly id: string; readonly name: string; readonly router: RouterState };

export type PrivateAccessObservation = { readonly servers: readonly ObservedServer[]; readonly partial: boolean; readonly enforced: boolean };

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

export function routersOn(frame: RouterFrame): Map<string, RouterState> {
  const states = new Map<string, RouterState>();
  for (const container of routerContainers(frame)) {
    const { state } = container.runtime;
    if (state === "running") states.set(container.machine_id, "running");
    else if (state !== "removing" && state !== "dead" && !states.has(container.machine_id)) states.set(container.machine_id, "stopped");
  }
  return states;
}

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
    enforced: frame.machines.some(({ machine }) => machine.runtime.device_enforcement === true),
  };
}
