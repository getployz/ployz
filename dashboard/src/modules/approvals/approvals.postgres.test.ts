import { it } from "@effect/vitest";
import type { Approval, ConfigCommand, ConfigStore, RpcError as SdkRpcError } from "@ployz/sdk";
import { createRequire } from "node:module";
import { sql } from "drizzle-orm";
import { Deferred, Effect, Fiber, Layer } from "effect";
import { expect, vi } from "vitest";
import {
  askBeforeDestructive,
  decideApproval,
  getApproval,
  requestApproval,
  setOrganizationSettings,
  trustedApproval,
} from "#/modules/approvals/approvals.server";
import { operationApprovals } from "#/modules/approvals/tables";
import { writeStoreAsMember } from "#/modules/config-store/config-store.server";
import { storeTry } from "#/modules/config-store/store-sdk.server";
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
  return { provided, write, refusal, caller, userId, store, pending, withApproval };
});

type Asked = { approval: string; approval_id: string; effects: unknown[] };
const asked = (refused: { details: unknown }) => refused.details as Asked;

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

type Reply = { written?: string; error?: { code: string; message: string; details: Partial<Asked> | null } };

it.live("the CLI's Publish asks over HTTPS, and its retry names the approval a human decided", () =>
  Effect.gen(function* () {
    const { provided, caller } = yield* shopRemovingWeb();
    const { secret } = yield* provided(createOrganizationToken(caller, { name: "agent", expiresInDays: 1 }));
    const cli = (command: ConfigCommand, approval?: string) => provided(Effect.gen(function* () {
      const headers = new Headers({ "content-type": "application/json", authorization: `Bearer ${secret}` });
      if (approval !== undefined) headers.set("x-ployz-approval", approval);
      const response = yield* handleConfigRequest(new Request("http://localhost:3000/api/config/write", {
        method: "POST", headers, body: JSON.stringify(command),
      }));
      // SAFETY: test-only view of the Store's JSON; assertions check every field read.
      return { status: response.status, json: (yield* Effect.promise(() => response.json())) as Reply };
    }));

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
