import "@tanstack/react-start/server-only";
import { Effect, Schema } from "effect";
import {
  createCustomerPortal,
  createEmbeddedCheckout,
  hasCachedActiveSubscription,
} from "#/modules/billing/billing.server";
import { customDomainsAllowed } from "#/modules/billing/custom-domain-capability";
import { Polar } from "#/modules/billing/polar-provider.server";
import { Uuid } from "#/modules/environment-design/schema";
import { disconnectGithub, githubBranches, githubConnection } from "#/modules/github/github-cli.server";
import type { Caller } from "#/modules/identity/actor";
import { callerOrganizations, resolveCaller } from "#/modules/identity/caller.server";
import {
  createOrganizationToken,
  listCredentials,
  revokeCredential,
} from "#/modules/identity/organization-token.server";
import {
  pendingServerRevocations,
  provideServerAccess,
  retireCredentialServerAccess,
} from "#/modules/machines/server-access.server";
import { refusal } from "#/modules/config-store/config-store.server";
import { removeOrganization } from "#/modules/organization/organization-removal.server";
import { AppConfig } from "#/server/config.server";
import { Conflict, NotFound, Validation } from "#/server/public-error";

const NewToken = Schema.Struct({
  name: Schema.Trim.check(Schema.isMinLength(1), Schema.isMaxLength(64)),
  expires_in_days: Schema.Int.check(Schema.isBetween({ minimum: 1, maximum: 365 })),
});

const decodeBody = <S extends Schema.ConstraintDecoder<unknown>>(schema: S, request: Request, message: string) =>
  Effect.tryPromise({ try: () => request.json(), catch: () => new Validation({ message, userFacing: true }) }).pipe(
    Effect.flatMap(Schema.decodeUnknownEffect(schema)),
    Effect.mapError(() => new Validation({ message, userFacing: true })),
  );

/**
 * `/api/cli/*`: the `ployz` CLI's account surface (Organizations and their removal, Organization Tokens and signed-in devices,
 * GitHub connections, billing). Every call acts as one Caller, bound to one Organization. Replies are snake_case JSON for the CLI.
 */
export const handleCliRequest = Effect.fn("Cli.handle")(function* (request: Request) {
  const config = yield* AppConfig;
  // TODO(#1275): dark until the Config Store cutover.
  if (config.nodeEnv === "production") return yield* new NotFound({ message: "Not found." });
  const caller = yield* resolveCaller(request.headers);
  const path = new URL(request.url).pathname.replace(/^\/api\/cli\//, "");
  const [noun, id, ...rest] = path.split("/");
  const route = `${request.method} ${noun}${id === undefined ? "" : "/:id"}`;
  if (rest.length > 0) return yield* new NotFound({ message: "Not found." });
  switch (route) {
    case "GET organizations":
      return { organizations: yield* callerOrganizations(caller) };
    case "DELETE organizations/:id": {
      const removal = yield* removeOrganization(caller, decodeURIComponent(id ?? ""));
      return removal.ok ? removal.value : refusal(removal.refusal);
    }
    case "GET tokens":
      return {
        ...(yield* listCredentials(caller)),
        revoking: yield* pendingServerRevocations(caller.organization.id),
      };
    case "POST tokens": {
      const input = yield* decodeBody(NewToken, request,
        "A token needs a name of 1 to 64 characters and an expiry of 1 to 365 days.");
      return { token: yield* createOrganizationToken(caller, { name: input.name, expiresInDays: input.expires_in_days }) };
    }
    case "DELETE tokens/:id": {
      if (!Schema.is(Uuid)(id)) return yield* new NotFound({ message: "No such token or signed-in device." });
      const removed = yield* revokeCredential(caller, id).pipe(
        Effect.catchTag("NotFound", (missing) => pendingRevocation(caller, id, missing)),
      );
      return { removed, servers: yield* retireCredentialServerAccess(id) };
    }
    case "POST logout": {
      if (caller.credential.kind !== "session") {
        return yield* new Validation({ message: "An Organization Token isn't signed in; revoke it with `ployz token rm`.", userFacing: true });
      }
      yield* revokeCredential(caller, caller.credential.id);
      return { signed_out: { id: caller.credential.id }, servers: yield* retireCredentialServerAccess(caller.credential.id) };
    }
    case "POST server-access":
      return yield* provideServerAccess(caller);
    case "GET github":
      return yield* githubConnection(caller);
    case "GET github/:id": {
      const repository = new URL(request.url).searchParams.get("repository");
      if (id !== "branches" || repository === null) return yield* new NotFound({ message: "Not found." });
      return yield* githubBranches(caller, repository);
    }
    case "DELETE github/:id": {
      const installation = Number(id);
      if (!Number.isSafeInteger(installation) || installation <= 0) {
        return yield* new NotFound({ message: "No such GitHub installation of yours." });
      }
      return yield* disconnectGithub(caller, installation);
    }
    case "GET billing":
      return { billing: yield* billingSummary(caller) };
    case "POST billing/:id":
      if (id === "checkout") return { url: yield* checkout(caller) };
      if (id === "portal") {
        const portal = yield* createCustomerPortal(caller, { organizationSlug: caller.organization.slug });
        return { url: portal.customerPortalUrl };
      }
      return yield* new NotFound({ message: "Not found." });
    default:
      return yield* new NotFound({ message: "Not found." });
  }
});

/** Rerunning `token rm` on a credential already gone retries the Clears its Servers haven't confirmed. */
const pendingRevocation = Effect.fn("Cli.pendingRevocation")(function* (caller: Caller, id: string, missing: NotFound) {
  const pending = (yield* pendingServerRevocations(caller.organization.id)).find((entry) => entry.id === id);
  if (pending === undefined) return yield* missing;
  return { id, kind: pending.kind };
});

/** The Billing Plan and the capability it grants, from the cached subscription row. */
const billingSummary = Effect.fn("Cli.billingSummary")(function* (caller: Caller) {
  const polar = yield* Polar;
  const selfHosted = polar.mode === "self_hosted";
  return {
    organization: caller.organization.slug,
    self_hosted: selfHosted,
    pro: !selfHosted && (yield* hasCachedActiveSubscription(caller.organization.id)),
    custom_domains: yield* customDomainsAllowed(caller.organization.id),
  };
});

const checkout = Effect.fn("Cli.checkout")(function* (caller: Caller) {
  if (yield* hasCachedActiveSubscription(caller.organization.id)) {
    return yield* new Conflict({ message: "This Organization already holds Pro.", userFacing: true });
  }
  const created = yield* createEmbeddedCheckout(caller, { organizationSlug: caller.organization.slug });
  return created.url;
});
