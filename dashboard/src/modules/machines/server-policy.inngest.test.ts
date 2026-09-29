import { InngestTestEngine } from "@inngest/test";
import { Inngest } from "inngest";
import { Effect } from "effect";
import { describe, expect, it } from "vitest";
import { createApplyServerPolicyChange } from "#/modules/machines/server-policy.inngest";
import type { PloyzSession } from "#/modules/runtime/ployz.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { asTestDouble } from "#/lib/test-double";
import { makeInngestEffectRunner, type runInngestEffect } from "#/server/run.server";

function runWithSession(updates: unknown[][]): typeof runInngestEffect {
  const session = asTestDouble<PloyzSession>()({
    updateMachine: (...args: Parameters<PloyzSession["updateMachine"]>) =>
      Effect.sync(() => {
        updates.push(args);
      }),
  });
  return makeInngestEffectRunner(
    <A, E>(effect: Effect.Effect<A, E, OrganizationRuntime>) =>
      Effect.runPromise(
        effect.pipe(
          Effect.provideService(OrganizationRuntime, {
            cancel: () => Effect.void,
            open: () =>
              Effect.succeed({ status: "connected" as const, connected: session }),
          }),
        ),
      ),
  ) as typeof runInngestEffect;
}

describe("Server Policy Inngest boundary", () => {
  it("applies a queued change to the named Server as a partial Machine update", async () => {
    const updates: unknown[][] = [];
    const output = await new InngestTestEngine({
      function: createApplyServerPolicyChange(
        new Inngest({ id: "test" }),
        runWithSession(updates),
      ),
      events: [
        {
          name: "machine/policy-change.requested",
          data: {
            organizationId: "org-1",
            machineId: "a".repeat(32),
            change: { acceptsBuilds: false, buildConcurrency: "automatic" },
          },
        },
      ],
    }).execute();

    expect(output.result).toEqual({ machineId: "a".repeat(32), applied: true });
    expect(updates).toEqual([
      [
        "a".repeat(32),
        { accepts_builds: false, build_concurrency: { action: "automatic" } },
      ],
    ]);
  });

  it("rejects a malformed change before touching the Runtime", async () => {
    const updates: unknown[][] = [];
    const output = await new InngestTestEngine({
      function: createApplyServerPolicyChange(
        new Inngest({ id: "test" }),
        runWithSession(updates),
      ),
      events: [
        {
          name: "machine/policy-change.requested",
          data: {
            organizationId: "org-1",
            machineId: "a".repeat(32),
            change: { buildConcurrency: 0 },
          },
        },
      ],
    }).execute();

    expect(output.result).toEqual({ skipped: true });
    expect(updates).toEqual([]);
  });
});
