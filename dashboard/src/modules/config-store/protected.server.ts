import "@tanstack/react-start/server-only";
import type { ConfigCommand } from "@ployz/sdk";
import { Effect } from "effect";
import type { OperationAsked } from "#/modules/approvals/approvals.server";
import { cloudStore, storeTry } from "#/modules/config-store/store-sdk.server";
import { gitSourceEnvironment } from "#/modules/config-store/git-evidence.server";
import { type Caller, callerEnvironments, callerPermissions } from "#/modules/identity/actor";
import { beyondEnvironments, lacksPermission, refusalOf } from "#/modules/organization/access.server";
import { can, type Permission } from "#/modules/organization/permissions";
import { type EnvironmentRef, StoreRefused, type StoreRead, type StoreRefusal } from "./store.contract";

const notProtector: StoreRefusal = { code: "forbidden", message: refusalOf("environments:protect"), details: null };

/**
 * The refusal of an agent's unprotect of `environment` for a `caller` who doesn't hold `environments:protect`, or whose
 * token's Environments leave it out, or null when they may. The Store would refuse the write too; this refuses before a
 * human is asked to approve it.
 */
export const protectorOnly = (caller: Pick<Caller, "role" | "support" | "credential">, environment: string): StoreRefusal | null => {
  const lacking = lacksPermission(caller, "environments:protect");
  if (lacking !== null) return { code: "forbidden", message: lacking, details: null };
  const environments = callerEnvironments(caller);
  const beyond = beyondEnvironments(environments, { environment });
  return beyond === null ? null : { code: "forbidden", message: beyond, details: { environments: environments ?? [] } };
};

/**
 * What the Store is told `caller` holds for `command`. A token holds its grant, but one whose person can protect ships
 * into a Protected Environment as they would, unasked: only an owner or admin makes a Deploy token for one. Only the
 * writes that ship get that, so the token still can't change protection, or a Protected Environment's source, otherwise.
 */
export const storePermissions = (caller: Pick<Caller, "role" | "support" | "credential">, command: ConfigCommand | null): Permission[] => {
  const held = callerPermissions(caller);
  const ships = command !== null && ["publish", "admit", "sync", "take"].includes(command.command);
  if (!ships || caller.role === null || !can(caller.role, "environments:protect") || !held.includes("changes:deploy")) return held;
  return held.includes("environments:protect") ? held : [...held, "environments:protect"];
};

/**
 * Refuses `command` when it changes an Environment's protection, or connects, changes or clears a repository or deploy
 * branch in a Protected one, and `permissions` lacks `environments:protect`. Cloud itself names none and is never
 * refused. The Store refuses the same write; this refuses before Cloud asks GitHub about it.
 */
export const guardProtected = Effect.fn("ConfigStore.guardProtected")(function* (
  permissions: ReadonlyArray<Permission> | undefined,
  command: ConfigCommand,
  read: StoreRead,
) {
  if (permissions === undefined || permissions.includes("environments:protect")) return;
  if (command.command !== "set_environment_protected") {
    const environment = gitSourceEnvironment(command);
    if (environment === undefined || (yield* listed(environment, read))?.protected !== true) return;
  }
  return yield* new StoreRefused(notProtector);
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
 * unprotect carries the Environment and protection Cloud decided on, so the Store refuses it if its reference now names
 * another Environment or another write changed the protection since. An unprotect of an Environment Cloud can't find
 * is refused here: one created before the write lands was decided on by no one.
 */
export const unprotectAsked = Effect.fn("ConfigStore.unprotectAsked")(function* (organizationId: string, command: ConfigCommand) {
  if (command.command !== "set_environment_protected" || command.protected) return { asked: null, command };
  const store = yield* cloudStore;
  const ref = command.environment;
  const { project, environments } = yield* storeTry(() => store.read(organizationId, { query: "environments", project: ref.project ?? null }));
  const environment = environments.find((candidate) => ref.environment == null ? candidate.default : candidate.name === ref.environment);
  if (environment === undefined) {
    return yield* new StoreRefused({ code: "not_found", message: `No Environment named ${ref.environment ?? "default"}`, details: null });
  }
  const decided = { ...command, seen: { environment: environment.id, protected: environment.protected, protections: environment.protections } };
  if (!environment.protected) return { asked: null, command: decided };
  const asked = {
    subject: `environment:${environment.id}`,
    verb: "unprotect",
    name: `${project.name}/${environment.name}`,
    preview: { project: command.environment.project ?? null, environment: environment.name, protected: false, protection: environment.protections },
    effects: [{ kind: "unprotects_environment", name: environment.name, node: environment.id, path: `${environment.name}.protected` }],
  } satisfies OperationAsked;
  return { asked, command: decided };
});
