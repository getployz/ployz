import type { ServiceVerb } from "@ployz/sdk";
import { InngestTestEngine, mockCtx } from "@inngest/test";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { InngestClient } from "#/modules/inngest/client";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { PloyzProviderError, type PloyzSdkError, type PloyzSession, type ServiceCopies } from "#/modules/runtime/ployz.server";
import type { ServiceActLeg } from "#/modules/service-act/service-act";
import { createActService } from "#/modules/service-act/service-act.inngest";
import { actOnServer, requestServiceAct, serviceOperations } from "#/modules/service-act/service-act.server";
import type { Database } from "#/server/database.server";
import { makeInngestEffectRunner, type runInngestEffect } from "#/server/run.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const organizationId = "00000000-0000-4000-8000-000000000d01";
const userId = "00000000-0000-4000-8000-000000000d02";
const caller = { organizationId, userId };

const inngest = new Inngest({ id: "service-act-test" });
const send = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

describe("act-service", () => {
  let harness: PostgresTestHarness;
  let copies: ServiceCopies;
  let calls: string[];
  let answer: (call: string) => Effect.Effect<void, PloyzSdkError>;

  const runEffect = makeInngestEffectRunner(<A, E>(operation: Effect.Effect<A, E, Database | OrganizationRuntime | InngestClient>) =>
    harness.runEffect(operation.pipe(
      Effect.provideService(InngestClient, inngest),
      Effect.provideService(OrganizationRuntime, {
        cancel: () => Effect.void,
        open: () => Effect.succeed({ status: "connected" as const, connected: asTestDouble<PloyzSession>()({
          observeService: () => Effect.succeed(copies),
          stopContainer: (_machine: string, container: string) => { calls.push(`stop ${container}`); return answer(`stop ${container}`); },
          startContainer: (_machine: string, container: string) => { calls.push(`start ${container}`); return answer(`start ${container}`); },
        }) }),
      }),
    ))) as typeof runInngestEffect;

  const request = (verb: ServiceVerb, attemptId: string = crypto.randomUUID()) => runEffect(requestServiceAct(caller, {
    attemptId, environmentId: "env-1", service: "web", namespace: "acme-prod", runtime: "web", verb, only: null, digest: verb,
  }));
  const act = (actId: string, runId = "run-act") => new InngestTestEngine({
    function: createActService(new Inngest({ id: "test" }), runEffect),
    events: [{ name: "service/act.requested", data: { actId, organizationId } }],
    transformCtx: (ctx) => ({ ...mockCtx(ctx), runId }),
  }).execute();
  const operation = async (actId: string) =>
    (await runEffect(serviceOperations(organizationId, { environment: "env-1", service: "web" }, actId, 1)))[0];

  beforeAll(async () => { harness = await startPostgresTestHarness(); }, 60_000);
  afterAll(async () => { await harness?.stop(); });

  beforeEach(async () => {
    copies = {
      copies: [
        { server: "alpha", machine: "m-alpha", container: "web-a", state: "running" },
        { server: "beta", machine: "m-beta", container: "web-b", state: "running" },
      ],
      unanswered: [],
    };
    calls = [];
    answer = () => Effect.void;
    send.mockReset();
    send.mockResolvedValue({ ids: [] });
    await harness.pool.query(`
      truncate table organization, "user", operation_outbox cascade;
      insert into organization (id, name, slug) values ('${organizationId}', 'Acme', 'acme');
      insert into "user" (id, email, name) values ('${userId}', 'member@example.com', 'Member');
    `);
  });

  it("restarts Server by Server, and a second delivery of the event moves nothing again", async () => {
    const admitted = await request("restart");
    if (!admitted.ok) throw new Error("refused");
    expect(send.mock.calls).toEqual([[expect.objectContaining({ id: `service-act-${admitted.id}` })]]);

    expect((await act(admitted.id)).result).toEqual({ actId: admitted.id, state: "done" });
    expect(calls).toEqual(["stop web-a", "start web-a", "stop web-b", "start web-b"]);
    expect(await operation(admitted.id)).toMatchObject({
      run: "restart web",
      state: "done",
      servers: [
        { server: "alpha", stopped: "yes", started: "yes", reason: null },
        { server: "beta", stopped: "yes", started: "yes", reason: null },
      ],
    });

    expect((await act(admitted.id, "run-duplicate")).result).toEqual({ actId: admitted.id, state: "done" });
    expect(calls).toHaveLength(4);
  });

  it("answers a replayed attempt with its row, and refuses another act while the Service is busy", async () => {
    const attemptId = crypto.randomUUID();
    const first = await request("stop", attemptId);
    expect(await request("stop", attemptId)).toEqual(first);
    if (!first.ok) throw new Error("refused");

    const busy = await request("restart");
    expect(busy).toMatchObject({ ok: false, refusal: { code: "conflict", details: { run: { attempt: first.id, run: "stop web", state: "accepted" } } } });
  });

  it("still starts a Container whose stop the Server never answered, and ends unknown", async () => {
    answer = (call) => call === "stop web-b"
      ? Effect.fail(new PloyzProviderError({ operation: "stop container", cause: { code: "unavailable", message: "gone" } }))
      : Effect.void;
    const admitted = await request("restart");
    if (!admitted.ok) throw new Error("refused");

    expect((await act(admitted.id)).result).toEqual({ actId: admitted.id, state: "unknown" });
    expect(calls).toEqual(["stop web-a", "start web-a", "stop web-b", "start web-b"]);
    expect((await operation(admitted.id))?.servers).toEqual([
      { server: "alpha", stopped: "yes", started: "yes", reason: null },
      { server: "beta", stopped: "unknown", started: "yes", reason: "beta didn't answer." },
    ]);
  });

  it("still starts a Container whose stop failed", async () => {
    answer = (call) => call === "stop web-a"
      ? Effect.fail(new PloyzProviderError({ operation: "stop container", cause: { code: "internal", message: "no" } }))
      : Effect.void;
    const admitted = await request("restart");
    if (!admitted.ok) throw new Error("refused");

    expect((await act(admitted.id)).result).toEqual({ actId: admitted.id, state: "failed" });
    expect(calls).toEqual(["stop web-a", "start web-a", "stop web-b", "start web-b"]);
    expect(await operation(admitted.id)).toMatchObject({ reason: "alpha couldn't stop it.", servers: [{ server: "alpha", stopped: "no", started: "yes" }, {}] });
  });

  it("never asks again for a stop a crashed step already sent", async () => {
    const admitted = await request("stop");
    if (!admitted.ok) throw new Error("refused");
    const sent: ServiceActLeg[] = [
      { server: "alpha", machine: "m-alpha", container: "web-a", stop: "sent", start: null, reason: null },
    ];
    await harness.pool.query(
      `update service_act set state = 'running', inngest_run_id = 'run-1', started_at = now(), before = $2, legs = $3 where id = $1`,
      [admitted.id, JSON.stringify(copies), JSON.stringify(sent)],
    );

    expect(await runEffect(actOnServer({ actId: admitted.id, organizationId }, "run-1", "alpha", "stop")))
      .toEqual([{ ...sent[0], stop: "unknown", reason: "alpha didn't answer." }]);
    expect(calls).toEqual([]);
  });

  it("leaves a Container that isn't running where it is on stop", async () => {
    copies.copies = copies.copies.map((copy) => copy.container === "web-b" ? { ...copy, state: "exited" } : copy);
    const admitted = await request("stop");
    if (!admitted.ok) throw new Error("refused");

    expect((await act(admitted.id)).result).toEqual({ actId: admitted.id, state: "done" });
    expect(calls).toEqual(["stop web-a"]);
    expect((await operation(admitted.id))?.servers.map(({ stopped }) => stopped)).toEqual(["yes", "yes"]);
  });
});
