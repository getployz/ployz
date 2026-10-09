import "@tanstack/react-start/server-only";
import { Effect, Schema } from "effect";
import { Uuid } from "#/lib/schema";
import { ApprovalDecision } from "#/modules/approvals/approvals";
import { decideApproval, gateOperation, getApproval } from "#/modules/approvals/approvals.server";
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
  retireServerAccess,
} from "#/modules/machines/server-access.server";
import { refusal } from "#/modules/config-store/config-store.server";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";
import { checkForgetServers, forgetServers } from "#/modules/machines/forget-servers.server";
import { loadAuthorizedMachineRemoveAttempt, machineRemoveAttemptConsuming } from "#/modules/machines/machine-removal.repository";
import { startMachineRemove } from "#/modules/machines/machine-removal.server";
import type { MachineRemoveAttemptView } from "#/modules/machines/machine-removal";
import { readCliNamespaceCleanup, requestNamespaceCleanup } from "#/modules/machines/namespace-cleanup.server";
import { readCliServerDrain, requestCliServerDrain } from "#/modules/machines/server-drain.server";
import { freshOperationDigest, planClean, planDrain, planRemove } from "#/modules/machines/server-operations.server";
import { readCliServerUpgrade, requestCliServerUpgrade } from "#/modules/server-upgrade/server-upgrade.server";
import { dataLossIdentitySchema } from "#/modules/runtime/data-loss-identity";
import { removeOrganization } from "#/modules/organization/organization-removal.server";
import { NotFound, Validation } from "#/server/public-error";
import type { VolumeRunInput } from "#/modules/volume-run/volume-run";
import { getVolumeRun, listVolumeRuns, requestVolumeRun } from "#/modules/volume-run/volume-run.server";

const NewToken = Schema.Struct({
  name: Schema.Trim.check(Schema.isMinLength(1), Schema.isMaxLength(64)),
  expires_in_days: Schema.Int.check(Schema.isBetween({ minimum: 1, maximum: 365 })),
});

/** `server rm`: reset, with its DataLossConfirmation (the Volumes the user accepted losing), or `--no-reset`. */
const RemoveServer = Schema.Union([
  Schema.Struct({ confirm_data_loss: Schema.Struct({ confirmed: Schema.Array(dataLossIdentitySchema) }) }),
  Schema.Struct({ no_reset: Schema.Literal(true) }),
]);

const UpgradeServer = Schema.Struct({ channel: Schema.optional(Schema.String) });

const isUpgradeAttemptId = (id: string | undefined): id is string => id !== undefined && /^[0-9a-f]{32}$/.test(id);

/** `server forget`: the Organization's slug, as the user typed it. */
const ForgetServers = Schema.Struct({ organization: Schema.String });

const RunEnvironment = Schema.Struct({ project: Schema.String, environment: Schema.String });
const NewVolumeRun = Schema.Union([
  Schema.Struct({ environment: RunEnvironment, kind: Schema.Literal("mirror"), to: Schema.String.check(Schema.isNonEmpty()) }),
  Schema.Struct({ environment: RunEnvironment, kind: Schema.Literal("sync"), full: Schema.optional(Schema.Boolean) }),
  Schema.Struct({
    environment: RunEnvironment,
    kind: Schema.Literal("delete_mirror"),
    slot: Schema.optional(Schema.String.check(Schema.isNonEmpty())),
    confirm: Schema.optional(Schema.String),
  }),
  Schema.Struct({ environment: RunEnvironment, kind: Schema.Literal("move"), to: Schema.String.check(Schema.isNonEmpty()) }),
  Schema.Struct({ environment: RunEnvironment, kind: Schema.Literal("release") }),
]);

function volumeRunInput(body: typeof NewVolumeRun.Type): VolumeRunInput {
  switch (body.kind) {
    case "mirror":
      return { kind: "mirror", args: { to: body.to } };
    case "sync":
      return { kind: "sync", args: { full: body.full ?? false } };
    case "delete_mirror":
      return { kind: "delete_mirror", args: { slot: body.slot ?? null, confirmed_name: body.confirm ?? null } };
    case "move":
      return { kind: "move", args: { to: body.to } };
    case "release":
      return { kind: "release", args: {} };
  }
}

// The CLI reads a bare 404 as an unsupported route, so a missing Volume or run answers as a refusal.
const missingRefusal = (message: string) => refusal({ code: "not_found", message, details: null });
const noSuchServer = () => missingRefusal("No such Server.");

const forgetter = (caller: Caller) => ({ userId: caller.userId, organizationId: caller.organization.id });

const approvalHeader = (request: Request) => request.headers.get("x-ployz-approval");

const accepted = (id: string) => Response.json({ id }, { status: 202 });

const serverIdOf = (id: string | undefined) => {
  const machineId = decodeURIComponent(id ?? "");
  return Schema.is(rustMachineIdSchema)(machineId) ? machineId : null;
};

const decodeBody = <S extends Schema.ConstraintDecoder<unknown>>(schema: S, request: Request, message: string) =>
  Effect.tryPromise({ try: () => request.json(), catch: () => new Validation({ message, userFacing: true }) }).pipe(
    Effect.flatMap(Schema.decodeUnknownEffect(schema)),
    Effect.mapError(() => new Validation({ message, userFacing: true })),
  );

/**
 * `/api/cli/*`: the `ployz` CLI's account surface (Organizations and their removal, Organization Tokens and signed-in devices,
 * GitHub connections), and removing, draining, upgrading or cleaning up after a Server Cloud manages through durable runs, or forgetting deleted ones (Forget Servers), and reading or deciding an approval a destructive write waits on. Every call acts as one Caller, bound to one Organization. Replies are snake_case JSON for the CLI.
 */
export const handleCliRequest = Effect.fn("Cli.handle")(function* (request: Request) {
  const caller = yield* resolveCaller(request.headers);
  const path = new URL(request.url).pathname.replace(/^\/api\/cli\//, "");
  const [noun, id, ...rest] = path.split("/");
  const route = `${request.method} ${noun}${id === undefined ? "" : "/:id"}${rest.map((segment) => `/${segment}`).join("")}`;
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
      return { removed, servers: yield* retireServerAccess(id) };
    }
    case "POST logout": {
      if (caller.credential.kind !== "session") {
        return yield* new Validation({ message: "An Organization Token isn't signed in; revoke it with `ployz token rm`.", userFacing: true });
      }
      yield* revokeCredential(caller, caller.credential.id);
      return { signed_out: { id: caller.credential.id }, servers: yield* retireServerAccess(caller.credential.id) };
    }
    case "DELETE servers/:id": {
      const machineId = serverIdOf(id);
      if (machineId === null) return noSuchServer();
      const input = yield* decodeBody(RemoveServer, request, "Removing a Server takes the Data Loss it confirms.");
      const noReset = "no_reset" in input;
      const approval = approvalHeader(request);
      const consumed = Schema.is(Uuid)(approval)
        ? yield* machineRemoveAttemptConsuming(caller.organization.id, approval, machineId, noReset)
        : null;
      if (consumed !== null) return { id: consumed };
      const plan = yield* planRemove(caller.organization.id, machineId, noReset ? null : input.confirm_data_loss.confirmed);
      if (plan === null) return noSuchServer();
      const gated = yield* gateOperation(caller, approval, plan);
      if (!gated.ok) return refusal(gated.refusal);
      const started = yield* startMachineRemove({
        organizationId: caller.organization.id,
        requestedByUserId: caller.userId,
        machineId,
        approvalId: gated.approvalId,
        ...("no_reset" in input
          ? { confirmDataLoss: [], noReset: true }
          : { confirmDataLoss: [...input.confirm_data_loss.confirmed] }),
      });
      return { id: started.id };
    }
    case "GET server-removals/:id": {
      const attempt = Schema.is(Uuid)(id)
        ? yield* loadAuthorizedMachineRemoveAttempt({ attemptId: id, organizationId: caller.organization.id })
        : null;
      if (attempt === null) return yield* new NotFound({ message: "No such Server removal." });
      return serverRemoval(attempt);
    }
    case "POST servers/:id/drain": {
      const machineId = serverIdOf(id);
      if (machineId === null) return noSuchServer();
      const plan = yield* planDrain(caller.organization.id, machineId);
      if (plan === null) return noSuchServer();
      const gated = yield* gateOperation(caller, approvalHeader(request), plan);
      if (!gated.ok) return refusal(gated.refusal);
      return accepted(yield* requestCliServerDrain(forgetter(caller), { machineId, targets: plan.targets, approvalId: gated.approvalId }));
    }
    case "GET server-drains/:id": {
      const drain = Schema.is(Uuid)(id) ? yield* readCliServerDrain(caller.organization.id, id) : null;
      return drain ?? missingRefusal("No such drain.");
    }
    case "POST servers/:id/upgrade": {
      const machineId = serverIdOf(id);
      if (machineId === null) return noSuchServer();
      const input = yield* decodeBody(UpgradeServer, request, "An upgrade takes at most the Release Channel it names.");
      const requested = yield* requestCliServerUpgrade(forgetter(caller), { machineId, channel: input.channel });
      return requested.ok ? accepted(requested.id) : refusal(requested.refusal);
    }
    case "GET server-upgrades/:id":
      if (!isUpgradeAttemptId(id)) return missingRefusal("No such upgrade.");
      return yield* readCliServerUpgrade(caller.organization.id, id);
    case "GET namespaces/:id/clean": {
      const namespace = decodeURIComponent(id ?? "");
      const plan = yield* planClean(caller.organization.id, namespace);
      return { namespace, volumes: plan.doomed.map(({ identity, label }) => ({ ...identity.id, label })) };
    }
    case "POST namespaces/:id/clean": {
      const namespace = decodeURIComponent(id ?? "");
      const plan = yield* planClean(caller.organization.id, namespace);
      const gated = yield* gateOperation(caller, approvalHeader(request), plan);
      if (!gated.ok) return refusal(gated.refusal);
      return accepted(yield* requestNamespaceCleanup(forgetter(caller), {
        namespace,
        confirmDataLoss: plan.doomed.map(({ identity }) => identity),
        approvalId: gated.approvalId,
      }));
    }
    case "GET namespace-cleanups/:id": {
      const cleanup = Schema.is(Uuid)(id) ? yield* readCliNamespaceCleanup(caller.organization.id, id) : null;
      return cleanup ?? missingRefusal("No such namespace clean.");
    }
    case "GET forget-servers": {
      const checked = yield* checkForgetServers(forgetter(caller));
      return checked.ok ? { organization: caller.organization.slug, ...checked.value } : refusal(checked.refusal);
    }
    case "POST forget-servers": {
      const input = yield* decodeBody(ForgetServers, request, "Forgetting the Servers takes the Organization's slug.");
      if (input.organization !== caller.organization.slug) {
        return refusal({
          code: "invalid_argument",
          message: `This credential acts in Organization ${caller.organization.slug}, not ${input.organization}. No changes made.`,
          details: { next: `ployz org use ${input.organization}` },
        });
      }
      const forgotten = yield* forgetServers(forgetter(caller));
      return forgotten.ok ? { organization: caller.organization.slug, ...forgotten.value } : refusal(forgotten.refusal);
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
    case "POST volumes/:id/runs": {
      const body = yield* decodeBody(NewVolumeRun, request, "A volume run takes its environment and what to run.");
      return yield* requestVolumeRun(forgetter(caller), {
        volumeId: id ?? "",
        environment: body.environment,
        ...volumeRunInput(body),
      }).pipe(
        Effect.map((requested) => requested.ok ? { run: requested.run } : refusal(requested.refusal)),
        Effect.catchTag("NotFound", (missing) => Effect.succeed(missingRefusal(missing.message))),
      );
    }
    case "GET volumes/:id/runs":
      return { runs: yield* listVolumeRuns(caller.organization.id, id ?? "") };
    case "GET volume-runs/:id":
      if (!Schema.is(Uuid)(id)) return missingRefusal("No such volume run.");
      return yield* getVolumeRun(caller.organization.id, id).pipe(
        Effect.map((run) => ({ run })),
        Effect.catchTag("NotFound", (missing) => Effect.succeed(missingRefusal(missing.message))),
      );
    case "GET approvals/:id":
      return yield* getApproval(caller.organization.id, id ?? "", freshOperationDigest).pipe(
        Effect.map((approval) => ({ approval })),
        Effect.catchTag("NotFound", (missing) => Effect.succeed(missingRefusal(missing.message))),
      );
    case "POST approvals/:id": {
      const decision = yield* decodeBody(ApprovalDecision, request, "An approval takes `approve` with the digest you reviewed, or `reject`.");
      return yield* decideApproval(caller, id ?? "", decision, freshOperationDigest).pipe(
        Effect.map((decided) => decided.ok ? { approval: decided.approval } : refusal(decided.refusal)),
        Effect.catchTag("NotFound", (missing) => Effect.succeed(missingRefusal(missing.message))),
      );
    }
    default:
      return yield* new NotFound({ message: "Not found." });
  }
});

/** A Server removal as `ployz server rm` follows it: under way, settled, or refused as it ended. */
function serverRemoval(attempt: MachineRemoveAttemptView) {
  switch (attempt.state) {
    case "pending":
    case "running":
      return { state: attempt.state };
    case "succeeded":
      return { state: attempt.state, reset_warning: attempt.result.resetWarning, release: attempt.result.release };
    case "missing_identities":
      return refusal({
        code: "invalid_argument",
        message: "The Server holds Volumes this removal didn't confirm. No changes made.",
        details: { missing: attempt.missingIdentities },
      });
    case "failed":
    case "cancelled":
      return refusal({ code: "unavailable", message: attempt.failureMessage, details: null });
    default: {
      const _exhaustive: never = attempt;
      throw new Error(`Unhandled machine remove attempt: ${String(_exhaustive)}`);
    }
  }
}

/** Rerunning `token rm` on a credential already gone retries the Clears its Servers haven't confirmed. */
const pendingRevocation = Effect.fn("Cli.pendingRevocation")(function* (caller: Caller, id: string, missing: NotFound) {
  const pending = (yield* pendingServerRevocations(caller.organization.id)).find((entry) => entry.id === id);
  if (pending === undefined) return yield* missing;
  return { id, kind: pending.kind };
});
