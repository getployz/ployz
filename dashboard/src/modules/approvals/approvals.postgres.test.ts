import { it } from "@effect/vitest";
import type { ConfigCommand, DiffView } from "@ployz/sdk";
import { Effect, Layer } from "effect";
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
import type { StoreRefusal } from "#/modules/config-store/store.contract";
import type { Caller } from "#/modules/identity/actor";
import { organization } from "#/modules/organization/tables";
import { Database } from "#/server/database.server";
import { seedStoreGitService, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000a501";
const OTHER = "00000000-0000-4000-8000-00000000a502";
const PROJECT = "00000000-0000-4000-8000-00000000a503";
const ENVIRONMENT = "00000000-0000-4000-8000-00000000a504";
const SERVICE = "00000000-0000-4000-8000-00000000a505";
const here = { project: null, environment: null };
const publish: ConfigCommand = { command: "publish", environment: here, version: null, accept_volume_loss: [] };
const addCache: ConfigCommand = { command: "create_service", id: SERVICE.replace("a505", "a506"), environment: here, name: "cache", image: "redis:7" };

/**
 * Shop with `web` published, then removed in the Working State. Running a Deployment here takes a Server, so
 * `refusal` words the Store's `approval_required` for removing `web` at the Environment's current version.
 */
const shopRemovingWeb = Effect.fn(function* () {
  const services = yield* Layer.build(yield* storeTestCloud());
  const provided = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provide(services));
  const userId = yield* provided(seedStoreOrganization(ORGANIZATION));
  const store = yield* provided(seedStoreGitService(ORGANIZATION, { project: PROJECT, environment: ENVIRONMENT, service: SERVICE }));
  const write = (command: ConfigCommand) => Effect.promise(() => store.write(ORGANIZATION, command));
  yield* write(publish);
  yield* write({ command: "remove_service", environment: here, service: "web" });
  const refusal = Effect.gen(function* () {
    const diff: DiffView = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "diff", environment: here }));
    const effects = [{ kind: "removes_service", name: "web", node: SERVICE, path: "web" }];
    const refused: StoreRefusal = {
      code: "approval_required",
      message: "Publishing would remove Service web.",
      details: { effects, approval: `${diff.version}:${"f".repeat(64)}`, diff },
    };
    return refused;
  });
  const caller: Caller = { userId, organization: { id: ORGANIZATION, slug: "shop" }, credential: { kind: "token", id: "token-1" } };
  return { provided, write, refusal, caller, userId, store };
});

type Asked = { approval: string; approval_id: string; effects: unknown[] };

it.live("a destructive CLI publish waits for a human to approve exactly what it destroys", () =>
  Effect.gen(function* () {
    const { provided, refusal, caller } = yield* shopRemovingWeb();
    // No settings row: the Organization asks.
    expect(yield* provided(askBeforeDestructive(ORGANIZATION))).toBe(true);
    expect(yield* provided(trustedApproval(ORGANIZATION, null))).toEqual({ ok: true, approval: "required" });

    const asked = (yield* provided(requestApproval(caller, publish, yield* refusal))).details as Asked;
    expect(asked.effects).toMatchObject([{ kind: "removes_service", name: "web" }]);
    // Asking again while it waits answers the same approval.
    const again = (yield* provided(requestApproval(caller, publish, yield* refusal))).details as Asked;
    expect(again.approval_id).toBe(asked.approval_id);
    expect(yield* provided(getApproval(ORGANIZATION, asked.approval_id))).toMatchObject({ status: "pending", digest: asked.approval });
    expect(yield* provided(trustedApproval(ORGANIZATION, asked.approval_id))).toEqual({ ok: true, approval: "required" });

    // Another Organization can't see it, or retry with it.
    yield* provided(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.insert(organization).values({ id: OTHER, name: "Other", slug: "other" });
    }));
    expect(yield* provided(trustedApproval(OTHER, asked.approval_id))).toMatchObject({ ok: false, refusal: { code: "not_found" } });
    expect(yield* provided(Effect.flip(getApproval(OTHER, asked.approval_id)))).toMatchObject({ _tag: "NotFound" });

    // Approving another digest conflicts; approving the one asked about is what the retry tells the Store.
    expect(yield* provided(decideApproval(caller, asked.approval_id, { approve: { digest: `${asked.approval}0` } })))
      .toMatchObject({ ok: false, refusal: { code: "conflict" } });
    expect(yield* provided(decideApproval(caller, asked.approval_id, { approve: { digest: asked.approval } })))
      .toMatchObject({ ok: true, approval: { status: "approved" } });
    expect(yield* provided(decideApproval(caller, asked.approval_id, { approve: { digest: asked.approval } })))
      .toMatchObject({ ok: true, approval: { status: "approved" } });
    expect(yield* provided(decideApproval(caller, asked.approval_id, { reject: {} })))
      .toMatchObject({ ok: false, refusal: { code: "conflict" } });
    expect(yield* provided(trustedApproval(ORGANIZATION, asked.approval_id)))
      .toEqual({ ok: true, approval: { approved: asked.approval } });
  }));

it.live("an approval the plan moved past is superseded, and a denial answers the agent with its reason", () =>
  Effect.gen(function* () {
    const { provided, write, refusal, caller } = yield* shopRemovingWeb();
    const stale = (yield* provided(requestApproval(caller, publish, yield* refusal))).details as Asked;

    // Editing the Environment moves its version: the waiting approval no longer names this plan.
    yield* write(addCache);
    const current = yield* provided(getApproval(ORGANIZATION, stale.approval_id));
    expect(current.status).toBe("superseded");
    expect(yield* provided(decideApproval(caller, stale.approval_id, { approve: { digest: current.digest } })))
      .toMatchObject({ ok: false, refusal: { code: "conflict" } });

    const fresh = (yield* provided(requestApproval(caller, publish, yield* refusal))).details as Asked;
    expect(fresh.approval_id).not.toBe(stale.approval_id);
    expect(yield* provided(decideApproval(caller, fresh.approval_id, { reject: { reason: "web still serves traffic" } })))
      .toMatchObject({ ok: true, approval: { status: "denied", reason: "web still serves traffic" } });
    expect(yield* provided(trustedApproval(ORGANIZATION, fresh.approval_id))).toMatchObject({
      ok: false,
      refusal: { code: "approval_denied", message: "A human denied this: web still serves traffic" },
    });
  }));

it.live("asking about a new plan supersedes the Environment's older pending approval", () =>
  Effect.gen(function* () {
    const { provided, write, refusal, caller } = yield* shopRemovingWeb();
    const before = (yield* provided(requestApproval(caller, publish, yield* refusal))).details as Asked;
    yield* write(addCache);
    yield* provided(requestApproval(caller, publish, yield* refusal));
    const rows = yield* provided(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      return yield* drizzle.select({ id: operationApprovals.id, status: operationApprovals.status }).from(operationApprovals);
    }));
    expect(rows.find((row) => row.id === before.approval_id)?.status).toBe("superseded");
    expect(rows.filter((row) => row.status === "pending")).toHaveLength(1);
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
