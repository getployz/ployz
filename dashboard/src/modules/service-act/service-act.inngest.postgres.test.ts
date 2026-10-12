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
import { actOnContainer, requestServiceAct, serviceOperations } from "#/modules/service-act/service-act.server";
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
  let answer: (call: string) => Effect.Effect<"serving" | "unconfirmed" | "unhealthy", PloyzSdkError>;

  const runEffect = makeInngestEffectRunner(<A, E>(operation: Effect.Effect<A, E, Database | OrganizationRuntime | InngestClient>) =>
    harness.runEffect(operation.pipe(
      Effect.provideService(InngestClient, inngest),
      Effect.provideService(OrganizationRuntime, {
        cancel: () => Effect.void,
        open: () => Effect.succeed({ status: "connected" as const, connected: asTestDouble<PloyzSession>()({
          observeService: () => Effect.succeed(copies),
          stopContainer: (_machine: string, container: string) => { calls.push(`stop ${container}`); return answer(`stop ${container}`).pipe(Effect.asVoid); },
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
        { server: "alpha", machine: "m-alpha", container: "web-a", state: "running", hook: false },
        { server: "beta", machine: "m-beta", container: "web-b", state: "running", hook: false },
      ],
      unanswered: [],
    };
    calls = [];
    answer = () => Effect.succeed("serving");
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

  it("restarts two Machines that share a name one at a time", async () => {
    copies.copies = copies.copies.map((copy) => ({ ...copy, server: "alpha" }));
    const admitted = await request("restart");
    if (!admitted.ok) throw new Error("refused");

    expect((await act(admitted.id)).result).toEqual({ actId: admitted.id, state: "done" });
    expect(calls).toEqual(["stop web-a", "start web-a", "stop web-b", "start web-b"]);
  });

  it("answers a replayed attempt with its row, and refuses another act while the Service is busy", async () => {
    const attemptId = crypto.randomUUID();
    const first = await request("stop", attemptId);
    expect(await request("stop", attemptId)).toEqual(first);
    if (!first.ok) throw new Error("refused");

    const busy = await request("restart");
    expect(busy).toMatchObject({ ok: false, refusal: { code: "conflict", details: { run: { attempt: first.id, run: "stop web", state: "accepted" } } } });
  });

  it("keeps a renamed Service busy while its runtime still has an act running", async () => {
    const first = await request("stop");
    if (!first.ok) throw new Error("refused");

    const renamed = await runEffect(requestServiceAct(caller, {
      attemptId: crypto.randomUUID(), environmentId: "env-1", service: "site", namespace: "acme-prod", runtime: "web", verb: "restart", only: null, digest: "restart",
    }));
    expect(renamed).toMatchObject({ ok: false, refusal: { code: "conflict", details: { run: { attempt: first.id, run: "stop web" } } } });
  });

  it("still starts a Container whose stop the Server never answered, and ends unknown", async () => {
    answer = (call) => call === "stop web-b"
      ? Effect.fail(new PloyzProviderError({ operation: "stop container", cause: { code: "unavailable", message: "gone" } }))
      : Effect.succeed("serving");
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
      : Effect.succeed("serving");
    const admitted = await request("restart");
    if (!admitted.ok) throw new Error("refused");

    expect((await act(admitted.id)).result).toEqual({ actId: admitted.id, state: "failed" });
    expect(calls).toEqual(["stop web-a", "start web-a", "stop web-b", "start web-b"]);
    expect(await operation(admitted.id)).toMatchObject({ reason: "alpha couldn't stop it.", servers: [{ server: "alpha", stopped: "no", started: "yes" }, {}] });
  });

  it("stops rolling when a Server's Container doesn't come back, leaving later Servers untouched", async () => {
    answer = (call) => call === "start web-a"
      ? Effect.fail(new PloyzProviderError({ operation: "start container", cause: { code: "unavailable", message: "gone" } }))
      : Effect.succeed("serving");
    const admitted = await request("restart");
    if (!admitted.ok) throw new Error("refused");

    expect((await act(admitted.id)).result).toEqual({ actId: admitted.id, state: "unknown" });
    expect(calls).toEqual(["stop web-a", "start web-a"]);
    expect(await operation(admitted.id)).toMatchObject({
      state: "unknown",
      reason: "alpha didn't come back, so Cloud left beta alone.",
      servers: [
        { server: "alpha", stopped: "yes", started: "unknown", reason: "alpha didn't answer." },
        { server: "beta", stopped: null, started: null, reason: null },
      ],
    });
  });

  it("stops rolling when a restarted Container starts but isn't serving", async () => {
    answer = (call) => Effect.succeed(call === "start web-a" ? "unconfirmed" : "serving");
    const admitted = await request("restart");
    if (!admitted.ok) throw new Error("refused");

    expect((await act(admitted.id)).result).toEqual({ actId: admitted.id, state: "unknown" });
    expect(calls).toEqual(["stop web-a", "start web-a"]);
    expect(await operation(admitted.id)).toMatchObject({
      reason: "alpha didn't come back, so Cloud left beta alone.",
      servers: [
        { server: "alpha", stopped: "yes", started: "unknown", reason: "alpha started it, but it isn't serving." },
        { server: "beta", stopped: null, started: null, reason: null },
      ],
    });
  });

  it("gives each Container its own step, and stops rolling when one fails its healthcheck", async () => {
    copies.copies.splice(1, 0, { server: "alpha", machine: "m-alpha", container: "web-a2", state: "running", hook: false });
    answer = (call) => Effect.succeed(call === "start web-a" ? "unhealthy" : "serving");
    const admitted = await request("restart");
    if (!admitted.ok) throw new Error("refused");

    const output = await act(admitted.id);
    expect(output.result).toEqual({ actId: admitted.id, state: "failed" });
    expect(vi.mocked(output.ctx.step.run).mock.calls.map(([id]) => id)).toEqual([
      "normalize-request",
      "claim",
      "stop:m-alpha:web-a",
      "stop:m-alpha:web-a2",
      "start:m-alpha:web-a",
      "start:m-alpha:web-a2",
      "record",
    ]);
    expect(calls).toEqual(["stop web-a", "stop web-a2", "start web-a", "start web-a2"]);
    expect(await operation(admitted.id)).toMatchObject({
      reason: "alpha didn't come back, so Cloud left beta alone.",
      servers: [
        { server: "alpha", stopped: "yes", started: "no", reason: "alpha started it, but it isn't healthy." },
        { server: "alpha", stopped: "yes", started: "yes", reason: null },
        { server: "beta", stopped: null, started: null, reason: null },
      ],
    });
  });

  it("stops a Service's hook Containers with it, and never starts them", async () => {
    copies.copies.splice(1, 0, { server: "alpha", machine: "m-alpha", container: "hook-a", state: "running", hook: true });
    const restarted = await request("restart");
    if (!restarted.ok) throw new Error("refused");

    expect((await act(restarted.id)).result).toEqual({ actId: restarted.id, state: "done" });
    expect(calls).toEqual(["stop web-a", "stop hook-a", "start web-a", "stop web-b", "start web-b"]);

    calls = [];
    copies.copies = copies.copies.map((copy) => ({ ...copy, state: "exited" }));
    const started = await request("start");
    if (!started.ok) throw new Error("refused");

    expect((await act(started.id, "run-start")).result).toEqual({ actId: started.id, state: "done" });
    expect(calls).toEqual(["start web-a", "start web-b"]);
  });

  it("never asks again for a stop a crashed step already sent", async () => {
    const admitted = await request("stop");
    if (!admitted.ok) throw new Error("refused");
    const sent: ServiceActLeg[] = [
      { server: "alpha", machine: "m-alpha", container: "web-a", hook: false, stop: "sent", start: null, reason: null },
    ];
    await harness.pool.query(
      `update service_act set state = 'running', inngest_run_id = 'run-1', started_at = now(), before = $2, legs = $3 where id = $1`,
      [admitted.id, JSON.stringify(copies), JSON.stringify(sent)],
    );

    expect(await runEffect(actOnContainer({ actId: admitted.id, organizationId }, "run-1", "web-a", "stop")))
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
