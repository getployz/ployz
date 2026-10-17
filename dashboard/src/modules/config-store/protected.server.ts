import "@tanstack/react-start/server-only";
import type { ConfigCommand } from "@ployz/sdk";
import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import type { OperationAsked } from "#/modules/approvals/approvals.server";
import { gitSourceEnvironment } from "#/modules/config-store/git-evidence.server";
import { cloudStore, storeTry } from "#/modules/config-store/store-sdk.server";
import { member } from "#/modules/identity/tables";
import { refusalOf } from "#/modules/organization/access.server";
import { can } from "#/modules/organization/permissions";
import { Database } from "#/server/database.server";
import { type EnvironmentRef, StoreRefused, type StoreRead, type StoreRefusal } from "./store.contract";

/**
 * Whether user `userId` may protect Environments in `organizationId` and choose a Protected one's repository and deploy
 * branch. Cloud itself (null) may; so may no one outside the Organization's members, support included.
 */
export const mayProtect = Effect.fn("ConfigStore.mayProtect")(function* (organizationId: string, userId: string | null) {
  if (userId === null) return true;
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ role: member.role }).from(member)
    .where(and(eq(member.organizationId, organizationId), eq(member.userId, userId))).limit(1).pipe(Effect.orDie);
  return row !== undefined && can(row.role, "environments:protect");
});

/** The refusal of someone who may not protect, before Cloud files anything for them. */
export const protectorOnly: StoreRefusal = { code: "forbidden", message: refusalOf("environments:protect"), details: null };

/**
 * Refuses `command` unless the caller `may` protect Environments, when it changes an Environment's protection or
 * connects, changes or clears a repository or deploy branch in a Protected one.
 */
export const guardProtected = Effect.fn("ConfigStore.guardProtected")(function* (
  may: boolean,
  command: ConfigCommand,
  read: StoreRead,
) {
  if (may) return;
  if (command.command !== "set_environment_protected") {
    const environment = gitSourceEnvironment(command);
    if (environment === undefined || (yield* listed(environment, read))?.protected !== true) return;
  }
  return yield* new StoreRefused(protectorOnly);
});

/** The Environment `ref` names; one the Store doesn't know is undefined, and the write's own refusal says so. */
const listed = Effect.fn("ConfigStore.listed")(function* (ref: typeof EnvironmentRef.Type, read: StoreRead) {
  const { environments } = yield* storeTry(() => read({ query: "environments", project: ref.project ?? null }));
  return environments.find((environment) => ref.environment == null ? environment.default : environment.name === ref.environment);
});

/**
 * What an agent's `command` waits on when it unprotects a Protected Environment, or null when it doesn't: that leaves the
 * Environment removable and its source open to anyone who can stage, so a human says yes first. The preview names which
 * protection it lifts, so a yes holds for that one: protecting it again asks again. `command` is what to write: an
 * unprotect carries the protection Cloud decided on, so the Store refuses it if another write changed it since.
 */
export const unprotectAsked = Effect.fn("ConfigStore.unprotectAsked")(function* (organizationId: string, command: ConfigCommand) {
  if (command.command !== "set_environment_protected" || command.protected) return { asked: null, command };
  const store = yield* cloudStore;
  // A read the Store refuses asks nothing: the write meets the same refusal.
  const environment = yield* listed(command.environment, (query) => store.read(organizationId, query)).pipe(
    Effect.catchTag("StoreRefused", () => Effect.succeed(undefined)),
  );
  if (environment === undefined) return { asked: null, command };
  const decided = { ...command, seen: { protected: environment.protected, protections: environment.protections } };
  if (!environment.protected) return { asked: null, command: decided };
  const asked = {
    subject: `environment:${environment.id}`,
    verb: "unprotect",
    name: environment.name,
    preview: { project: command.environment.project ?? null, environment: environment.name, protected: false, protection: environment.protections },
    effects: [{ kind: "unprotects_environment", name: environment.name, node: environment.id, path: `${environment.name}.protected` }],
  } satisfies OperationAsked;
  return { asked, command: decided };
});
