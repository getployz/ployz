import { describe, expect, it } from "vitest";
import type { createClusterDomainSyncRequestedEvent } from "#/modules/inngest/events";
import { asTestDouble } from "#/lib/test-double";
import { applyServerWebTraffic } from "#/modules/machines/server-web-traffic.inngest";
import type { runInngestEffect } from "#/server/run.server";

const data = { attemptId: "00000000-0000-4000-8000-0000000000e1", organizationId: "org-1", machineId: "a".repeat(32) };

type Answers = {
  readonly "bind-run": { readonly kind: "bound" } | { readonly kind: "settled"; readonly state: "finished" };
  readonly "execute-once"?: { readonly attemptId: string; readonly state: "finished" };
};

/** Runs the handler with each Database step answering from `answers`, and records the events it sends. */
async function apply(answers: Answers) {
  const sent: ReturnType<typeof createClusterDomainSyncRequestedEvent>[] = [];
  const step = {
    run: <A>(name: string, fn: () => A) => (name in answers ? answers[name as keyof Answers] : fn()),
    sendEvent: (_id: string, event: ReturnType<typeof createClusterDomainSyncRequestedEvent>) => { sent.push(event); },
  };
  const ended = await applyServerWebTraffic(
    { event: { data }, step: asTestDouble<Parameters<typeof applyServerWebTraffic>[0]["step"]>()(step), runId: "run-1" },
    asTestDouble<typeof runInngestEffect>()(() => undefined),
  );
  return { ended, sent };
}

describe("applyServerWebTraffic", () => {
  it("asks DNS to catch up once the change ends, since a proxy that kept running changes no container", async () => {
    const { ended, sent } = await apply({ "bind-run": { kind: "bound" }, "execute-once": { attemptId: data.attemptId, state: "finished" } });
    expect(ended).toEqual({ attemptId: data.attemptId, state: "finished" });
    expect(sent).toEqual([{ name: "cluster-domain/sync.requested", data: { organizationId: "org-1" } }]);
  });

  it("sends nothing for a change another run already settled", async () => {
    const { sent } = await apply({ "bind-run": { kind: "settled", state: "finished" } });
    expect(sent).toEqual([]);
  });
});
