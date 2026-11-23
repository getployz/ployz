import "@tanstack/react-start/server-only";
import { pinImages } from "@ployz/sdk";
import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { requireAccess } from "#/modules/organization/access.server";
import { trustedPasskeys } from "#/modules/passkeys/passkeys.server";
import { passkeyRequest } from "#/modules/passkeys/tables";
import { codeOfPasskeyKey } from "#/modules/passkeys/passkey-key";
import { firstRuntimeFrame } from "#/modules/runtime/organization-runtime.server";
import { Database } from "#/server/database.server";

/** What a Server that enforces device approval needs to check a passkey's versions. */
const SIGNS = ["ployz.container.signed-version.proposal.v1", "ployz.authority.scoped-keys.v1"];

/**
 * What a person's Deploy needs before it asks for their passkey: nothing when no Server enforces device approval, else
 * the Cluster's key, the person's passkeys every enforcing Server trusts, and the digest each of `tags` runs as. Any
 * other answer is why it can't ask, in words the sheet shows.
 */
export type DeployTap =
  | { readonly kind: "off" }
  | { readonly kind: "refused"; readonly message: string }
  | {
    readonly kind: "ready";
    readonly cluster: string;
    readonly passkeys: readonly { readonly credentialId: string; readonly code: string }[];
    readonly pins: Readonly<Record<string, string>>;
  };

export const prepareDeployTap = Effect.fn("DeployTap.prepare")(function* (actor: Actor, input: { readonly organizationSlug: string; readonly tags: readonly string[] }) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "changes:deploy");
  const frame = yield* firstRuntimeFrame(organization.id).pipe(Effect.orElseSucceed(() => null));
  const enforcing = (frame?.machines ?? []).map(({ machine }) => machine).filter((machine) => machine.runtime.device_enforcement === true);
  // As the CLI does, a Cluster Cloud can't read signs nothing; a Server that enforces refuses what nobody signed.
  if (enforcing.length === 0) return { kind: "off" } satisfies DeployTap;
  const refused = (message: string): DeployTap => ({ kind: "refused", message });
  // A record that lists no capabilities claims none either way; that Server's own check decides.
  const older = enforcing.find(({ runtime: { capabilities = [] } }) => capabilities.length > 0 && SIGNS.some((needed) => !capabilities.includes(needed)));
  if (older !== undefined) return refused(`Upgrade ${older.name} first. Its ployz can't check your passkey.`);
  const cluster = enforcing.flatMap(({ runtime }) => runtime.sealing?.kind === "held" ? [runtime.sealing.public_key] : [])[0];
  if (cluster === undefined) return refused("Your servers didn't report the cluster's key. Try again.");
  const { drizzle } = yield* Database;
  const own = yield* drizzle.select({ code: passkeyRequest.code, credentialId: passkeyRequest.credentialId }).from(passkeyRequest)
    .where(and(eq(passkeyRequest.organizationId, organization.id), eq(passkeyRequest.userId, actor.userId)));
  const ids = new Set<string>(enforcing.map(({ id }) => id));
  const lists = ((yield* trustedPasskeys(organization.id)) ?? []).filter(({ id }) => id !== null && ids.has(id))
    .flatMap(({ passkeys }) => passkeys === null ? [] : [passkeys]);
  const passkeys = own.filter((row) => lists.length > 0 && lists.every((held) => held.some(({ trusted }) => codeOfPasskeyKey(trusted.passkey.public_key) === row.code)));
  if (passkeys.length === 0) return refused("Your servers don't trust your passkey. Add it under Account, Passkeys, or run ployz deploy.");
  return yield* Effect.tryPromise(() => pinImages([...new Set(input.tags)])).pipe(
    Effect.map((images): DeployTap => ({ kind: "ready", cluster, passkeys, pins: Object.fromEntries(images.map(({ tag, image }) => [tag, image])) })),
    Effect.catch((error) => Effect.succeed(refused(error.cause instanceof Error ? error.cause.message : "Couldn't look up an image's digest. Try again."))),
  );
});
