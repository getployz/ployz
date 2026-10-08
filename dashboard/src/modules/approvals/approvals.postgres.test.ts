import { it } from "@effect/vitest";
import type { Approval, ConfigCommand, ConfigStore, RpcError as SdkRpcError, RuntimeWatchView } from "@ployz/sdk";
import { createRequire } from "node:module";
import { sql } from "drizzle-orm";
import { Deferred, Effect, Fiber, Layer } from "effect";
import { expect, vi } from "vitest";
import {
  askBeforeDestructive,
  decideApproval,
  gateOperation,
  getApproval,
  type OperationAsked,
  type OperationDigest,
  operationDigest,
  pendingApprovals,
  requestApproval,
  setOrganizationSettings,
  trustedApproval,
} from "#/modules/approvals/approvals.server";
import { asTestDouble } from "#/lib/test-double";
import { operationApprovals } from "#/modules/approvals/tables";
import { writeStoreAsMember } from "#/modules/config-store/config-store.server";
import { storeTry } from "#/modules/config-store/store-sdk.server";
import { cleanPlan, drainPlan, removePlan } from "#/modules/machines/server-operations.server";
import type { Caller } from "#/modules/identity/actor";
import { createOrganizationToken } from "#/modules/identity/organization-token.server";
import { organization } from "#/modules/organization/tables";
import { handleConfigRequest } from "#/routes/api/config/-config.handler";
import { AuthLive } from "#/server/auth.server";
import { Database } from "#/server/database.server";
import { acmeWeb, seedStoreGitService, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const { RpcError } = createRequire(import.meta.url)("@ployz/sdk") as { RpcError: typeof SdkRpcError };

const ORGANIZATION = "00000000-0000-4000-8000-00000000a501";
const OTHER = "00000000-0000-4000-8000-00000000a502";
const PROJECT = "00000000-0000-4000-8000-00000000a503";
const ENVIRONMENT = "00000000-0000-4000-8000-00000000a504";
const SERVICE = "00000000-0000-4000-8000-00000000a505";
const DEPLOYMENT = "00000000-0000-4000-8000-00000000a506";
const UNKNOWN_APPROVAL = "00000000-0000-4000-8000-00000000a5ff";
const here = { project: null, environment: null };
const publish: ConfigCommand = { command: "publish", environment: here, version: null, accept_volume_loss: [] };
const addService = (n: number): ConfigCommand => ({
  command: "create_service", id: `00000000-0000-4000-8000-0000000a51${n.toString().padStart(2, "0")}`, environment: here, name: `cache${n}`, image: "redis:7",
});

/**
 * Shop with `web` deployed (its Applied State written as a finished Deployment would), then removed in the Working
 * State. `refusal` is what the Store answers the CLI's Publish with while a human must approve removing `web`.
 */
const shopRemovingWeb = Effect.fn(function* () {
  const cloud = yield* storeTestCloud();
  const services = yield* Layer.build(Layer.merge(AuthLive.pipe(Layer.provide(cloud)), cloud));
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
  yield* write({ command: "remove_service", environment: here, service: "web" });
  const withApproval = (approval: Approval) => ({ ...acmeWeb(), approval });
  const refusal = Effect.flip(storeTry(() => store.write(ORGANIZATION, publish, withApproval("required"))))
    .pipe(Effect.map((refused) => refused.refusal));
  const caller: Caller = { userId, organization: { id: ORGANIZATION, slug: "shop" }, credential: { kind: "token", id: "token-1" } };
  const pending = provided(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const rows = yield* drizzle.select({ id: operationApprovals.id }).from(operationApprovals)
      .where(sql`${operationApprovals.status} = 'pending'`);
    return rows.map((row) => row.id);
  }));
  const { secret } = yield* provided(createOrganizationToken(caller, { name: "agent", expiresInDays: 1 }));
  const cli = (command: ConfigCommand, approval?: string) => provided(Effect.gen(function* () {
    const headers = new Headers({ "content-type": "application/json", authorization: `Bearer ${secret}` });
    if (approval !== undefined) headers.set("x-ployz-approval", approval);
    const response = yield* handleConfigRequest(new Request("http://localhost:3000/api/config/write", {
      method: "POST", headers, body: JSON.stringify(command),
    }));
    return { status: response.status, json: (yield* Effect.promise(() => response.json())) as Reply };
  }));
  return { provided, write, refusal, caller, userId, store, pending, withApproval, cli };
});

type Asked = { approval: string; approval_id: string; effects: unknown[] };
const asked = (refused: { details: unknown }) => refused.details as Asked;
type Reply = { written?: string; error?: { code: string; message: string; details: Partial<Asked> | null } };

it.live("a destructive CLI publish waits for a human to approve exactly what it destroys, once", () =>
  Effect.gen(function* () {
    const { provided, write, refusal, caller, store, withApproval } = yield* shopRemovingWeb();
    // No settings row: the Organization asks.
    expect(yield* provided(askBeforeDestructive(ORGANIZATION))).toBe(true);
    expect(yield* provided(trustedApproval(ORGANIZATION, null))).toEqual({ ok: true, approval: "required" });

    const refused = yield* refusal;
    expect(refused.code).toBe("approval_required");
    const first = asked(yield* provided(requestApproval(caller, publish, refused)));
    expect(first.effects).toMatchObject([{ kind: "removes_service", name: "web" }]);
    // Asking again while it waits answers the same approval.
    expect(asked(yield* provided(requestApproval(caller, publish, yield* refusal))).approval_id).toBe(first.approval_id);
    expect(yield* provided(getApproval(ORGANIZATION, first.approval_id))).toMatchObject({ status: "pending", digest: first.approval });
    expect(yield* provided(trustedApproval(ORGANIZATION, first.approval_id))).toEqual({ ok: true, approval: "required" });

    // Another Organization can't see it, or retry with it.
    yield* provided(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.insert(organization).values({ id: OTHER, name: "Other", slug: "other" });
    }));
    expect(yield* provided(trustedApproval(OTHER, first.approval_id))).toMatchObject({ ok: false, refusal: { code: "invalid_argument" } });
    expect(yield* provided(Effect.flip(getApproval(OTHER, first.approval_id)))).toMatchObject({ _tag: "NotFound" });

    // Approving another digest conflicts; approving the one asked about is what the retry tells the Store.
    expect(yield* provided(decideApproval(caller, first.approval_id, { approve: { digest: `${first.approval}0` } })))
      .toMatchObject({ ok: false, refusal: { code: "conflict" } });
    expect(yield* provided(decideApproval(caller, first.approval_id, { approve: { digest: first.approval } })))
      .toMatchObject({ ok: true, approval: { status: "approved" } });
    expect(yield* provided(decideApproval(caller, first.approval_id, { approve: { digest: first.approval } })))
      .toMatchObject({ ok: true, approval: { status: "approved" } });
    expect(yield* provided(decideApproval(caller, first.approval_id, { reject: {} })))
      .toMatchObject({ ok: false, refusal: { code: "conflict" } });
    const trusted = yield* provided(trustedApproval(ORGANIZATION, first.approval_id));
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
    const stale = asked(yield* provided(requestApproval(caller, publish, yield* refusal)));

    // Editing the Environment moves its version: the waiting approval no longer names this plan.
    yield* write(addService(1));
    const current = yield* provided(getApproval(ORGANIZATION, stale.approval_id));
    expect(current.status).toBe("superseded");
    expect(yield* provided(decideApproval(caller, stale.approval_id, { approve: { digest: current.digest } })))
      .toMatchObject({ ok: false, refusal: { code: "conflict" } });

    const fresh = asked(yield* provided(requestApproval(caller, publish, yield* refusal)));
    expect(fresh.approval_id).not.toBe(stale.approval_id);
    expect(yield* provided(decideApproval(caller, fresh.approval_id, { reject: { reason: "web still serves traffic" } })))
      .toMatchObject({ ok: true, approval: { status: "denied", reason: "web still serves traffic" } });
    expect(yield* provided(trustedApproval(ORGANIZATION, fresh.approval_id))).toMatchObject({
      ok: false,
      refusal: { code: "approval_denied", message: `A human denied approval ${fresh.approval_id}: web still serves traffic` },
    });
  }));

it.live("a delayed refusal for an older plan leaves the current plan's approval pending", () =>
  Effect.gen(function* () {
    const { provided, write, refusal, caller } = yield* shopRemovingWeb();
    const old = yield* refusal;
    yield* write(addService(1));
    const current = asked(yield* provided(requestApproval(caller, publish, yield* refusal)));
    const late = asked(yield* provided(requestApproval(caller, publish, old)));

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
        [fresh, old, fresh, old, old, fresh, old].map((refused) => provided(requestApproval(caller, publish, refused))),
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
    const slow = yield* Effect.forkChild(provided(requestApproval(caller, publish, old)));
    yield* Deferred.await(answered);

    yield* write(addService(1));
    const current = yield* Effect.forkChild(provided(requestApproval(caller, publish, yield* refusal)));
    yield* Fiber.join(current).pipe(Effect.timeout("1 second"), Effect.ignore);
    yield* Deferred.succeed(delivered, undefined);
    yield* Fiber.join(slow);
    const live = asked(yield* Fiber.join(current));

    expect(yield* pending).toEqual([live.approval_id]);
  }));

it.live("renaming the Project keeps a waiting approval, which still follows its Environment's plan", () =>
  Effect.gen(function* () {
    const { provided, write, refusal, caller } = yield* shopRemovingWeb();
    const waiting = asked(yield* provided(requestApproval(caller, publish, yield* refusal)));

    yield* write({ command: "rename_project", project: "shop", name: "store" });
    expect((yield* provided(getApproval(ORGANIZATION, waiting.approval_id))).status).toBe("pending");

    yield* write(addService(1));
    expect((yield* provided(getApproval(ORGANIZATION, waiting.approval_id))).status).toBe("superseded");
  }));

it.live("the pending list drops an approval the plan moved past and shows the one asked about now", () =>
  Effect.gen(function* () {
    const { provided, write, refusal, caller } = yield* shopRemovingWeb();
    const stale = asked(yield* provided(requestApproval(caller, publish, yield* refusal)));
    expect(yield* provided(pendingApprovals(ORGANIZATION))).toMatchObject([{ id: stale.approval_id, status: "pending", digest: stale.approval }]);

    yield* write(addService(1));
    expect(yield* provided(pendingApprovals(ORGANIZATION))).toEqual([]);
    expect((yield* provided(getApproval(ORGANIZATION, stale.approval_id))).status).toBe("superseded");

    const fresh = asked(yield* provided(requestApproval(caller, publish, yield* refusal)));
    expect((yield* provided(pendingApprovals(ORGANIZATION))).map((approval) => approval.id)).toEqual([fresh.approval_id]);
  }));

it.live("a Store that can't answer leaves a waiting approval pending", () =>
  Effect.gen(function* () {
    const { provided, refusal, caller, store } = yield* shopRemovingWeb();
    const waiting = asked(yield* provided(requestApproval(caller, publish, yield* refusal)));
    vi.spyOn(store, "read").mockRejectedValueOnce(new RpcError({ code: "unavailable", message: "The Store is busy.", details: null }));
    expect((yield* provided(getApproval(ORGANIZATION, waiting.approval_id))).status).toBe("pending");
    expect((yield* provided(getApproval(ORGANIZATION, waiting.approval_id))).status).toBe("pending");
  }));

it.live("an Organization that doesn't ask tells the Store nothing needs approving", () =>
  Effect.gen(function* () {
    const { provided, userId } = yield* shopRemovingWeb();
    expect(yield* provided(setOrganizationSettings({ userId }, { organizationSlug: "shop", askBeforeDestructive: false })))
      .toEqual({ id: ORGANIZATION, askBeforeDestructive: false });
    expect(yield* provided(askBeforeDestructive(ORGANIZATION))).toBe(false);
    expect(yield* provided(trustedApproval(ORGANIZATION, null))).toEqual({ ok: true, approval: "not_required" });
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

    // A write that never publishes ignores the header, whatever it names.
    expect(yield* cli(addService(1), UNKNOWN_APPROVAL)).toMatchObject({ status: 200 });

    const first = yield* cli(publish);
    expect(first.status).toBe(409);
    expect(first.json.error).toMatchObject({ code: "approval_required", details: { effects: [{ kind: "removes_service", name: "web" }] } });
    const firstId = first.json.error?.details?.approval_id ?? expect.fail("no approval_id");

    const unknown = yield* cli(publish, UNKNOWN_APPROVAL);
    expect(unknown.status).toBe(422);
    expect(unknown.json.error).toMatchObject({ code: "invalid_argument", details: { approval_id: UNKNOWN_APPROVAL } });
    expect(unknown.json.error?.message).toContain(UNKNOWN_APPROVAL);

    yield* provided(decideApproval(caller, firstId, { reject: { reason: "not today" } }));
    const denied = yield* cli(publish, firstId);
    expect(denied.status).toBe(403);
    expect(denied.json.error).toMatchObject({ code: "approval_denied", message: `A human denied approval ${firstId}: not today` });

    // A denial isn't remembered either: asking again opens a new approval.
    const second = yield* cli(publish);
    const secondId = second.json.error?.details?.approval_id ?? expect.fail("no approval_id");
    expect(secondId).not.toBe(firstId);
    const digest = second.json.error?.details?.approval ?? expect.fail("no digest");
    yield* provided(decideApproval(caller, secondId, { approve: { digest } }));
    expect(yield* cli(publish, secondId)).toMatchObject({ status: 200, json: { written: "published" } });
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
    answers: (id: string) => ({ status: 403, json: { error: { code: "approval_denied", message: `A human denied approval ${id}: web still serves traffic` } } }),
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
  for (const asking of [true, false]) {
    it.live(`a retry naming ${state} answers the same with asking ${asking ? "on" : "off"}`, () =>
      Effect.gen(function* () {
        const shop = yield* shopRemovingWeb();
        const id = yield* leave(shop);
        yield* shop.provided(setOrganizationSettings(shop.caller, { organizationSlug: "shop", askBeforeDestructive: asking }));
        const writes = vi.spyOn(shop.store, "write");
        expect(yield* shop.cli(publish, id)).toMatchObject(answers(id));
        expect(writes.mock.calls.length > 0).toBe(reachesStore);
      }));
  }
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
  const cloud = yield* storeTestCloud();
  const services = yield* Layer.build(Layer.merge(AuthLive.pipe(Layer.provide(cloud)), cloud));
  const provided = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provide(services));
  const userId = yield* provided(seedStoreOrganization(ORGANIZATION));
  const caller: Caller = { userId, organization: { id: ORGANIZATION, slug: "shop" }, credential: { kind: "token", id: "token-1" } };
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
  expect(operationDigest(asked)).toMatch(/^drain:[0-9a-f]{64}$/);
});

it.live("an operation that destroys nothing runs without asking", () =>
  Effect.gen(function* () {
    const { gate, rows } = yield* operationCloud();
    expect(yield* gate({ ...drainOf(["shop.web"]), effects: [] })).toEqual({ ok: true, approvalId: null });
    expect(yield* rows).toEqual([]);
  }));

it.live("an Organization that doesn't ask runs a destructive operation without recording an approval", () =>
  Effect.gen(function* () {
    const { provided, userId, gate, rows } = yield* operationCloud();
    yield* provided(setOrganizationSettings({ userId }, { organizationSlug: "shop", askBeforeDestructive: false }));
    expect(yield* gate(drainOf(["shop.web"]))).toEqual({ ok: true, approvalId: null });
    expect(yield* rows).toEqual([]);
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
      operation: { verb: "drain", name: "fra-1" },
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
    const removal = refusedWith(yield* gate(removePlan(MACHINE, "fra-1", null)));
    const drain = refusedWith(yield* gate(drainOf(["shop.web"])));
    expect((yield* provided(getApproval(ORGANIZATION, removal.approval_id))).status).toBe("pending");
    expect((yield* provided(getApproval(ORGANIZATION, drain.approval_id))).status).toBe("pending");
  }));

it("a removal that resets the Server and one that keeps its data are different approvals", () => {
  expect(operationDigest(removePlan(MACHINE, "fra-1", []))).not.toBe(operationDigest(removePlan(MACHINE, "fra-1", null)));
});

for (const verb of ["remove", "drain", "clean"] as const) {
  for (const asking of [true, false]) {
    for (const reason of ["declined", "cancelled"]) {
      it.live(`a ${reason} ${verb} refuses its named retry with asking ${asking ? "on" : "off"}, even once it destroys nothing`, () =>
        Effect.gen(function* () {
          const { provided, caller, gate, rows } = yield* operationCloud();
          const plan = { ...drainOf(["shop.web"]), verb };
          const asked = refusedWith(yield* gate(plan));
          yield* provided(decideApproval(caller, asked.approval_id, { reject: { reason } }, answers(asked.approval)));
          yield* provided(setOrganizationSettings(caller, { organizationSlug: "shop", askBeforeDestructive: asking }));

          const denied = { ok: false, refusal: { code: "approval_denied", message: `A human denied approval ${asked.approval_id}: ${reason}` } };
          expect(yield* gate(plan, asked.approval_id)).toMatchObject(denied);
          expect(yield* gate({ ...plan, effects: [] }, asked.approval_id)).toMatchObject(denied);
          expect(yield* rows).toEqual([{ id: asked.approval_id, status: "denied", subject: `server:${MACHINE}` }]);
        }));
    }
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
    expect(yield* gate(emptied, crypto.randomUUID())).toMatchObject({ ok: false, refusal: { code: "invalid_argument" } });
  }));

it.live("a pending approval named with asking off still waits on the human", () =>
  Effect.gen(function* () {
    const { provided, caller, gate, rows } = yield* operationCloud();
    const waiting = refusedWith(yield* gate(drainOf(["shop.web"])));
    yield* provided(setOrganizationSettings(caller, { organizationSlug: "shop", askBeforeDestructive: false }));
    expect(refusedWith(yield* gate(drainOf(["shop.web"]), waiting.approval_id)).approval_id).toBe(waiting.approval_id);
    expect(yield* gate(drainOf(["shop.web"]))).toEqual({ ok: true, approvalId: null });
    expect(yield* rows).toEqual([{ id: waiting.approval_id, status: "pending", subject: `server:${MACHINE}` }]);
  }));
