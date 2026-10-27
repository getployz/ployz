import { FULL_GRANT } from "#/modules/identity/token-grant";
import { it } from "@effect/vitest";
import { type Approval, type ConfigCommand, type ConfigStore, RpcError, type RuntimeWatchView } from "@ployz/sdk";
import { eq, sql } from "drizzle-orm";
import { Deferred, Effect, Fiber } from "effect";
import { expect, vi } from "vitest";
import {
  decideApproval,
  gateOperation,
  getApproval,
  type OperationAsked,
  type OperationDigest,
  operationDigest,
  pendingApprovals,
  readAsker,
  redeemApproval,
  requestApproval,
} from "#/modules/approvals/approvals.server";
import { destroyedLine, type OperationVerb } from "#/modules/approvals/approvals";
import { asTestDouble } from "#/lib/test-double";
import { decideAndApply } from "#/modules/approvals/apply.server";
import { operationApprovals } from "#/modules/approvals/tables";
import { permissionsOf } from "#/modules/organization/permissions";
import { writeStoreAsMember } from "#/modules/config-store/config-store.server";
import { storeTry } from "#/modules/config-store/store-sdk.server";
import { ownedNamespaces } from "#/modules/machines/namespace-cleanup.server";
import { forgetServersAsked } from "#/modules/machines/forget-servers.server";
import { cleanPlan, drainPlan, freshApproval, freshOperationDigest, freshPendingApprovals, removePlan } from "#/modules/machines/server-operations.server";
import { organizationRemovalAsked } from "#/modules/organization/organization-removal.server";
import { OrganizationRuntime, type OrganizationRuntimeService } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSession } from "#/modules/runtime/ployz.server";
import type { Caller } from "#/modules/identity/actor";
import { createOrganizationToken } from "#/modules/identity/organization-token.server";
import { member, user } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";
import { handleConfigRequest } from "#/routes/api/config/-config.handler";
import { AuthLive } from "#/server/auth.server";
import { RunningObserver } from "#/modules/config-store/running-evidence.server";
import { Database } from "#/server/database.server";
import { acmeWeb, enrollStoreServer, seedStoreGitService, seedStoreOrganization, storeTestCloudWith } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000a501";
const OTHER = "00000000-0000-4000-8000-00000000a502";
const PROJECT = "00000000-0000-4000-8000-00000000a503";
const ENVIRONMENT = "00000000-0000-4000-8000-00000000a504";
const SERVICE = "00000000-0000-4000-8000-00000000a505";
const DEPLOYMENT = "00000000-0000-4000-8000-00000000a506";
const UNKNOWN_APPROVAL = "00000000-0000-4000-8000-00000000a5ff";
const here = { project: null, environment: null };
const deploy = (n: number): ConfigCommand => ({
  command: "admit", admit: "deploy", id: `00000000-0000-4000-8000-0000000a52${n.toString().padStart(2, "0")}`,
  environment: here, services: [], version: null, accept_volume_loss: [],
});
const publish: ConfigCommand = { command: "publish", environment: here, version: null, accept_volume_loss: [] };
const saving = { command: "publish" };
const addService = (n: number): ConfigCommand => ({
  command: "create_service", id: `00000000-0000-4000-8000-0000000a51${n.toString().padStart(2, "0")}`, environment: here, name: `cache${n}`, image: "redis:7",
});

/**
 * Shop with `web` deployed (its Applied State written as a finished Deployment would), then removed in the Working
 * State. `refusal` is what the Store answers the CLI's Publish with while a human must approve removing `web`.
 */
const shopRemovingWeb = Effect.fn(function* (removeWeb = true) {
  const services = yield* storeTestCloudWith(AuthLive);
  const provided = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provide(services));
  const userId = yield* provided(seedStoreOrganization(ORGANIZATION));
  const store: ConfigStore = yield* provided(seedStoreGitService(ORGANIZATION, { project: PROJECT, environment: ENVIRONMENT, service: SERVICE }));
  const write = (command: ConfigCommand) => Effect.promise(() => store.write(ORGANIZATION, command));
  yield* Effect.promise(() => store.write(ORGANIZATION, {
    command: "admit", admit: "deploy", id: DEPLOYMENT, environment: here, services: [], version: null, accept_volume_loss: [],
  }, acmeWeb()));
  yield* provided(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`update config_deployment set status = 'applied' where id = ${DEPLOYMENT}`);
    yield* drizzle.execute(sql`insert into config_applied (environment_id, node_id, organization_id, deployment_id, node_type, node)
      select environment_id, service->>'id', organization_id, ${DEPLOYMENT}, 'service', service::text
      from config_saved, lateral jsonb_array_elements(intent::jsonb->'services') service where environment_id = ${ENVIRONMENT}`);
  }));
  if (removeWeb) yield* write({ command: "remove_service", environment: here, service: "web" });
  const withApproval = (approval: Approval) => ({ ...acmeWeb(), approval });
  const refusal = Effect.flip(storeTry(() => store.write(ORGANIZATION, publish, withApproval("required"))))
    .pipe(Effect.map((refused) => refused.refusal));
  const protectedRefusal = write({ command: "set_environment_protected", environment: here, protected: true }).pipe(
    Effect.andThen(Effect.flip(storeTry(() => store.write(ORGANIZATION, publish, { ...withApproval("required"), permissions: permissionsOf("member") })))),
    Effect.map((refused) => refused.refusal));
  const caller: Caller = { userId, organization: { id: ORGANIZATION, slug: "shop" }, credential: { kind: "token", id: "token-1", grant: FULL_GRANT }, role: "owner", support: null, via: "cli" };
  const pending = provided(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const rows = yield* drizzle.select({ id: operationApprovals.id }).from(operationApprovals)
      .where(sql`${operationApprovals.status} = 'pending'`);
    return rows.map((row) => row.id);
  }));
  const { secret } = yield* provided(createOrganizationToken({ ...caller, credential: { kind: "session", id: "session-1" } }, { name: "agent", expiresInDays: 1 }));
  // `running` is what the Servers run, as Cloud asks them before a full Deploy; nothing beyond the Environment by default.
  const cli = (command: ConfigCommand, approval?: string, via?: "agent", bearer = secret, running: string[] = []) => provided(Effect.gen(function* () {
    const headers = new Headers({ "content-type": "application/json", authorization: `Bearer ${bearer}` });
    if (approval !== undefined) headers.set("x-ployz-approval", approval);
    if (via !== undefined) headers.set("x-ployz-via", via);
    const response = yield* handleConfigRequest(new Request("http://localhost:3000/api/config/write", {
      method: "POST", headers, body: JSON.stringify(command),
    })).pipe(Effect.provideService(RunningObserver, { observe: async () => ({ services: running, unanswered: [] }) }));
    return { status: response.status, json: (yield* Effect.promise(() => response.json())) as Reply };
  }));
  return { provided, write, refusal, protectedRefusal, caller, userId, store, pending, withApproval, cli };
});

const anonymous = (userId: string) => Effect.gen(function* () {
  const { drizzle } = yield* Database;
  yield* drizzle.update(user).set({ isAnonymous: true }).where(eq(user.id, userId));
});

const personOwner = Effect.gen(function* () {
  const { drizzle } = yield* Database;
  const [person] = yield* drizzle.insert(user).values({ email: "grace@example.test", name: "Grace" }).returning();
  yield* drizzle.insert(member).values({ userId: person?.id ?? "", organizationId: ORGANIZATION, role: "owner" });
});

/** A member who can't protect, asking from the dashboard. */
const dashboardMember = Effect.gen(function* () {
  const { drizzle } = yield* Database;
  const [person] = yield* drizzle.insert(user).values({ email: "ada.member@example.test", name: "Ada Member" }).returning();
  const userId = person?.id ?? "";
  yield* drizzle.insert(member).values({ userId, organizationId: ORGANIZATION, role: "member" });
  return {
    userId, organization: { id: ORGANIZATION, slug: "shop" }, credential: { kind: "session", id: "ada-session" }, role: "member", support: null, via: "dashboard",
  } satisfies Caller;
});

type Asked = { approval: string; approval_id: string; effects: unknown[] };
const asked = (refused: { details: unknown }) => refused.details as Asked;
type Reply = { written?: string; error?: { code: string; message: string; details: Partial<Asked> | null } };

it.live("an ask for a Deploy that prunes keeps saying so, on its card and to the asker", () =>
  Effect.gen(function* () {
    const { provided, refusal, caller } = yield* shopRemovingWeb();
    const refused = yield* refusal;
    const pruning = { ...refused, details: { ...(refused.details as object), prunes: true, dropped: ["machine-3"] } };
    const answered = yield* provided(requestApproval(caller, publish, pruning, null));
    expect(answered.details).toMatchObject({ prunes: true, dropped: ["machine-3"] });
    const row = yield* provided(getApproval(ORGANIZATION, asked(answered).approval_id));
    expect(row.review).toMatchObject({ prunes: true, dropped: ["machine-3"] });
  }));

it.live("a destructive CLI publish waits for a human to approve exactly what it destroys, once", () =>
  Effect.gen(function* () {
    const { provided, write, refusal, caller, store, withApproval } = yield* shopRemovingWeb();
    expect(yield* provided(redeemApproval(caller, null, saving))).toEqual({ ok: true, approval: "required" });

    const refused = yield* refusal;
    expect(refused.code).toBe("approval_required");
    const first = asked(yield* provided(requestApproval(caller, publish, refused, null)));
    expect(first.effects).toMatchObject([{ kind: "removes_service", name: "web" }]);
    // Asking again while it waits answers the same approval.
    expect(asked(yield* provided(requestApproval(caller, publish, yield* refusal, null))).approval_id).toBe(first.approval_id);
    expect(yield* provided(getApproval(ORGANIZATION, first.approval_id))).toMatchObject({ status: "pending", digest: first.approval });
    expect(yield* provided(redeemApproval(caller, first.approval_id, saving))).toEqual({ ok: true, approval: "required" });

    // Another Organization can't see it, or retry with it.
    yield* provided(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.insert(organization).values({ id: OTHER, name: "Other", slug: "other" });
    }));
    expect(yield* provided(redeemApproval({ ...caller, organization: { id: OTHER, slug: "other" } }, first.approval_id, saving))).toMatchObject({ ok: false, refusal: { code: "not_found" } });
    expect(yield* provided(Effect.flip(getApproval(OTHER, first.approval_id)))).toMatchObject({ _tag: "NotFound" });

    // Approving another digest conflicts; approving the one asked about is what the retry tells the Store.
    expect(yield* provided(decideApproval(caller, first.approval_id, { approve: { digest: `${first.approval}0` } })))
      .toMatchObject({ ok: false, refusal: { code: "conflict" } });
    expect(yield* provided(decideApproval(caller, first.approval_id, { approve: { digest: first.approval } })))
      .toMatchObject({ ok: true, approval: { status: "approved" } });
    expect(yield* provided(decideApproval(caller, first.approval_id, { approve: { digest: first.approval } })))
      .toMatchObject({ ok: true, approval: { status: "approved" } });
    expect(yield* provided(decideApproval(caller, first.approval_id, { reject: {} })))
      .toMatchObject({ ok: false, refusal: { code: "conflict", message: "This approval was already decided elsewhere (approved)." } });
    const trusted = yield* provided(redeemApproval(caller, first.approval_id, saving));
    expect(trusted).toEqual({ ok: true, approval: { approved: first.approval } });
    if (!trusted.ok) return;
    expect(yield* storeTry(() => store.write(ORGANIZATION, publish, withApproval(trusted.approval))))
      .toMatchObject({ written: "published", created: true });

    // The approval covered that Publication only: the next one carrying the removal asks again.
    yield* write(addService(1));
    const again = yield* Effect.flip(storeTry(() => store.write(ORGANIZATION, publish, withApproval(trusted.approval))));
    expect(again.code).toBe("approval_required");
    expect(asked(again).approval).not.toBe(first.approval);
  }));

it.live("an approval the plan moved past is superseded, and a denial answers the agent with its reason", () =>
  Effect.gen(function* () {
    const { provided, write, refusal, caller } = yield* shopRemovingWeb();
    const stale = asked(yield* provided(requestApproval(caller, publish, yield* refusal, null)));

    // Editing the Environment moves its version: the waiting approval no longer names this plan.
    yield* write(addService(1));
    const current = yield* provided(getApproval(ORGANIZATION, stale.approval_id));
    expect(current.status).toBe("superseded");
    expect(yield* provided(decideApproval(caller, stale.approval_id, { approve: { digest: current.digest } })))
      .toMatchObject({ ok: false, refusal: { code: "conflict" } });

    const fresh = asked(yield* provided(requestApproval(caller, publish, yield* refusal, null)));
    expect(fresh.approval_id).not.toBe(stale.approval_id);
    expect(yield* provided(decideApproval(caller, fresh.approval_id, { reject: { reason: "web still serves traffic" } })))
      .toMatchObject({ ok: true, approval: { status: "denied", reason: "web still serves traffic" } });
    expect(yield* provided(redeemApproval(caller, fresh.approval_id, saving))).toMatchObject({
      ok: false,
      refusal: { code: "approval_denied", message: "A human denied the request to save production: web still serves traffic" },
    });
  }));

it.live("an agent answers no approval, and what it asked waits on a person in Cloud or, for `ployz mcp`, at its person's prompt", () =>
  Effect.gen(function* () {
    const { provided, refusal, caller } = yield* shopRemovingWeb();
    const ask = (requester: Caller) => provided(Effect.gen(function* () {
      return asked(yield* requestApproval(requester, publish, yield* refusal, null)).approval_id;
    }));
    const approve = (decider: Parameters<typeof decideApproval>[0], id: string) => provided(Effect.gen(function* () {
      const digest = (yield* getApproval(ORGANIZATION, id)).digest;
      return yield* decideApproval(decider, id, { approve: { digest } });
    }));
    const deny = (decider: Caller, id: string) => provided(decideApproval(decider, id, { reject: {} }));
    const status = (id: string) => provided(Effect.map(getApproval(ORGANIZATION, id), ({ status }) => status));
    const agentRefused = { ok: false, refusal: { code: "forbidden", message: "Agents can't answer approvals." } };
    const refused = {
      ok: false,
      refusal: { code: "forbidden", message: "Only a person in Ployz Cloud, or the prompt that asked, can approve this." },
    };
    const otherToken: Caller = { ...caller, credential: { kind: "token", id: "token-2", grant: FULL_GRANT } };
    const mcp: Caller = { ...caller, credential: { kind: "token", id: "token-mcp", grant: FULL_GRANT }, via: "agent" };
    const grant: Caller = { ...caller, credential: { kind: "oauth", id: "grant-1" }, via: "agent" };
    const sidebar: Caller = { ...caller, credential: { kind: "session", id: "browser" }, via: "agent" };
    const colleague: Caller = { ...otherToken, userId: "00000000-0000-4000-8000-00000000a5aa", role: "member" };

    const fromMcp = yield* ask(mcp);
    for (const agent of [mcp, grant, sidebar]) {
      expect(yield* approve(agent, fromMcp)).toMatchObject(agentRefused);
      expect(yield* deny(agent, fromMcp)).toMatchObject(agentRefused);
    }
    expect(yield* status(fromMcp)).toBe("pending");
    expect(yield* approve(colleague, fromMcp)).toMatchObject(refused);
    expect(yield* approve(otherToken, fromMcp)).toMatchObject({ ok: true, approval: { status: "approved" } });

    const fromGrant = yield* ask(grant);
    expect(yield* approve(caller, fromGrant)).toMatchObject(refused);
    expect(yield* deny(caller, fromGrant)).toMatchObject({ ok: true, approval: { status: "denied" } });

    const fromSidebar = yield* ask(sidebar);
    expect(yield* approve(caller, fromSidebar)).toMatchObject(refused);
    expect(yield* approve({ ...caller, via: "dashboard" }, fromSidebar)).toMatchObject({ ok: true, approval: { status: "approved" } });

    // A person's own `ployz` request still approves from any of their CLI credentials, as the TTY prompt does.
    const fromCli = yield* ask(caller);
    expect(yield* approve(mcp, fromCli)).toMatchObject(agentRefused);
    expect(yield* approve(otherToken, fromCli)).toMatchObject({ ok: true, approval: { status: "approved" } });
  }));

it.live("a delayed refusal for an older plan leaves the current plan's approval pending", () =>
  Effect.gen(function* () {
    const { provided, write, refusal, caller } = yield* shopRemovingWeb();
    const old = yield* refusal;
    yield* write(addService(1));
    const current = asked(yield* provided(requestApproval(caller, publish, yield* refusal, null)));
    const late = asked(yield* provided(requestApproval(caller, publish, old, null)));

    expect(late.approval_id).not.toBe(current.approval_id);
    expect((yield* provided(getApproval(ORGANIZATION, current.approval_id))).status).toBe("pending");
    expect((yield* provided(getApproval(ORGANIZATION, late.approval_id))).status).toBe("superseded");
  }));

it.live("concurrent asks about an older and the current plan leave exactly the current one pending", () =>
  Effect.gen(function* () {
    const { provided, write, refusal, caller, pending } = yield* shopRemovingWeb();
    for (const round of [1, 2, 3, 4, 5]) {
      const old = yield* refusal;
      yield* write(addService(round));
      const fresh = yield* refusal;
      const answers = yield* Effect.all(
        [fresh, old, fresh, old, old, fresh, old].map((refused) => provided(requestApproval(caller, publish, refused, null))),
        { concurrency: "unbounded" },
      );
      const ids = answers.map((answer) => asked(answer).approval_id);
      expect(ids).toEqual(ids.map(() => expect.any(String)));
      const freshIds = new Set([ids[0], ids[2], ids[5]]);
      expect(freshIds.size).toBe(1);
      expect(yield* pending).toEqual([...freshIds]);
    }
  }));

it.live("an ask whose Store read answers late can't supersede an approval recorded meanwhile", () =>
  Effect.gen(function* () {
    const { provided, write, refusal, caller, store, pending } = yield* shopRemovingWeb();
    const old = yield* refusal;
    const read = store.read.bind(store);
    const answered = yield* Deferred.make<void>();
    const delivered = yield* Deferred.make<void>();
    vi.spyOn(store, "read").mockImplementationOnce(async (...args) => {
      const view = await read(...args);
      await Effect.runPromise(Deferred.succeed(answered, undefined).pipe(Effect.andThen(Deferred.await(delivered))));
      return view;
    });
    const slow = yield* Effect.forkChild(provided(requestApproval(caller, publish, old, null)));
    yield* Deferred.await(answered);

    yield* write(addService(1));
    const current = yield* Effect.forkChild(provided(requestApproval(caller, publish, yield* refusal, null)));
    yield* Fiber.join(current).pipe(Effect.timeout("1 second"), Effect.ignore);
    yield* Deferred.succeed(delivered, undefined);
    yield* Fiber.join(slow);
    const live = asked(yield* Fiber.join(current));

    expect(yield* pending).toEqual([live.approval_id]);
  }));

it.live("renaming the Project keeps a waiting approval, which still follows its Environment's plan", () =>
  Effect.gen(function* () {
    const { provided, write, refusal, caller } = yield* shopRemovingWeb();
    const waiting = asked(yield* provided(requestApproval(caller, publish, yield* refusal, null)));

    yield* write({ command: "rename_project", project: "shop", name: "store" });
    expect((yield* provided(getApproval(ORGANIZATION, waiting.approval_id))).status).toBe("pending");

    yield* write(addService(1));
    expect((yield* provided(getApproval(ORGANIZATION, waiting.approval_id))).status).toBe("superseded");
  }));

it.live("the pending list drops an approval the plan moved past and shows the one asked about now", () =>
  Effect.gen(function* () {
    const { provided, write, refusal, caller } = yield* shopRemovingWeb();
    const stale = asked(yield* provided(requestApproval(caller, publish, yield* refusal, null)));
    expect(yield* provided(pendingApprovals(ORGANIZATION))).toMatchObject([{ id: stale.approval_id, status: "pending", digest: stale.approval }]);

    yield* write(addService(1));
    expect(yield* provided(pendingApprovals(ORGANIZATION))).toEqual([]);
    expect((yield* provided(getApproval(ORGANIZATION, stale.approval_id))).status).toBe("superseded");

    const fresh = asked(yield* provided(requestApproval(caller, publish, yield* refusal, null)));
    expect((yield* provided(pendingApprovals(ORGANIZATION))).map((approval) => approval.id)).toEqual([fresh.approval_id]);
  }));

it.live("a Store that can't answer leaves a waiting approval pending", () =>
  Effect.gen(function* () {
    const { provided, refusal, caller, store } = yield* shopRemovingWeb();
    const waiting = asked(yield* provided(requestApproval(caller, publish, yield* refusal, null)));
    vi.spyOn(store, "read").mockRejectedValueOnce(new RpcError({ code: "unavailable", message: "The Store is busy.", details: null }));
    expect((yield* provided(getApproval(ORGANIZATION, waiting.approval_id))).status).toBe("pending");
    expect((yield* provided(getApproval(ORGANIZATION, waiting.approval_id))).status).toBe("pending");
  }));

it.live("nothing turns asking off: no Organization setting is kept, and a call without an approval needs one", () =>
  Effect.gen(function* () {
    const { provided, caller } = yield* shopRemovingWeb();
    const settings = yield* provided(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      return yield* drizzle.execute<{ table: string | null }>(sql`select to_regclass('organization_settings')::text as table`, "objects");
    }));
    expect(settings).toEqual([{ table: null }]);
    expect(yield* provided(redeemApproval(caller, null, saving))).toEqual({ ok: true, approval: "required" });
  }));

it.live("a member's deploy into a Protected Environment opens an Approval, which keeps what it runs to itself", () =>
  Effect.gen(function* () {
    const { provided, protectedRefusal } = yield* shopRemovingWeb();
    const ada = yield* provided(dashboardMember);
    const opened = yield* provided(requestApproval(ada, publish, yield* protectedRefusal, null));
    expect(opened.details).toMatchObject({ protected: true });
    const approval = yield* provided(getApproval(ORGANIZATION, asked(opened).approval_id));
    expect(approval).toMatchObject({ status: "pending", protected: true, requested_by: ada.userId, review: { protected: { environment: "production" } } });
    expect(approval.review).not.toHaveProperty("apply");
  }));

it.live("whoever asked into a Protected Environment can't decide it, either way", () =>
  Effect.gen(function* () {
    const { provided, protectedRefusal } = yield* shopRemovingWeb();
    const ada = yield* provided(dashboardMember);
    const opened = asked(yield* provided(requestApproval(ada, publish, yield* protectedRefusal, null)));
    for (const decision of [{ approve: { digest: opened.approval } }, { reject: {} }]) {
      expect(yield* provided(decideApproval(ada, opened.approval_id, decision)))
        .toMatchObject({ ok: false, refusal: { message: "You asked for this, so someone else who can protect it decides." } });
    }
    expect(yield* provided(getApproval(ORGANIZATION, opened.approval_id))).toMatchObject({ status: "pending" });
  }));

it.live("another member can't decide a protected ask either way; an owner can deny it", () =>
  Effect.gen(function* () {
    const { provided, protectedRefusal, caller } = yield* shopRemovingWeb();
    const ada = yield* provided(dashboardMember);
    const opened = asked(yield* provided(requestApproval(ada, publish, yield* protectedRefusal, null)));
    const bob = yield* provided(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const [person] = yield* drizzle.insert(user).values({ email: "bob.member@example.test", name: "Bob Member" }).returning();
      const userId = person?.id ?? "";
      yield* drizzle.insert(member).values({ userId, organizationId: ORGANIZATION, role: "member" });
      return { ...ada, userId, credential: { kind: "session", id: "bob-session" } } satisfies Caller;
    }));
    for (const decision of [{ approve: { digest: opened.approval } }, { reject: {} }]) {
      expect(yield* provided(decideApproval(bob, opened.approval_id, decision))).toMatchObject({ ok: false, refusal: { code: "forbidden" } });
    }
    expect(yield* provided(getApproval(ORGANIZATION, opened.approval_id))).toMatchObject({ status: "pending" });
    expect(yield* provided(decideApproval(caller, opened.approval_id, { reject: { reason: "not today" } }))).toMatchObject({ ok: true, approval: { status: "denied" } });
  }));

it.live("an admin's approval runs a protected dashboard ask as its asker, with the digest, once", () =>
  Effect.gen(function* () {
    const { provided, protectedRefusal, caller, store } = yield* shopRemovingWeb();
    const ada = yield* provided(dashboardMember);
    const opened = asked(yield* provided(requestApproval(ada, publish, yield* protectedRefusal, null)));
    const write = vi.spyOn(store, "write");
    const approve = { approve: { digest: opened.approval } };
    expect(yield* provided(decideAndApply(caller, opened.approval_id, approve))).toMatchObject({ ok: true, approval: { status: "approved" } });
    expect(write.mock.calls).toHaveLength(1);
    const [, command, trusted, principal] = write.mock.calls[0] ?? expect.fail("nothing written");
    expect(command).toMatchObject({ command: "publish", version: expect.any(String) });
    expect(trusted).toMatchObject({ approval: { approved: opened.approval }, permissions: expect.not.arrayContaining(["environments:protect"]) });
    expect(principal).toBe("Ada Member");
    // Approving again answers the row and runs nothing more.
    expect(yield* provided(decideAndApply(caller, opened.approval_id, approve))).toMatchObject({ ok: true });
    expect(write.mock.calls).toHaveLength(1);
  }));

it.live("an approved dashboard ask that fails to run waits again, unless its plan moved", () =>
  Effect.gen(function* () {
    const { provided, protectedRefusal, caller, store } = yield* shopRemovingWeb();
    const ada = yield* provided(dashboardMember);
    const opened = asked(yield* provided(requestApproval(ada, publish, yield* protectedRefusal, null)));
    const approve = { approve: { digest: opened.approval } };
    const write = vi.spyOn(store, "write").mockRejectedValueOnce(new Error("socket hang up"));
    expect(yield* provided(decideAndApply(caller, opened.approval_id, approve)))
      .toMatchObject({ ok: false, refusal: { message: expect.stringContaining("It's still waiting for approval.") } });
    expect(yield* provided(getApproval(ORGANIZATION, opened.approval_id))).toMatchObject({ status: "pending" });

    write.mockRejectedValueOnce(new RpcError({ code: "conflict", message: "Production moved on.", details: null }));
    expect(yield* provided(decideAndApply(caller, opened.approval_id, approve)))
      .toMatchObject({ ok: false, refusal: { message: expect.stringContaining("it has to be asked again") } });
    expect(yield* provided(getApproval(ORGANIZATION, opened.approval_id))).toMatchObject({ status: "superseded" });
  }));

it.live("each credential's protected ask is its own approval, and only the dashboard's runs what it asked once approved", () =>
  Effect.gen(function* () {
    const { provided, protectedRefusal, cli, caller, store } = yield* shopRemovingWeb();
    const ada = yield* provided(dashboardMember);
    const { secret } = yield* provided(createOrganizationToken(ada, { name: "ada", expiresInDays: 1 }));
    const runs = Effect.fn(function* (id: string) {
      const asker = yield* provided(readAsker(ORGANIZATION, id));
      return asker !== undefined && "apply" in asker.review;
    });
    const guarded = yield* protectedRefusal;

    const fromCli = (yield* cli(publish, undefined, undefined, secret)).json.error?.details?.approval_id ?? expect.fail("no approval_id");
    expect(yield* runs(fromCli)).toBe(false);
    const fromDashboard = asked(yield* provided(requestApproval(ada, publish, guarded, null))).approval_id;
    expect(fromDashboard).not.toBe(fromCli);
    expect(yield* runs(fromDashboard)).toBe(true);
    expect(asked(yield* provided(requestApproval(ada, publish, guarded, null))).approval_id).toBe(fromDashboard);
    expect((yield* cli(publish, undefined, undefined, secret)).json.error?.details?.approval_id).toBe(fromCli);
    expect(yield* runs(fromCli)).toBe(false);

    const write = vi.spyOn(store, "write");
    const digest = asked(guarded).approval;
    expect(yield* provided(decideAndApply(caller, fromDashboard, { approve: { digest } }))).toMatchObject({ ok: true });
    expect(write.mock.calls.map(([, command]) => command.command)).toEqual(["publish"]);
  }));

it.live("an agent's protected sync waits on one card at the version it reviewed, which an owner's approval runs", () =>
  Effect.gen(function* () {
    const { provided, write, store, pending, cli, caller } = yield* shopRemovingWeb(false);
    const production = { project: null, environment: "production" };
    const fixWeb = { project: null, environment: "fix-web" };
    yield* write({ command: "create_branch", id: "00000000-0000-4000-8000-00000000a5f1", from: production, name: "fix-web", copy: ["web"], live: [], setup: [], keep: false });
    yield* write({ command: "edit", environment: fixWeb, expect: null, changes: [{ op: "set", path: "web.env.FLAG", value: "on" }] });
    yield* write({ command: "set_environment_protected", environment: production, protected: true });
    const view = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "sync", from: fixWeb, into: production }));
    const sync: ConfigCommand = { command: "sync", from: fixWeb, into: production, version: view.view === "sync" ? view.version : expect.fail("no sync") };
    const { secret: ada } = yield* provided(createOrganizationToken(yield* provided(dashboardMember), { name: "ada", expiresInDays: 1 }));

    const ids = new Set<string | undefined>();
    let digest = "";
    for (const _ of [1, 2]) {
      const answer = yield* cli(sync, undefined, "agent", ada);
      expect(answer.json.error).toMatchObject({ code: "approval_required", details: { protected: true } });
      ids.add(answer.json.error?.details?.approval_id);
      digest = answer.json.error?.details?.approval ?? expect.fail("no digest");
    }
    expect(ids.size).toBe(1);
    const [id] = [...ids];
    expect(yield* pending).toEqual([id]);
    expect(yield* provided(pendingApprovals(ORGANIZATION))).toMatchObject([{ id, status: "pending" }]);

    yield* provided(decideApproval(caller, id ?? "", { approve: { digest } }));
    expect(yield* cli(sync, id, "agent", ada)).toMatchObject({ status: 200, json: { written: "synced" } });
  }));

it.live("an agent's protected take of one plan is one card, however often it asks", () =>
  Effect.gen(function* () {
    const { protectedRefusal, store, pending, cli } = yield* shopRemovingWeb();
    const guarded = yield* protectedRefusal;
    vi.spyOn(store, "write").mockRejectedValue(new RpcError({ ...guarded, code: "approval_required" }));
    const take: ConfigCommand = { command: "take", from: "staging", into: here, version: "1" };
    const ids = new Set<string | undefined>();
    for (const _ of [1, 2]) {
      const answer = yield* cli(take, undefined, "agent");
      expect(answer.json.error).toMatchObject({ code: "approval_required", details: { protected: true } });
      ids.add(answer.json.error?.details?.approval_id);
    }
    expect(ids.size).toBe(1);
    expect(yield* pending).toHaveLength(1);
  }));

it.live("the dashboard's writes never ask", () =>
  Effect.gen(function* () {
    const { provided, userId, store } = yield* shopRemovingWeb();
    const write = vi.spyOn(store, "write");
    expect(yield* provided(writeStoreAsMember({ userId }, "shop", publish))).toMatchObject({ ok: true });
    expect(write.mock.calls.map(([, , trusted]) => trusted?.approval)).toEqual(["not_required"]);
  }));

it.live("the CLI's Publish asks over HTTPS, and its retry names the approval a human decided", () =>
  Effect.gen(function* () {
    const { provided, caller, cli } = yield* shopRemovingWeb();

    // Even a write that never asks checks the approval it carries.
    expect(yield* cli(addService(1), UNKNOWN_APPROVAL)).toMatchObject({ status: 404, json: { error: { code: "not_found" } } });

    const first = yield* cli(publish);
    expect(first.status).toBe(409);
    expect(first.json.error).toMatchObject({ code: "approval_required", details: { effects: [{ kind: "removes_service", name: "web" }] } });
    const firstId = first.json.error?.details?.approval_id ?? expect.fail("no approval_id");

    const unknown = yield* cli(publish, UNKNOWN_APPROVAL);
    expect(unknown.status).toBe(404);
    expect(unknown.json.error).toMatchObject({ code: "not_found", details: { approval_id: UNKNOWN_APPROVAL } });
    expect(unknown.json.error?.message).not.toContain(UNKNOWN_APPROVAL);

    yield* provided(decideApproval(caller, firstId, { reject: { reason: "not today" } }));
    const denied = yield* cli(publish, firstId);
    expect(denied.status).toBe(403);
    expect(denied.json.error).toMatchObject({ code: "approval_denied", message: "A human denied the request to save production: not today" });
    expect(denied.json.error?.message).not.toContain(firstId);

    // A denial isn't remembered either: asking again opens a new approval.
    const second = yield* cli(publish);
    const secondId = second.json.error?.details?.approval_id ?? expect.fail("no approval_id");
    expect(secondId).not.toBe(firstId);
    const digest = second.json.error?.details?.approval ?? expect.fail("no digest");
    yield* provided(decideApproval(caller, secondId, { approve: { digest } }));
    expect(yield* cli(publish, secondId)).toMatchObject({ status: 200, json: { written: "published" } });
  }));

it.live("`ployz mcp`'s Publish asks as an agent, which its person's `ployz` approves, unlike a request from before Cloud recorded who asked", () =>
  Effect.gen(function* () {
    const { provided, caller, cli } = yield* shopRemovingWeb();
    const otherToken: Caller = { ...caller, credential: { kind: "token", id: "token-2", grant: FULL_GRANT } };
    const approve = (id: string, digest: string) => provided(decideApproval(otherToken, id, { approve: { digest } }));
    const setVia = (id: string, via: string | null) => provided(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.execute(sql`update operation_approvals set via = ${via} where id = ${id}`);
    }));
    const askedBy = Effect.fn(function* (via?: "agent") {
      const details = (yield* cli(publish, undefined, via)).json.error?.details;
      return { id: details?.approval_id ?? expect.fail("no approval_id"), digest: details?.approval ?? expect.fail("no digest") };
    });

    const fromMcp = yield* askedBy("agent");
    yield* setVia(fromMcp.id, null);
    expect(yield* approve(fromMcp.id, fromMcp.digest)).toMatchObject({ ok: false, refusal: { code: "forbidden" } });
    yield* setVia(fromMcp.id, "agent");
    expect(yield* approve(fromMcp.id, fromMcp.digest)).toMatchObject({ ok: true, approval: { status: "approved" } });

    const fromCli = yield* askedBy();
    expect(yield* approve(fromCli.id, fromCli.digest)).toMatchObject({ ok: true, approval: { status: "approved" } });
  }));

it.live("every Deploy `ployz mcp` asks for waits on a human, even one that destroys nothing; the CLI's doesn't", () =>
  Effect.gen(function* () {
    const { provided, caller, cli, store } = yield* shopRemovingWeb(false);
    const write = vi.spyOn(store, "write");
    yield* provided(enrollStoreServer(ORGANIZATION));
    expect(yield* cli(deploy(1))).toMatchObject({ status: 200 });
    expect(yield* cli(addService(1), undefined, "agent")).toMatchObject({ status: 200 });

    const asked = yield* cli(deploy(2), undefined, "agent");
    expect(asked.status).toBe(409);
    expect(asked.json.error).toMatchObject({ code: "approval_required", details: { effects: [] } });
    expect(write.mock.calls.map(([, command, trusted]) => [command.command, trusted?.asks]))
      .toEqual([["admit", "destroys"], ["create_service", "destroys"], ["admit", "every_deploy"]]);

    const id = asked.json.error?.details?.approval_id ?? expect.fail("no approval_id");
    const digest = asked.json.error?.details?.approval ?? expect.fail("no digest");
    const person: Caller = { ...caller, credential: { kind: "session", id: "browser" }, via: "dashboard" };
    expect(yield* provided(decideApproval(person, id, { approve: { digest } }))).toMatchObject({ ok: true });
    expect(yield* cli(deploy(2), id, "agent")).toMatchObject({ status: 200 });

    // The Store binds the version a Deploy reviewed, which moves once it queues: another Deploy under it asks again.
    const again = yield* cli(deploy(3), id, "agent");
    expect(again).toMatchObject({ status: 409, json: { error: { code: "approval_required" } } });
    expect(again.json.error?.details?.approval_id).not.toBe(id);
  }));

it.live("a Deploy token for staging is refused a production Deploy before anyone is asked", () =>
  Effect.gen(function* () {
    const { provided, caller, cli, pending } = yield* shopRemovingWeb(false);
    yield* provided(enrollStoreServer(ORGANIZATION));
    const { secret } = yield* provided(createOrganizationToken({ ...caller, credential: { kind: "session", id: "session-1" } },
      { name: "staging", expiresInDays: 1, preset: { preset: "deploy", environment: "staging" } }));

    const refused = yield* cli(deploy(1), undefined, "agent", secret);
    expect(refused.status).toBe(403);
    expect(refused.json.error?.message).toBe("This token can deploy only to staging.");
    expect(yield* pending).toEqual([]);
  }));

it.live("an owner's Deploy token ships into its Protected Environment unasked but can't unprotect it; a member's token still asks", () =>
  Effect.gen(function* () {
    const { provided, caller, cli, write, pending, store } = yield* shopRemovingWeb(false);
    yield* provided(enrollStoreServer(ORGANIZATION));
    yield* write({ command: "set_environment_protected", environment: here, protected: true });
    const owner = { ...caller, credential: { kind: "session", id: "session-1" } } satisfies Caller;
    const { secret: deployToken } = yield* provided(createOrganizationToken(owner,
      { name: "ci", expiresInDays: 1, preset: { preset: "deploy", environment: "production" } }));
    const sent = vi.spyOn(store, "write");

    // A Deploy token can't prune: refused while something runs the prune would remove, shipped once nothing does.
    const pruning = yield* cli(deploy(1), undefined, undefined, deployToken, ["shop-production/extra2"]);
    expect(pruning).toMatchObject({ status: 403, json: { error: { code: "forbidden" } } });
    expect(pruning.json.error?.message).toContain("extra2");
    expect(yield* cli(deploy(1), undefined, undefined, deployToken, ["shop-production/web"])).toMatchObject({ status: 200 });
    // The Store's fences on protection and source still see the token's own grant.
    expect(sent.mock.calls[0]?.[2]).toMatchObject({ protector: true });
    expect(sent.mock.calls[0]?.[2]?.permissions).not.toContain("environments:protect");
    expect(yield* cli(publish, undefined, undefined, deployToken)).toMatchObject({ status: 200 });
    expect(yield* pending).toEqual([]);
    const unprotect = yield* cli({ command: "set_environment_protected", environment: here, protected: false }, undefined, undefined, deployToken);
    expect(unprotect).toMatchObject({ status: 403, json: { error: { code: "forbidden" } } });

    const { secret: memberToken } = yield* provided(createOrganizationToken(
      { ...yield* provided(dashboardMember), credential: { kind: "session", id: "ada-session" }, via: "cli" }, { name: "ada", expiresInDays: 1 }));
    const asked = yield* cli(deploy(2), undefined, undefined, memberToken);
    expect(asked.json.error).toMatchObject({ code: "approval_required", details: { protected: true } });
  }));

it.live("an anonymous agent's Organization has no person to ask: its Deploys and destroys go through as not required", () =>
  Effect.gen(function* () {
    const { provided, userId, cli, store } = yield* shopRemovingWeb();
    yield* provided(enrollStoreServer(ORGANIZATION));
    yield* provided(anonymous(userId));
    const write = vi.spyOn(store, "write");
    expect(yield* cli(deploy(1), undefined, "agent")).toMatchObject({ status: 200 });
    expect(yield* cli(publish)).toMatchObject({ status: 200 });
    expect(write.mock.calls.map(([, command, trusted]) => [command.command, trusted?.approval]))
      .toEqual([["admit", "not_required"], ["publish", "not_required"]]);
  }));

it.live("an approval from another Organization is no approval here, whether or not this one asks, and nothing saves", () =>
  Effect.gen(function* () {
    const { provided, caller, userId, cli, store } = yield* shopRemovingWeb();
    const details = (yield* cli(publish)).json.error?.details;
    const own = details?.approval_id ?? expect.fail("no approval_id");
    yield* provided(decideApproval(caller, own, { approve: { digest: details?.approval ??expect.fail("no digest") } }));
    const foreign = yield* provided(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.insert(organization).values({ id: OTHER, name: "Other", slug: "other" });
      const [approved] = yield* drizzle.select().from(operationApprovals).where(eq(operationApprovals.id, own));
      const [copied] = yield* drizzle.insert(operationApprovals)
        .values({ ...(approved ?? expect.fail("no approval")), id: crypto.randomUUID(), organizationId: OTHER }).returning();
      return copied?.id ?? expect.fail("no copy");
    }));
    const write = vi.spyOn(store, "write");
    const notFound = { status: 404, json: { error: { code: "not_found", message: "No such approval.", details: { approval_id: foreign } } } };

    expect(yield* cli(publish, foreign)).toMatchObject(notFound);
    yield* provided(anonymous(userId));
    expect(yield* cli(publish, foreign)).toMatchObject(notFound);
    expect(write).not.toHaveBeenCalled();

    expect(yield* cli(publish, own)).toMatchObject({ status: 200, json: { written: "published" } });
  }));

/** Another owner of the Organization, signed in to the dashboard. */
const colleagueOwner = Effect.gen(function* () {
  const { drizzle } = yield* Database;
  const [person] = yield* drizzle.insert(user).values({ email: "grace.owner@example.test", name: "Grace Owner" }).returning();
  const userId = person?.id ?? expect.fail("no colleague");
  yield* drizzle.insert(member).values({ userId, organizationId: ORGANIZATION, role: "owner" });
  return {
    userId, organization: { id: ORGANIZATION, slug: "shop" }, credential: { kind: "session", id: "grace-session" }, role: "owner", support: null, via: "cli",
  } satisfies Caller;
});

const notTheirs = "Someone else asked for this approval, so only they can use it. Run the command again without it to ask for your own.";
const elsewhere = "This approval was asked for from another session or token. Run the command again without it to ask for one from here.";

it.live("an approval runs only for whoever asked, through the token they asked from, and only for the command they asked about", () =>
  Effect.gen(function* () {
    const { provided, caller, cli, store, refusal } = yield* shopRemovingWeb();
    const { secret: laptop } = yield* provided(createOrganizationToken({ ...caller, credential: { kind: "session", id: "session-2" } }, { name: "laptop", expiresInDays: 1 }));
    const grace = yield* provided(colleagueOwner);
    const { secret: graceToken } = yield* provided(createOrganizationToken(grace, { name: "grace", expiresInDays: 1 }));
    const details = (yield* cli(publish)).json.error?.details;
    const id = details?.approval_id ?? expect.fail("no approval_id");
    yield* provided(decideApproval({ ...grace, via: "dashboard" }, id, { approve: { digest: details?.approval ?? expect.fail("no digest") } }));
    const write = vi.spyOn(store, "write");
    const refused = (message: string) => ({ status: 403, json: { error: { code: "forbidden", message, details: { approval_id: id } } } });

    expect(yield* cli(publish, id, undefined, laptop)).toMatchObject(refused(elsewhere));
    expect(yield* cli(publish, id, undefined, graceToken)).toMatchObject(refused(notTheirs));
    const covers = "This approval covers only the request to save production. Run the command again without it to ask for one.";
    expect(yield* cli(deploy(1), id)).toMatchObject(refused(covers));
    expect(write).not.toHaveBeenCalled();

    // The Store names the Environment a write lands in as it asks again, so one carried elsewhere is refused, not asked.
    const token = yield* provided(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const [row] = yield* drizzle.select({ id: operationApprovals.credentialId }).from(operationApprovals).where(eq(operationApprovals.id, id));
      return row?.id ?? expect.fail("no approval");
    }));
    const asker: Caller = { ...caller, credential: { kind: "token", id: token, grant: FULL_GRANT } };
    const { review } = yield* provided(getApproval(ORGANIZATION, id));
    if (!("diff" in review)) return expect.fail("no diff");
    const staging = { ...review.diff, environment: { ...review.diff.environment, id: OTHER, name: "staging" } };
    const intoStaging = { code: "approval_required", message: "asked", details: { approval: details?.approval ?? "", effects: review.effects, diff: staging } };
    expect(yield* provided(requestApproval(asker, publish, intoStaging, id))).toEqual({ code: "forbidden", message: covers, details: { approval_id: id } });
    expect(asked(yield* provided(requestApproval(asker, publish, yield* refusal, id))).approval_id).not.toBe(id);

    expect(yield* cli(publish, id)).toMatchObject({ status: 200, json: { written: "published" } });
  }));

const STAGING = "00000000-0000-4000-8000-00000000a507";

// Catches an approval carried into a write the Store lets through without one, which never compares its target.
it.live("an approval carried into another Environment is refused there, even for a write nobody would be asked about", () =>
  Effect.gen(function* () {
    const { provided, caller, cli, write, store } = yield* shopRemovingWeb();
    yield* write({ command: "create_environment", id: STAGING, project: "shop", name: "staging" });
    const details = (yield* cli(publish)).json.error?.details;
    const id = details?.approval_id ?? expect.fail("no approval_id");
    yield* provided(decideApproval(caller, id, { approve: { digest: details?.approval ?? expect.fail("no digest") } }));
    const writes = vi.spyOn(store, "write");
    const staging: ConfigCommand = { ...publish, environment: { project: "shop", environment: "staging" } };

    expect(yield* cli(staging, id)).toMatchObject({ status: 403, json: { error: {
      code: "forbidden",
      message: "This approval covers only the request to save production. Run the command again without it to ask for one.",
      details: { approval_id: id },
    } } });
    expect(writes).not.toHaveBeenCalled();
    expect(yield* cli(publish, id)).toMatchObject({ status: 200, json: { written: "published" } });
  }));

// Catches an approval carried into a write that is already in the state it asks for, which never reaches the gate.
it.live("an unprotect approval carried into an Environment that is already unprotected, or into a write that never asks, is refused", () =>
  Effect.gen(function* () {
    const { provided, caller, cli, write, store } = yield* shopRemovingWeb();
    yield* write({ command: "create_environment", id: STAGING, project: "shop", name: "staging" });
    yield* write({ command: "set_environment_protected", environment: here, protected: true });
    const unprotect: ConfigCommand = { command: "set_environment_protected", environment: here, protected: false };
    const details = (yield* cli(unprotect)).json.error?.details;
    const id = details?.approval_id ?? expect.fail("no approval_id");
    yield* provided(decideApproval({ ...caller, via: "dashboard" }, id, { approve: { digest: details?.approval ?? expect.fail("no digest") } }));
    const writes = vi.spyOn(store, "write");
    const covers = "This approval covers only the request to unprotect shop/production. Run the command again without it to ask for one.";
    const refused = { status: 403, json: { error: { code: "forbidden", message: covers, details: { approval_id: id } } } };

    expect(yield* cli({ ...unprotect, environment: { project: "shop", environment: "staging" } }, id)).toMatchObject(refused);
    expect(yield* cli({ ...unprotect, protected: true }, id)).toMatchObject(refused);
    expect(yield* cli(addService(2), id)).toMatchObject(refused);
    expect(writes).not.toHaveBeenCalled();
    expect(yield* cli(unprotect, id)).toMatchObject({ status: 200 });
  }));

const removeShop =(version: string | null = null): ConfigCommand => ({ command: "remove", project: "shop", version });
/** The version a `confirmation_required` refusal hands back, the one a removal names to go ahead. */
const confirmed = (reply: { json: Reply }) => (reply.json.error?.details as { version?: string } | null)?.version ?? expect.fail("no version");
/** `web` ran on the Servers, so removing Shop stops it there. */
const ran = Effect.gen(function* () {
  const { drizzle } = yield* Database;
  yield* drizzle.execute(sql`update config_deployment set started = 1 where id = ${DEPLOYMENT}`);
});
const marks = Effect.gen(function* () {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.execute<{ marks: number }>(sql`select count(*)::int as marks from config_removal_mark`, "objects");
  return rows[0]?.marks;
});

for (const [who, via] of [["an agent's", "agent"], ["the CLI's", undefined]] as const) {
  it.live(`${who} Project removal waits on a human to approve it, and removes nothing until then`, () =>
    Effect.gen(function* () {
      const { provided, caller, cli } = yield* shopRemovingWeb(false);
      yield* provided(enrollStoreServer(ORGANIZATION));
      yield* provided(ran);

      const version = confirmed(yield* cli(removeShop(), undefined, via));
      const asked = yield* cli(removeShop(version), undefined, via);
      expect(asked.status).toBe(409);
      expect(asked.json.error).toMatchObject({
        code: "approval_required",
        details: { approval_id: expect.any(String), operation: { verb: "remove", name: "shop" }, organization: "shop" },
      });
      expect(yield* provided(marks)).toBe(0);

      const id = asked.json.error?.details?.approval_id ?? expect.fail("no approval_id");
      const digest = asked.json.error?.details?.approval ?? expect.fail("no digest");
      const person: Caller = { ...caller, credential: { kind: "session", id: "browser" }, via: "dashboard" };
      expect(yield* provided(decideApproval(person, id, { approve: { digest } }))).toMatchObject({ ok: true });
      expect(yield* cli(removeShop(version), id, via)).toMatchObject({ status: 200, json: { written: "removal" } });
      expect(yield* provided(marks)).toBe(1);
    }));
}

it.live("an anonymous agent's Project removal goes through without asking", () =>
  Effect.gen(function* () {
    const { provided, userId, cli } = yield* shopRemovingWeb(false);
    yield* provided(enrollStoreServer(ORGANIZATION));
    yield* provided(anonymous(userId));
    yield* provided(ran);
    const version = confirmed(yield* cli(removeShop(), undefined, "agent"));
    expect(yield* cli(removeShop(version), undefined, "agent")).toMatchObject({ status: 200, json: { written: "removal" } });
    expect(yield* provided(marks)).toBe(1);
  }));

it.live("a person's Project removal from the dashboard goes through without asking", () =>
  Effect.gen(function* () {
    const { provided, userId, cli } = yield* shopRemovingWeb(false);
    yield* provided(enrollStoreServer(ORGANIZATION));
    yield* provided(ran);
    const version = confirmed(yield* cli(removeShop()));
    expect(yield* provided(writeStoreAsMember({ userId }, "shop", removeShop(version)))).toMatchObject({ ok: true });
    expect(yield* provided(marks)).toBe(1);
  }));

it.live("a retry `ployz mcp` asks for waits on a human as a Retry; the CLI's doesn't", () =>
  Effect.gen(function* () {
    const { provided, cli, store } = yield* shopRemovingWeb(false);
    const write = vi.spyOn(store, "write");
    yield* provided(enrollStoreServer(ORGANIZATION));
    yield* provided(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.execute(sql`update config_deployment set status = 'failed' where id = ${DEPLOYMENT}`);
    }));
    const retry = (n: number): ConfigCommand => ({
      command: "admit", admit: "retry", id: `00000000-0000-4000-8000-0000000a53${n.toString().padStart(2, "0")}`, deployment: DEPLOYMENT,
    });

    const asked = yield* cli(retry(1), undefined, "agent");
    expect(asked.status).toBe(409);
    expect(asked.json.error).toMatchObject({ code: "approval_required", details: { effects: [] } });
    const id = asked.json.error?.details?.approval_id ?? expect.fail("no approval_id");
    expect(yield* provided(getApproval(ORGANIZATION, id))).toMatchObject({ command: "retry", status: "pending" });

    expect(yield* cli(retry(2))).toMatchObject({ status: 200 });
    expect(write.mock.calls.map(([, command, trusted]) => [command.command, trusted?.asks]))
      .toEqual([["admit", "every_deploy"], ["admit", "destroys"]]);
  }));

it.live("a member's CLI retry into a Protected Environment runs once an owner approves it", () =>
  Effect.gen(function* () {
    const { provided, cli, write, caller } = yield* shopRemovingWeb(false);
    yield* provided(enrollStoreServer(ORGANIZATION));
    yield* provided(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.execute(sql`update config_deployment set status = 'failed' where id = ${DEPLOYMENT}`);
    }));
    yield* write({ command: "set_environment_protected", environment: here, protected: true });
    const { secret: ada } = yield* provided(createOrganizationToken(yield* provided(dashboardMember), { name: "ada", expiresInDays: 1 }));
    const retry: ConfigCommand = { command: "admit", admit: "retry", id: "00000000-0000-4000-8000-0000000a5401", deployment: DEPLOYMENT };

    const asked = yield* cli(retry, undefined, undefined, ada);
    expect(asked.json.error).toMatchObject({ code: "approval_required", details: { protected: true } });
    const id = asked.json.error?.details?.approval_id ?? expect.fail("no approval_id");
    yield* provided(decideApproval(caller, id, { approve: { digest: asked.json.error?.details?.approval ?? "" } }));
    expect(yield* cli(retry, id, undefined, ada)).toMatchObject({ status: 200 });
  }));

type Shop = Effect.Success<ReturnType<typeof shopRemovingWeb>>;
const askedOverHttps = Effect.fn(function* ({ cli }: Shop) {
  const reply = yield* cli(publish);
  expect(reply.json.error?.code).toBe("approval_required");
  const details = reply.json.error?.details;
  return { id: details?.approval_id ?? expect.fail("no approval_id"), digest: details?.approval ?? expect.fail("no digest") };
});

const recorded = [
  {
    state: "a denied approval",
    leave: Effect.fn(function* (shop: Shop) {
      const { id } = yield* askedOverHttps(shop);
      yield* shop.provided(decideApproval(shop.caller, id, { reject: { reason: "web still serves traffic" } }));
      return id;
    }),
    reachesStore: false,
    answers: () => ({ status: 403, json: { error: { code: "approval_denied", message: "A human denied the request to save production: web still serves traffic" } } }),
  },
  {
    state: "a superseded approval",
    leave: Effect.fn(function* (shop: Shop) {
      const { id } = yield* askedOverHttps(shop);
      yield* shop.write(addService(1));
      expect((yield* shop.provided(getApproval(ORGANIZATION, id))).status).toBe("superseded");
      return id;
    }),
    reachesStore: true,
    answers: () => ({ status: 409, json: { error: { code: "approval_required" } } }),
  },
  {
    state: "a pending approval",
    leave: Effect.fn(function* (shop: Shop) {
      return (yield* askedOverHttps(shop)).id;
    }),
    reachesStore: true,
    answers: (id: string) => ({ status: 409, json: { error: { code: "approval_required", details: { approval_id: id } } } }),
  },
  {
    state: "an approval of a plan that moved since",
    leave: Effect.fn(function* (shop: Shop) {
      const { id, digest } = yield* askedOverHttps(shop);
      yield* shop.provided(decideApproval(shop.caller, id, { approve: { digest } }));
      yield* shop.write(addService(1));
      return id;
    }),
    reachesStore: true,
    answers: () => ({ status: 409, json: { error: { code: "approval_required" } } }),
  },
  {
    state: "an approval of the current plan",
    leave: Effect.fn(function* (shop: Shop) {
      const { id, digest } = yield* askedOverHttps(shop);
      yield* shop.provided(decideApproval(shop.caller, id, { approve: { digest } }));
      return id;
    }),
    reachesStore: true,
    answers: () => ({ status: 200, json: { written: "published" } }),
  },
];

for (const { state, leave, reachesStore, answers } of recorded) {
  it.live(`a retry naming ${state} answers the same`, () =>
    Effect.gen(function* () {
      const shop = yield* shopRemovingWeb();
      const id = yield* leave(shop);
      const writes = vi.spyOn(shop.store, "write");
      expect(yield* shop.cli(publish, id)).toMatchObject(answers(id));
      expect(writes.mock.calls.length > 0).toBe(reachesStore);
    }));
}

const MACHINE = "a".repeat(32);
const drainOf = (moves: string[]): OperationAsked => ({
  subject: `server:${MACHINE}`,
  verb: "drain",
  name: "fra-1",
  preview: { server: MACHINE, moves, stays: [], retires: ["shop.edge"] },
  effects: [{ kind: "removes_service", name: "shop.edge", node: "edge", path: "services/shop.edge" }],
});
const answers = (digest: string | null): OperationDigest<never> => () => Effect.succeed(digest);

const operationCloud = Effect.fn(function* () {
  const services = yield* storeTestCloudWith(AuthLive);
  const provided = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provide(services));
  const userId = yield* provided(seedStoreOrganization(ORGANIZATION));
  const caller: Caller = { userId, organization: { id: ORGANIZATION, slug: "shop" }, credential: { kind: "token", id: "token-1", grant: FULL_GRANT }, role: "owner", support: null, via: "cli" };
  const rows = provided(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    return yield* drizzle.select({ id: operationApprovals.id, status: operationApprovals.status, subject: operationApprovals.subject })
      .from(operationApprovals);
  }));
  const gate = (asked: OperationAsked, approvalId: string | null = null) => provided(gateOperation(caller, approvalId, asked));
  return { provided, caller, userId, rows, gate };
});

const refusedWith = (gated: { ok: true } | { ok: false; refusal: { code: string; details: unknown } }) => {
  if (gated.ok) return expect.fail("the operation ran without asking");
  expect(gated.refusal.code).toBe("approval_required");
  return gated.refusal.details as Asked & { operation: { verb: string; name: string } };
};

it("an operation digest ignores key order and changes with the preview or with what it destroys", () => {
  const asked = drainOf(["shop.web"]);
  expect(operationDigest({ ...asked, preview: { b: [1, 2], a: { y: 1, x: 2 } } }))
    .toBe(operationDigest({ ...asked, preview: { a: { x: 2, y: 1 }, b: [1, 2] } }));
  expect(operationDigest(drainOf(["shop.api"]))).not.toBe(operationDigest(asked));
  const metrics = { kind: "removes_service", name: "shop.metrics", node: "metrics", path: "services/shop.metrics" } as const;
  expect(operationDigest({ ...asked, effects: [...asked.effects, metrics] })).not.toBe(operationDigest(asked));
  expect(operationDigest({ ...asked, verb: "clean" })).not.toBe(operationDigest(asked));
  expect(operationDigest({ ...asked, subject: `server:${"b".repeat(32)}` })).not.toBe(operationDigest(asked));
  expect(operationDigest(asked)).toMatch(/^drain:[0-9a-f]{64}$/);
});

it.live("an operation approval runs only for whoever asked, through the token they asked from, and only on the target they asked about", () =>
  Effect.gen(function* () {
    const { provided, caller, gate } = yield* operationCloud();
    const grace = yield* provided(colleagueOwner);
    const laptop: Caller = { ...caller, credential: { kind: "token", id: "token-2", grant: FULL_GRANT } };
    const asked = drainOf(["shop.web"]);
    const first = refusedWith(yield* gate(asked));
    // Anyone else asking for the same opens an approval of their own, and leaves this one theirs.
    expect(refusedWith(yield* provided(gateOperation(grace, null, asked))).approval_id).not.toBe(first.approval_id);
    expect(refusedWith(yield* provided(gateOperation(laptop, null, asked))).approval_id).not.toBe(first.approval_id);
    yield* provided(decideApproval({ ...grace, via: "dashboard" }, first.approval_id, { approve: { digest: first.approval } }, answers(first.approval)));
    const refused = (message: string) => ({ ok: false, refusal: { code: "forbidden", message, details: { approval_id: first.approval_id } } });

    expect(yield* provided(gateOperation(laptop, first.approval_id, asked))).toEqual(refused(elsewhere));
    expect(yield* provided(gateOperation(grace, first.approval_id, asked))).toEqual(refused(notTheirs));
    const fra2 = { ...asked, subject: `server:${"b".repeat(32)}`, name: "fra-2" };
    expect(yield* gate(fra2, first.approval_id)).toEqual(refused(
      "This approval covers only the request to drain fra-1. Run the command again without it to ask for one.",
    ));
    expect(yield* gate({ ...asked, verb: "clean" }, first.approval_id)).toEqual(refused(
      "This approval covers only the request to drain fra-1. Run the command again without it to ask for one.",
    ));

    expect(yield* gate(asked, first.approval_id)).toEqual({ ok: true, approvalId: first.approval_id });
  }));

const VERBS: readonly OperationVerb[] = ["drain", "set", "clean", "remove", "forget", "release", "remove_mirror", "stop", "restart", "unprotect"];

// Catches an approval that, once run, runs the same operation again: a demotion repeated after the person was restored.
for (const verb of VERBS) {
  it.live(`a ${verb} approval runs one operation, and using it again asks again`, () =>
    Effect.gen(function* () {
      const { provided, caller, gate } = yield* operationCloud();
      const asked = { ...drainOf(["shop.web"]), verb };
      const first = refusedWith(yield* gate(asked));
      yield* provided(decideApproval(caller, first.approval_id, { approve: { digest: first.approval } }, answers(first.approval)));
      expect(yield* gate(asked, first.approval_id)).toEqual({ ok: true, approvalId: first.approval_id });

      const again = refusedWith(yield* gate(asked, first.approval_id));
      expect(again.approval_id).not.toBe(first.approval_id);
      expect((yield* provided(getApproval(ORGANIZATION, first.approval_id))).status).toBe("approved");
    }));
}

it.live("two operations racing on one approval run one of them; the other asks again", () =>
  Effect.gen(function* () {
    const { provided, caller, gate } = yield* operationCloud();
    const asked = drainOf(["shop.web"]);
    const first = refusedWith(yield* gate(asked));
    yield* provided(decideApproval(caller, first.approval_id, { approve: { digest: first.approval } }, answers(first.approval)));
    const raced = yield* Effect.all([gate(asked, first.approval_id), gate(asked, first.approval_id)], { concurrency: 2 });
    expect(raced.filter((gated) => gated.ok)).toEqual([{ ok: true, approvalId: first.approval_id }]);
  }));

it.live("an operation that destroys nothing runs without asking", () =>
  Effect.gen(function* () {
    const { gate, rows } = yield* operationCloud();
    expect(yield* gate({ ...drainOf(["shop.web"]), effects: [] })).toEqual({ ok: true, approvalId: null });
    expect(yield* rows).toEqual([]);
  }));

it.live("a person's destructive operation from the dashboard runs without asking and records no approval", () =>
  Effect.gen(function* () {
    const { provided, caller, rows } = yield* operationCloud();
    const person: Caller = { ...caller, via: "dashboard" };
    expect(yield* provided(gateOperation(person, null, drainOf(["shop.web"])))).toEqual({ ok: true, approvalId: null });
    expect(yield* rows).toEqual([]);
  }));

it.live("an anonymous agent's destructive operation runs without asking and records no approval", () =>
  Effect.gen(function* () {
    const { provided, userId, gate, rows } = yield* operationCloud();
    yield* provided(anonymous(userId));
    expect(yield* gate(drainOf(["shop.web"]))).toEqual({ ok: true, approvalId: null });
    expect(yield* rows).toEqual([]);
  }));

it.live("an agent's destructive operation still asks once a person also owns the Organization", () =>
  Effect.gen(function* () {
    const { provided, userId, gate } = yield* operationCloud();
    yield* provided(anonymous(userId));
    yield* provided(personOwner);
    expect(refusedWith(yield* gate(drainOf(["shop.web"]))).approval).toBe(operationDigest(drainOf(["shop.web"])));
  }));

it.live("an older server removal approval recovers its subject without changing its digest", () =>
  Effect.gen(function* () {
    const { provided, caller, gate } = yield* operationCloud();
    const asked: OperationAsked = {
      ...drainOf([]), verb: "remove",
      effects: [{ kind: "deletes_volume", name: "pgdata", node: "v1", path: "volumes/v1" }],
    };
    const first = refusedWith(yield* gate(asked));
    yield* provided(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.update(operationApprovals).set({
        review: sql`jsonb_set(${operationApprovals.review}, '{operation}', (${operationApprovals.review}->'operation') - 'subject')`,
      }).where(eq(operationApprovals.id, first.approval_id));
    }));
    const approval = yield* provided(getApproval(caller.organization.id, first.approval_id));
    expect(approval.digest).toBe(first.approval);
    expect(approval.review).toMatchObject({ operation: { subject: asked.subject } });
    const effect = approval.review.effects[0] ?? expect.fail("no volume effect");
    expect(destroyedLine(effect, approval.command, approval.review).verb).toBe("Loses access to volume");
  }));

it.live("a destructive operation waits on one approval of exactly its preview, and its retry runs once approved", () =>
  Effect.gen(function* () {
    const { provided, caller, gate, rows } = yield* operationCloud();
    const asked = drainOf(["shop.web"]);
    const first = refusedWith(yield* gate(asked));
    expect(first).toEqual({
      effects: asked.effects,
      approval: operationDigest(asked),
      approval_id: expect.any(String),
      operation: { verb: "drain", name: "fra-1", subject: asked.subject },
      organization: caller.organization.slug,
    });
    expect(refusedWith(yield* gate(asked)).approval_id).toBe(first.approval_id);
    expect(refusedWith(yield* gate(asked, first.approval_id)).approval_id).toBe(first.approval_id);
    expect(yield* rows).toEqual([{ id: first.approval_id, status: "pending", subject: `server:${MACHINE}` }]);

    const unchanged = answers(first.approval);
    expect(yield* provided(decideApproval(caller, first.approval_id, { approve: { digest: first.approval } }, unchanged)))
      .toMatchObject({ ok: true, approval: { status: "approved" } });
    expect(yield* gate(asked, first.approval_id)).toEqual({ ok: true, approvalId: first.approval_id });

    const moved = refusedWith(yield* gate(drainOf(["shop.api", "shop.web"]), first.approval_id));
    expect(moved.approval_id).not.toBe(first.approval_id);
  }));

it.live("a person who asks for what an agent asked gets their own approval to answer at their prompt; no agent answers either", () =>
  Effect.gen(function* () {
    const { provided, caller } = yield* operationCloud();
    const asked = drainOf(["shop.web"]);
    const digest = operationDigest(asked);
    const gateAs = (requester: Caller) => Effect.map(provided(gateOperation(requester, null, asked)), refusedWith);
    const approveAs = (decider: Caller, id: string) =>
      provided(decideApproval(decider, id, { approve: { digest } }, answers(digest)));
    const refused = { ok: false, refusal: { code: "forbidden", message: "Agents can't answer approvals." } };
    const mcp: Caller = { ...caller, credential: { kind: "token", id: "token-mcp", grant: FULL_GRANT }, via: "agent" };
    const otherAgent: Caller = { ...caller, credential: { kind: "token", id: "token-other", grant: FULL_GRANT }, via: "agent" };

    const fromAgent = yield* gateAs(mcp);
    expect((yield* gateAs(mcp)).approval_id).toBe(fromAgent.approval_id);
    expect((yield* gateAs(otherAgent)).approval_id).not.toBe(fromAgent.approval_id);
    expect(yield* approveAs(otherAgent, fromAgent.approval_id)).toMatchObject(refused);
    expect(yield* approveAs(mcp, fromAgent.approval_id)).toMatchObject(refused);

    const fromPerson = yield* gateAs(caller);
    expect(fromPerson.approval_id).not.toBe(fromAgent.approval_id);
    expect(yield* approveAs(caller, fromPerson.approval_id)).toMatchObject({ ok: true, approval: { status: "approved" } });
    expect(yield* provided(gateOperation(caller, fromPerson.approval_id, asked))).toEqual({ ok: true, approvalId: fromPerson.approval_id });
  }));

it.live("asking about a Server's new preview supersedes its pending approval of the old one", () =>
  Effect.gen(function* () {
    const { provided, gate } = yield* operationCloud();
    const old = refusedWith(yield* gate(drainOf(["shop.web"])));
    const current = refusedWith(yield* gate(drainOf(["shop.api", "shop.web"])));
    const other = refusedWith(yield* gate({ ...drainOf(["shop.web"]), subject: "server:other" }));

    expect((yield* provided(getApproval(ORGANIZATION, old.approval_id))).status).toBe("superseded");
    expect((yield* provided(getApproval(ORGANIZATION, current.approval_id))).status).toBe("pending");
    expect((yield* provided(getApproval(ORGANIZATION, other.approval_id))).status).toBe("pending");
  }));

it.live("a pending operation approval reads superseded once its fresh preview differs, and can't be approved", () =>
  Effect.gen(function* () {
    const { provided, caller, gate } = yield* operationCloud();
    const waiting = refusedWith(yield* gate(drainOf(["shop.web"])));

    expect((yield* provided(getApproval(ORGANIZATION, waiting.approval_id))).status).toBe("pending");
    expect((yield* provided(getApproval(ORGANIZATION, waiting.approval_id, answers(waiting.approval)))).status).toBe("pending");
    const changed = answers(operationDigest(drainOf(["shop.api"])));
    expect((yield* provided(getApproval(ORGANIZATION, waiting.approval_id, changed))).status).toBe("superseded");
    expect(yield* provided(decideApproval(caller, waiting.approval_id, { approve: { digest: waiting.approval } }, changed)))
      .toMatchObject({ ok: false, refusal: { code: "conflict" } });
  }));

it.live("a pending operation approval whose Server is gone reads superseded", () =>
  Effect.gen(function* () {
    const { provided, gate } = yield* operationCloud();
    const waiting = refusedWith(yield* gate(drainOf(["shop.web"])));
    expect((yield* provided(getApproval(ORGANIZATION, waiting.approval_id, answers(null)))).status).toBe("superseded");
  }));

const EDGE_ELSEWHERE = "b".repeat(32);
const slot = (machine: string, state: string) => ({
  kind: "service_container", machine_id: machine, runtime: { state }, created_at_unix_nanos: 1,
  resolved_spec: { mode: { mode: "global" }, volumes: [] },
});
const drainFrame = (edgeElsewhere: string) => asTestDouble<RuntimeWatchView>()({
  machines: [{ machine: { id: MACHINE, name: "fra-1" } }, { machine: { id: EDGE_ELSEWHERE, name: "fra-2" } }],
  services: [
    { identity: "shop/metrics", service_id: "metrics", containers: [slot(MACHINE, "running")] },
    { identity: "shop/edge", service_id: "edge", containers: [slot(MACHINE, "running"), slot(EDGE_ELSEWHERE, edgeElsewhere)] },
  ],
});
const cleanFrame = (edge: string) => asTestDouble<RuntimeWatchView>()({
  services: [
    { identity: "shop/metrics", service_id: "metrics", containers: [slot(MACHINE, "running")] },
    { identity: "shop/edge", service_id: "edge", containers: [slot(MACHINE, edge)] },
  ],
});
const removing = (asked: OperationAsked) => asked.effects.map(({ name }) => name);

it.live("a drain approval can't be approved once a Global's other slot stops and the drain would remove it too", () =>
  Effect.gen(function* () {
    const { provided, caller, gate } = yield* operationCloud();
    const approved = drainPlan(drainFrame("running"), new Set(["shop"]), MACHINE) ?? expect.fail("no drain");
    const grown = drainPlan(drainFrame("exited"), new Set(["shop"]), MACHINE) ?? expect.fail("no drain");
    expect(removing(approved)).toEqual(["shop/metrics"]);
    expect(removing(grown)).toEqual(["shop/edge", "shop/metrics"]);

    const waiting = refusedWith(yield* gate(approved));
    const now = answers(operationDigest(grown));
    expect((yield* provided(getApproval(ORGANIZATION, waiting.approval_id, now))).status).toBe("superseded");
    expect(yield* provided(decideApproval(caller, waiting.approval_id, { approve: { digest: waiting.approval } }, now)))
      .toMatchObject({ ok: false, refusal: { code: "conflict" } });
  }));

it.live("an approved clean doesn't cover a Service that started running since, and its retry asks again", () =>
  Effect.gen(function* () {
    const { provided, caller, gate } = yield* operationCloud();
    const approved = cleanPlan(cleanFrame("exited"), "shop", []);
    const grown = cleanPlan(cleanFrame("running"), "shop", []);
    expect(removing(approved)).toEqual(["shop/metrics"]);
    expect(removing(grown)).toEqual(["shop/edge", "shop/metrics"]);

    const waiting = refusedWith(yield* gate(approved));
    yield* provided(decideApproval(caller, waiting.approval_id, { approve: { digest: waiting.approval } }, answers(waiting.approval)));
    const retried = refusedWith(yield* gate(grown, waiting.approval_id));
    expect(retried.approval_id).not.toBe(waiting.approval_id);
    expect(retried.effects).toMatchObject([{ name: "shop/edge" }, { name: "shop/metrics" }]);
  }));

it.live("asking about a Server's drain leaves its pending removal waiting", () =>
  Effect.gen(function* () {
    const { provided, gate } = yield* operationCloud();
    const removal = refusedWith(yield* gate(removePlan(MACHINE, "fra-1", { reset: false, volumes: [] })));
    const drain = refusedWith(yield* gate(drainOf(["shop.web"])));
    expect((yield* provided(getApproval(ORGANIZATION, removal.approval_id))).status).toBe("pending");
    expect((yield* provided(getApproval(ORGANIZATION, drain.approval_id))).status).toBe("pending");
  }));

const freshReads = {
  "the pending list the sidebar and the CLI read": (id: string) =>
    freshPendingApprovals(ORGANIZATION).pipe(Effect.map((approvals) => approvals.some((approval) => approval.id === id))),
  "one approval the sidebar and the CLI read": (id: string) =>
    freshApproval(ORGANIZATION, id).pipe(Effect.map((approval) => approval.status === "pending")),
};

for (const [read, stillWaiting] of Object.entries(freshReads)) {
  it.live(`${read} drops a clean of a Namespace an Environment owns by now`, () =>
    Effect.gen(function* () {
      const { provided, caller } = yield* shopRemovingWeb();
      const [owned = expect.fail("the Environment owns no Namespace")] = yield* provided(ownedNamespaces(ORGANIZATION));
      const frame = asTestDouble<RuntimeWatchView>()({
        services: [{ identity: `${owned}/web`, service_id: "web", containers: [slot(MACHINE, "running")] }],
      });
      const waiting = refusedWith(yield* provided(gateOperation(caller, null, cleanPlan(frame, owned, []))));
      expect((yield* provided(pendingApprovals(ORGANIZATION))).map((approval) => approval.id)).toEqual([waiting.approval_id]);

      const cluster = <A, E, R>(effect: Effect.Effect<A, E, R>) =>
        provided(effect.pipe(Effect.provideService(OrganizationRuntime, asTestDouble<OrganizationRuntimeService>()({}))));
      expect(yield* cluster(stillWaiting(waiting.approval_id))).toBe(false);
      expect((yield* provided(getApproval(ORGANIZATION, waiting.approval_id))).status).toBe("superseded");
    }));
}

it.live("an org rm or forget-servers approval stays approvable while the Cluster answers, and its retry runs", () =>
  Effect.gen(function* () {
    const { provided, caller, gate } = yield* operationCloud();
    const frame = asTestDouble<RuntimeWatchView>()({ machines: [{ machine: { id: MACHINE, name: "fra-1" } }], services: [] });
    const cluster = <A, E, R>(effect: Effect.Effect<A, E, R>) => provided(effect.pipe(Effect.provideService(
      OrganizationRuntime,
      asTestDouble<OrganizationRuntimeService>()({
        open: () => Effect.succeed({ status: "connected" as const, connected: asTestDouble<PloyzSession>()({ watchFirstFrame: () => Effect.succeed(frame) }) }),
      }),
    )));
    const servers = { servers: [{ id: MACHINE, name: "fra-1", reach: "didnt_answer" as const }], volumes: [] };
    for (const asked of [organizationRemovalAsked(caller.organization), forgetServersAsked(caller.organization, servers)]) {
      const waiting = refusedWith(yield* gate(asked));
      expect((yield* cluster(freshApproval(ORGANIZATION, waiting.approval_id))).status).toBe("pending");
      const person: Caller = { ...caller, credential: { kind: "session", id: "browser" }, via: "dashboard" };
      expect(yield* cluster(decideApproval(person, waiting.approval_id, { approve: { digest: waiting.approval } }, freshOperationDigest)))
        .toMatchObject({ ok: true });
      expect(yield* gate(asked, waiting.approval_id)).toMatchObject({ ok: true, approvalId: waiting.approval_id });
    }
  }));

it("a removal that resets the Server and one that keeps its data are different approvals", () => {
  expect(operationDigest(removePlan(MACHINE, "fra-1", { reset: true, volumes: [] }))).not.toBe(operationDigest(removePlan(MACHINE, "fra-1", { reset: false, volumes: [] })));
});

for (const verb of ["remove", "drain", "clean"] as const) {
  for (const reason of ["declined", "cancelled"]) {
    it.live(`a ${reason} ${verb} refuses its named retry, even once it destroys nothing`, () =>
      Effect.gen(function* () {
        const { provided, caller, gate, rows } = yield* operationCloud();
        const plan = { ...drainOf(["shop.web"]), verb };
        const asked = refusedWith(yield* gate(plan));
        yield* provided(decideApproval(caller, asked.approval_id, { reject: { reason } }, answers(asked.approval)));

        const denied = { ok: false, refusal: { code: "approval_denied", message: `A human denied the request to ${verb} fra-1: ${reason}` } };
        expect(yield* gate(plan, asked.approval_id)).toMatchObject(denied);
        expect(yield* gate({ ...plan, effects: [] }, asked.approval_id)).toMatchObject(denied);
        expect(yield* rows).toEqual([{ id: asked.approval_id, status: "denied", subject: `server:${MACHINE}` }]);
      }));
  }
}

it.live("a named approval no human denied lets an operation that destroys nothing run, and leaves a pending one pending", () =>
  Effect.gen(function* () {
    const { provided, caller, gate, rows } = yield* operationCloud();
    const emptied = { ...drainOf(["shop.web"]), effects: [] };
    const waiting = refusedWith(yield* gate(drainOf(["shop.web"])));
    expect(yield* gate(emptied, waiting.approval_id)).toEqual({ ok: true, approvalId: null });
    expect(yield* rows).toEqual([{ id: waiting.approval_id, status: "pending", subject: `server:${MACHINE}` }]);

    yield* provided(decideApproval(caller, waiting.approval_id, { approve: { digest: waiting.approval } }, answers(waiting.approval)));
    expect(yield* gate(emptied, waiting.approval_id)).toEqual({ ok: true, approvalId: null });
    expect(yield* gate(emptied, crypto.randomUUID())).toMatchObject({ ok: false, refusal: { code: "not_found" } });
  }));

it.live("a pending approval still waits on the human whether a retry names it or not", () =>
  Effect.gen(function* () {
    const { gate, rows } = yield* operationCloud();
    const waiting = refusedWith(yield* gate(drainOf(["shop.web"])));
    expect(refusedWith(yield* gate(drainOf(["shop.web"]), waiting.approval_id)).approval_id).toBe(waiting.approval_id);
    expect(refusedWith(yield* gate(drainOf(["shop.web"]))).approval_id).toBe(waiting.approval_id);
    expect(yield* rows).toEqual([{ id: waiting.approval_id, status: "pending", subject: `server:${MACHINE}` }]);
  }));

it.live("the pending list drops an operation approval whose Server is gone", () =>
  Effect.gen(function* () {
    const { provided, gate } = yield* operationCloud();
    const waiting = refusedWith(yield* gate(drainOf(["shop.web"])));
    expect((yield* provided(pendingApprovals(ORGANIZATION, answers(waiting.approval)))).map(({ id }) => id)).toEqual([waiting.approval_id]);
    expect(yield* provided(pendingApprovals(ORGANIZATION, answers(null)))).toEqual([]);
    expect((yield* provided(getApproval(ORGANIZATION, waiting.approval_id))).status).toBe("superseded");
  }));
