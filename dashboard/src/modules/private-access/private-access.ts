import type { RuntimeWatchView } from "@ployz/sdk";
import { Schema } from "effect";

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

export type PrivateAccessObservation = { readonly servers: readonly RouterRow[]; readonly partial: boolean };

export type PrivateAccessChanged = {
  readonly servers: readonly RouterRow[];
  readonly failures: readonly { readonly id: string; readonly name: string; readonly message: string }[];
};

export function routersRunningOn(frame: Pick<RuntimeWatchView, "services">): Set<string> {
  const routers = frame.services.find((service) => service.identity === TAILSCALE);
  return new Set(routers?.containers.flatMap((container) => container.runtime.state === "running" ? [container.machine_id] : []));
}

export const privateAccessOn = (observed: PrivateAccessObservation) => observed.servers.some((server) => server.router);

export const routerCount = (servers: readonly RouterRow[]) => servers.filter((server) => server.router).length;
