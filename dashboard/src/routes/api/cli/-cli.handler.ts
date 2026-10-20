import "@tanstack/react-start/server-only";
import { Effect, Schema } from "effect";
import { Uuid } from "#/lib/schema";
import { ApprovalDecision } from "#/modules/approvals/approvals";
import { decideAndApply } from "#/modules/approvals/apply.server";
import { carriedInto, type Gate, gateOperation } from "#/modules/approvals/approvals.server";
import { disconnectGithub, githubBranches, githubConnection, GithubRepositories } from "#/modules/github/github-cli.server";
import { type Caller, TOKEN_KINDS } from "#/modules/identity/actor";
import { callerOrganizations, resolveCaller } from "#/modules/identity/caller.server";
import { requireClaimed } from "#/modules/identity/claim-required.server";
import { TOKEN_PRESETS, type TokenPreset } from "#/modules/identity/token-grant";
import {
  createOrganizationToken,
  knownEnvironment,
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
import { checkForgetServers, forgetServers, forgetServersAsked } from "#/modules/machines/forget-servers.server";
import { FLEET_TOKEN_LIFETIMES, FleetTokenName } from "#/modules/machines/waiting/fleet-token";
import { createFleetToken, listFleetTokens, revokeFleetToken } from "#/modules/machines/waiting/fleet-token.server";
import {
  cancelServerEnrollment,
  readServerEnrollments,
  type ServerEnrollmentView,
  previewServerEnrollment,
  startServerEnrollment,
} from "#/modules/machines/waiting/enroll.server";
import { openProbeSelection } from "#/modules/machines/waiting/probe.server";
import { rejectWaitingServers } from "#/modules/machines/waiting/reject.server";
import type { EnrollInput } from "#/modules/machines/waiting/server-enrollment";
import { listWaitingServers, showWaitingServer, type WaitingServerView } from "#/modules/machines/waiting/waiting-servers.server";
import { loadAuthorizedMachineRemoveAttempt } from "#/modules/machines/machine-removal.repository";
import type { MachineRemoveAttemptView } from "#/modules/machines/machine-removal";
import { readCliNamespaceCleanup } from "#/modules/machines/namespace-cleanup.server";
import { readCliServerDrain } from "#/modules/machines/server-drain.server";
import { freshApproval, freshOperationDigest, planClean } from "#/modules/machines/server-operations.server";
import { readCliServerUpgrade } from "#/modules/server-upgrade/server-upgrade.server";
import { type Admission, REQUEST_KEY_HEADER, requestKeyOf } from "#/modules/operations/keyed.server";
import { ForgetServer, ForgetServers, NewVolumeRun, RemoveServer, SyncVolume, UpgradeServer } from "#/modules/operations/cli-wire";
import { OPERATIONS } from "#/modules/operations/operations";
import { cleanNamespace, drainServer, forgetServer, removeServer, runVolume, unheld, upgradeServer } from "#/modules/operations/operations.server";
import { removeOrganization } from "#/modules/organization/organization-removal.server";
import { CreateOrganization } from "#/modules/organization/organization-state";
import { createOrganizationForSession } from "#/modules/organization/organization-state.server";
import { slugifySegment } from "#/utils/slug";
import { NotFound, Validation } from "#/server/public-error";
import type { VolumeRunInput } from "#/modules/volume-run/volume-run";
import { getVolumeRun, listVolumeRuns } from "#/modules/volume-run/volume-run.server";
import { refreshMirror } from "#/modules/volume-copies/volume-copies.refresh.server";

const NewToken = Schema.Struct({
  name: Schema.Trim.check(Schema.isMinLength(1), Schema.isMaxLength(64)),
  expires_in_days: Schema.Int.check(Schema.isBetween({ minimum: 1, maximum: 365 })),
  kind: Schema.optional(Schema.Literals(TOKEN_KINDS)),
  preset: Schema.optional(Schema.Literals(TOKEN_PRESETS)),
  environment: Schema.optional(Schema.String.check(Schema.isMaxLength(253))),
});

const presetOf = (input: typeof NewToken.Type): Effect.Effect<TokenPreset, Validation> => {
  const preset = input.preset ?? "full";
  if (preset === "deploy") {
    // Any other name is looked up, so one that isn't a DNS label hears that no Environment has it.
    return input.environment === undefined || input.environment === ""
      ? Effect.fail(new Validation({ message: "A Deploy token needs an Environment; pass `--env`.", userFacing: true }))
      : Effect.succeed({ preset, environment: input.environment });
  }
  return input.environment === undefined
    ? Effect.succeed({ preset })
    : Effect.fail(new Validation({ message: "Only a Deploy token takes an Environment.", userFacing: true }));
};

/** The slug defaults to the name; both then decode as the dashboard's New organization form does. */
const NewOrganization = Schema.Struct({ name: Schema.String, slug: Schema.optional(Schema.String) });
const BAD_ORGANIZATION = "An Organization needs a name of 1 to 64 characters, and a slug of 1 to 64 letters, digits or hyphens.";
const NAME_WITHOUT_SLUG = "This name has no letters or digits to make a slug from. Pass --slug.";

const NewFleetToken = Schema.Struct({
  name: Schema.optional(FleetTokenName),
  expires_in: Schema.Literals(FLEET_TOKEN_LIFETIMES),
  auto_enroll_max: Schema.optional(Schema.Int.check(Schema.isBetween({ minimum: 1, maximum: 1000 }))),
});

const WaitingServerIds = Schema.Struct({ ids: Schema.Array(Uuid).check(Schema.isMinLength(1), Schema.isMaxLength(1000)) });

const EnrollServers = Schema.Struct({
  ids: Schema.Array(Uuid).check(Schema.isMinLength(1), Schema.isMaxLength(1000)),
  first: Schema.optionalKey(Uuid),
  storage: Schema.Union([
    Schema.Struct({ kind: Schema.Literal("zfs"), use_spare_disk: Schema.Boolean, fallback_to_docker: Schema.Boolean }),
    Schema.Struct({ kind: Schema.Literal("none") }),
  ]),
  names: Schema.Union([
    Schema.Struct({ kind: Schema.Literal("hostnames") }),
    Schema.Struct({ kind: Schema.Literal("pattern"), pattern: Schema.Trim.check(Schema.isMinLength(1), Schema.isMaxLength(63)) }),
  ]),
  runs: Schema.Struct({ services: Schema.Boolean, ingress: Schema.Boolean, builds: Schema.Boolean }),
});

function enrollInput(body: typeof EnrollServers.Type): EnrollInput {
  const input: EnrollInput = {
    ids: body.ids,
    storage: body.storage.kind === "zfs"
      ? { kind: "zfs", useSpareDisk: body.storage.use_spare_disk, fallbackToDocker: body.storage.fallback_to_docker }
      : { kind: "none" },
    names: body.names,
    runs: body.runs,
  };
  return body.first === undefined ? input : { ...input, first: body.first };
}

const isUpgradeAttemptId = (id: string | undefined): id is string => id !== undefined && /^[0-9a-f]{32}$/.test(id);

function volumeRunInput(body: typeof NewVolumeRun.Type): VolumeRunInput {
  switch (body.kind) {
    case "mirror":
      return { kind: "mirror", args: { to: body.to } };
    case "sync":
      return { kind: "sync", args: { full: true } };
    case "delete_mirror":
      return { kind: "delete_mirror", args: { slot: body.slot ?? null, confirmed_name: body.confirm ?? null } };
    case "move":
      return { kind: "move", args: { to: body.to } };
    case "release":
      return { kind: "release", args: {} };
    case "restore":
      return { kind: "restore", args: { from: body.from } };
  }
}

// The CLI reads a bare 404 as an unsupported route, so a missing Volume or run answers as a refusal.
const missingRefusal = (message: string) => refusal({ code: "not_found", message, details: null });
const noSuchServer = () => missingRefusal("No such Server.");
/** A token can't make or revoke tokens or Fleet Tokens (see `requireSignedIn`): refused before its request is read, so no field of it is blamed. */
const personOnly = (message: string) => refusal({ code: "forbidden", message, details: { next: "ployz login" } });

const forgetter = (caller: Caller) => ({ userId: caller.userId, credential: caller.credential, organizationId: caller.organization.id });

const approvalHeader = (request: Request) => request.headers.get("x-ployz-approval");

/** Asks a human before the operation runs, unless the request carries the approval they gave. */
const gated = (caller: Caller, request: Request): Gate => (asked) => gateOperation(caller, approvalHeader(request), asked);

const accepted = (id: string) => Response.json({ id }, { status: 202 });

/** The request key and approval an operation call carries. */
const operationCall = (request: Request) => {
  const key = requestKeyOf(request.headers.get(REQUEST_KEY_HEADER));
  return Effect.succeed(key.ok
    ? { ok: true, key: key.key, approvalId: approvalHeader(request) } as const
    : { ok: false, refused: key } as const);
};

const admitted = <A>(admission: Admission | null, reply: (id: string) => A) => {
  if (admission === null) return noSuchServer();
  return admission.ok ? reply(admission.id) : refusal(admission.refusal);
};

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
 * `/api/cli/*`: the `ployz` CLI's account surface (Organizations, making and removing them, Organization Tokens, signed-in devices, Fleet Tokens, Waiting Servers and their Server Enrollments,
 * GitHub connections), and removing, draining, upgrading or cleaning up after a Server Cloud manages through durable runs, or forgetting deleted ones (Forget Servers), and reading or deciding an approval a destructive write waits on. Every call acts as one Caller, bound to one Organization. Replies are snake_case JSON for the CLI.
 */
export const handleCliRequest = Effect.fn("Cli.handle")(function* (request: Request) {
  const caller = yield* resolveCaller(request.headers);
  const path = new URL(request.url).pathname.replace(/^\/api\/cli\//, "");
  const [noun, id, ...rest] = path.split("/");
  const route = `${request.method} ${noun}${id === undefined ? "" : "/:id"}${rest.map((segment) => `/${segment}`).join("")}`;
  switch (route) {
    case "GET organizations":
      return { organizations: yield* callerOrganizations(caller), via: caller.via };
    case "POST organizations": {
      if (caller.credential.kind !== "session") {
        return refusal({
          code: "invalid_argument",
          message: "Only a signed-in person can make an Organization; this credential acts only in its own. No changes made.",
          details: { next: "ployz login" },
        });
      }
      const body = yield* decodeBody(NewOrganization, request, BAD_ORGANIZATION);
      const input = yield* Schema.decodeUnknownEffect(CreateOrganization)({ name: body.name, slug: body.slug ?? body.name }).pipe(
        Effect.catchIf(Schema.isSchemaError, () => Effect.succeed(null)),
      );
      if (input === null) {
        const unsluggable = body.slug === undefined && body.name.trim() !== "" && slugifySegment(body.name) === "";
        return refusal({ code: "invalid_argument", message: unsluggable ? NAME_WITHOUT_SLUG : BAD_ORGANIZATION, details: null });
      }
      return { organization: yield* createOrganizationForSession(caller.userId, caller.credential.id, input) };
    }
    case "DELETE organizations/:id": {
      // A Project refuses the removal before the gate, so an approval it carries is checked first.
      const carried = yield* carriedInto(caller, approvalHeader(request), { command: "remove", subject: `organization:${caller.organization.id}` });
      if (carried !== null) return refusal(carried);
      const removal = yield* removeOrganization(caller, decodeURIComponent(id ?? ""), { gate: gated(caller, request) });
      return removal.ok ? removal.value : refusal(removal.refusal);
    }
    case "GET tokens":
      return {
        ...(yield* listCredentials(caller)),
        revoking: yield* pendingServerRevocations(caller.organization.id),
      };
    case "POST tokens": {
      if (caller.credential.kind !== "session") {
        return yield* requireClaimed(caller.organization.id, "mint_token").pipe(
          Effect.as(personOnly("Only a signed-in person can make tokens. No changes made.")),
          Effect.catchTag("StoreRefused", (refused) => Effect.succeed(refusal(refused.refusal))),
        );
      }
      const input = yield* decodeBody(NewToken, request,
        "A token needs a name of 1 to 64 characters, expires_in_days of 1 to 365, and a kind of cli or agent.");
      const preset = yield* knownEnvironment(caller, yield* presetOf(input));
      return yield* createOrganizationToken(caller, { name: input.name, expiresInDays: input.expires_in_days, kind: input.kind, preset }).pipe(
        Effect.map((token) => ({ token })),
        Effect.catchTag("StoreRefused", (refused) => Effect.succeed(refusal(refused.refusal))),
      );
    }
    case "DELETE tokens/:id": {
      if (caller.credential.kind !== "session") {
        return personOnly("Only a signed-in person can revoke tokens; revoke it under Organization → Tokens. No changes made.");
      }
      if (!Schema.is(Uuid)(id)) return yield* new NotFound({ message: "No such token or signed-in device." });
      const removed = yield* revokeCredential(caller, id).pipe(
        Effect.catchTag("NotFound", (missing) => pendingRevocation(caller, id, missing)),
      );
      return { removed, servers: yield* retireServerAccess(id) };
    }
    case "GET fleet-tokens": {
      const tokens = yield* listFleetTokens(caller, { organizationSlug: caller.organization.slug });
      return {
        fleet_tokens: tokens.map((token) => ({
          id: token.id,
          name: token.name,
          expires_at: token.expiresAt,
          expired: token.expired,
          auto_enroll_max: token.autoEnrollMax,
          last_used_at: token.lastUsedAt,
          created_at: token.createdAt,
        })),
      };
    }
    case "POST fleet-tokens": {
      if (caller.credential.kind !== "session") {
        return personOnly("Only a signed-in person can make Fleet Tokens. No changes made.");
      }
      const input = yield* decodeBody(NewFleetToken, request,
        "A Fleet Token takes an expiry of 1d, 7d or 30d, and an optional name of lowercase letters, digits and hyphens.");
      const created = yield* createFleetToken(caller, {
        organizationSlug: caller.organization.slug,
        name: input.name ?? null,
        expiresIn: input.expires_in,
        autoEnroll: input.auto_enroll_max === undefined ? null : { maxServers: input.auto_enroll_max },
      });
      return { fleet_token: { id: created.id, name: created.name, token: created.token, expires_at: created.expiresAt } };
    }
    case "DELETE fleet-tokens/:id": {
      if (caller.credential.kind !== "session") {
        return personOnly("Only a signed-in person can revoke Fleet Tokens; revoke it under Servers → Tokens. No changes made.");
      }
      const ref = decodeURIComponent(id ?? "");
      const target = Schema.is(Uuid)(ref) ? { id: ref } : { name: ref };
      const revoked = yield* revokeFleetToken(caller, { organizationSlug: caller.organization.slug, ...target });
      return { waiting_removed: revoked.waitingRemoved };
    }
    case "GET waiting-servers":
      return {
        waiting_servers: (yield* listWaitingServers(caller, { organizationSlug: caller.organization.slug })).map(cliWaitingServer),
      };
    case "GET waiting-servers/:id": {
      if (!Schema.is(Uuid)(id)) return yield* new NotFound({ message: "No such waiting server." });
      return { waiting_server: cliWaitingServer(yield* showWaitingServer(caller, { organizationSlug: caller.organization.slug, id })) };
    }
    case "POST reject-servers": {
      const input = yield* decodeBody(WaitingServerIds, request, "Rejecting takes the ids of 1 to 1000 waiting servers.");
      return yield* rejectWaitingServers(caller, { organizationSlug: caller.organization.slug, ids: input.ids });
    }
    case "POST enrollments": {
      const body = yield* decodeBody(EnrollServers, request, "Enrolling takes 1 to 1000 waiting server ids, storage, names and what the servers run.");
      const started = yield* startServerEnrollment(caller, { organizationSlug: caller.organization.slug, ...enrollInput(body) });
      return {
        enrollments: started.attempts.map((attempt) => ({
          id: attempt.id,
          waiting_server_id: attempt.waitingServerId,
          role: attempt.role,
          name: attempt.name,
          state: attempt.state,
        })),
        skipped: started.skipped,
        spread: started.spread,
      };
    }
    case "POST enrollments/:id": {
      if (id !== "plan") return yield* new NotFound({ message: "Not found." });
      const body = yield* decodeBody(EnrollServers, request, "Enrolling takes 1 to 1000 waiting server ids, storage, names and what the servers run.");
      const plan = yield* previewServerEnrollment(caller, { organizationSlug: caller.organization.slug, ...enrollInput(body) });
      return {
        plan: plan.rows.map((row) => ({
          waiting_server_id: row.waitingServerId,
          hostname: row.hostname,
          role: row.role,
          name: row.instruction.name,
          storage: row.instruction.storage,
        })),
        skipped: plan.skipped,
        spread: plan.spread,
      };
    }
    case "GET enrollments":
      return { enrollments: (yield* readServerEnrollments(caller, { organizationSlug: caller.organization.slug })).map(cliEnrollment) };
    case "GET enrollments/:id": {
      if (!Schema.is(Uuid)(id)) return yield* new NotFound({ message: "No such enrollment." });
      const [found] = yield* readServerEnrollments(caller, { organizationSlug: caller.organization.slug, id });
      if (found === undefined) return yield* new NotFound({ message: "No such enrollment." });
      return { enrollment: cliEnrollment(found) };
    }
    case "POST enrollments/:id/cancel": {
      if (!Schema.is(Uuid)(id)) return yield* new NotFound({ message: "No such enrollment." });
      return yield* cancelServerEnrollment(caller, { organizationSlug: caller.organization.slug, id });
    }
    case "POST probe-servers": {
      const input = yield* decodeBody(WaitingServerIds, request, "Probing takes the ids of 1 to 1000 waiting servers.");
      return yield* openProbeSelection(caller, { organizationSlug: caller.organization.slug, ids: input.ids });
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
      const call = yield* operationCall(request);
      if (!call.ok) return refusal(call.refused.refusal);
      return admitted(yield* removeServer(caller, machineId, input.confirm_data_loss.confirmed, call), (id) => ({ id }));
    }
    case "POST servers/:id/forget": {
      const machineId = serverIdOf(id);
      if (machineId === null) return noSuchServer();
      yield* decodeBody(ForgetServer, request, "Forgetting a Server takes no options.");
      const call = yield* operationCall(request);
      if (!call.ok) return refusal(call.refused.refusal);
      return admitted(yield* forgetServer(caller, machineId, call), (id) => ({ id }));
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
      const call = yield* operationCall(request);
      if (!call.ok) return refusal(call.refused.refusal);
      return admitted(yield* drainServer(caller, machineId, call), accepted);
    }
    case "GET server-drains/:id": {
      const drain = Schema.is(Uuid)(id) ? yield* readCliServerDrain(caller.organization.id, id) : null;
      return drain ?? missingRefusal("No such drain.");
    }
    case "POST servers/:id/upgrade": {
      const machineId = serverIdOf(id);
      if (machineId === null) return noSuchServer();
      const input = yield* decodeBody(UpgradeServer, request, "An upgrade takes at most the Release Channel it names.");
      const call = yield* operationCall(request);
      if (!call.ok) return refusal(call.refused.refusal);
      return admitted(yield* upgradeServer(caller, { machineId, channel: input.channel }, call), accepted);
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
      const call = yield* operationCall(request);
      if (!call.ok) return refusal(call.refused.refusal);
      return admitted(yield* cleanNamespace(caller, decodeURIComponent(id ?? ""), call), accepted);
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
      // Nothing to forget skips the gate, so an approval it carries is checked first.
      const carried = yield* carriedInto(caller, approvalHeader(request), { command: "forget", subject: `servers:${caller.organization.id}` });
      if (carried !== null) return refusal(carried);
      const forgotten = yield* forgetServers(forgetter(caller), (checked) => gated(caller, request)(forgetServersAsked(caller.organization, checked)));
      return forgotten.ok ? { organization: caller.organization.slug, ...forgotten.value } : refusal(forgotten.refusal);
    }
    case "POST server-access": {
      const access = yield* provideServerAccess(caller);
      return access.ok ? access.value : refusal(access.refusal);
    }
    case "GET github":
      return yield* requireClaimed(caller.organization.id, "github_connect").pipe(
        Effect.andThen(githubConnection(caller)),
        Effect.catchTag("StoreRefused", (refused) => Effect.succeed(refusal(refused.refusal))),
      );
    case "GET github/:id": {
      const query = new URL(request.url).searchParams;
      const repository = query.get("repository");
      const path = query.get("path");
      const optional = (key: string) => query.get(key) ?? undefined;
      if (repository === null) return yield* new NotFound({ message: "Not found." });
      const github = yield* GithubRepositories;
      const read = Effect.gen(function* () {
        switch (id) {
          case "branches":
            return yield* githubBranches(caller, repository);
          case "tree":
            return yield* github.tree(caller, { repository, path: optional("path"), ref: optional("ref"), match: optional("match") });
          case "file":
            if (path === null) return yield* new NotFound({ message: "Not found." });
            return yield* github.file(caller, { repository, path, ref: optional("ref") });
          default:
            return yield* new NotFound({ message: "Not found." });
        }
      });
      return yield* read.pipe(Effect.catchTag("StoreRefused", (refused) => Effect.succeed(refusal(refused.refusal))));
    }
    case "DELETE github/:id":
      return yield* disconnectGithub(caller, caller.organization.slug, id ?? "");
    case "POST volumes/:id/runs": {
      const body = yield* decodeBody(NewVolumeRun, request, "A volume run takes its environment and what to run.");
      const call = yield* operationCall(request);
      if (!call.ok) return refusal(call.refused.refusal);
      const input = { volumeId: id ?? "", environment: body.environment, ...volumeRunInput(body) };
      return yield* Effect.gen(function* () {
        const run = yield* runVolume(caller, input, call);
        return run.ok ? { run: yield* getVolumeRun(caller.organization.id, run.id) } : refusal(run.refusal);
      }).pipe(Effect.catchTag("NotFound", (missing) => Effect.succeed(missingRefusal(missing.message))));
    }
    case "POST volumes/:id/sync": {
      const body = yield* decodeBody(SyncVolume, request, "A volume sync takes its environment.");
      const unsynced = unheld(caller, OPERATIONS["volume.sync"].requires, body.environment.environment);
      if (unsynced !== null) return refusal(unsynced.refusal);
      // It asks no one, and still refuses an approval carried into it that isn't for exactly this sync.
      const carried = yield* carriedInto(caller, approvalHeader(request), { command: "sync", subject: `volume:${id ?? ""}` });
      if (carried !== null) return refusal(carried);
      return yield* refreshMirror({ userId: caller.userId, organizationId: caller.organization.id }, body.environment, id ?? "").pipe(
        Effect.map((synced) => synced.ok ? { lines: synced.lines } : refusal(synced.refusal)),
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
      return yield* freshApproval(caller.organization.id, id ?? "").pipe(
        Effect.map((approval) => ({ approval })),
        Effect.catchTag("NotFound", (missing) => Effect.succeed(missingRefusal(missing.message))),
      );
    case "POST approvals/:id": {
      const decision = yield* decodeBody(ApprovalDecision, request, "An approval takes `approve` with the digest you reviewed, or `reject`.");
      return yield* decideAndApply(caller, id ?? "", decision, freshOperationDigest).pipe(
        Effect.map((decided) => decided.ok ? { approval: decided.approval } : refusal(decided.refusal)),
        Effect.catchTag("NotFound", (missing) => Effect.succeed(missingRefusal(missing.message))),
      );
    }
    default:
      return yield* new NotFound({ message: "Not found." });
  }
});

function cliWaitingServer(server: WaitingServerView) {
  return {
    id: server.id,
    name: server.report.hostname,
    status: server.status,
    observed_ip: server.observedIp,
    fleet_token: server.fleetToken,
    report: server.report,
    first_seen_at: server.firstSeenAt,
    last_check_in_at: server.lastCheckInAt,
  };
}

function cliEnrollment(enrollment: ServerEnrollmentView) {
  return {
    id: enrollment.id,
    waiting_server_id: enrollment.waitingServerId,
    role: enrollment.role,
    name: enrollment.name,
    storage: enrollment.storage,
    state: enrollment.state,
    stage: enrollment.stage,
    queued: enrollment.queued,
    ahead: enrollment.ahead,
    machine_id: enrollment.machineId,
    failure: enrollment.failure,
    requested_at: enrollment.requestedAt,
    ended_at: enrollment.endedAt,
  };
}

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
