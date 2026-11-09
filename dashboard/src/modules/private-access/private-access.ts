import type { AccessChanged, RuntimeWatchView } from "@ployz/sdk";
import { Schema } from "effect";
import { asRecord } from "#/lib/json";

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

export function routersOn(frame: Pick<RuntimeWatchView, "services">): Map<string, RouterState> {
  const routers = frame.services.find((service) => service.identity === TAILSCALE);
  const states = new Map<string, RouterState>();
  for (const container of routers?.containers ?? []) {
    const { state } = container.runtime;
    if (state === "running") states.set(container.machine_id, "running");
    else if (state !== "removing" && state !== "dead" && !states.has(container.machine_id)) states.set(container.machine_id, "stopped");
  }
  return states;
}

export const privateAccessOn = (observed: PrivateAccessObservation) => observed.servers.some((server) => server.router === "running");

export const routerCount = (servers: readonly RouterRow[]) => servers.filter((server) => server.router).length;
