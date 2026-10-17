import { describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { type createServerEnrollmentFoundedEvent, serverEnrollmentReportedEvent } from "#/modules/inngest/events";
import { executeServerEnrollment } from "#/modules/machines/waiting/server-enrollment.inngest";
import type { runInngestEffect } from "#/server/run.server";

const attemptId = "00000000-0000-4000-8000-0000000000f1";
const joinerId = "00000000-0000-4000-8000-0000000000f2";
const machineId = "a".repeat(32);
const data = { attemptId, organizationId: "org-1" };
type News = { stage: string; failure?: { stage: string; message: string } } | { completed: true } | { founded: true };
const heard = (news: News) => ({ name: serverEnrollmentReportedEvent, data: { attemptId, ...news } });
type StepAnswer =
  | boolean
  | undefined
  | readonly string[]
  | typeof data
  | ReturnType<typeof heard>
  | { readonly kind: string; readonly role?: string; readonly state?: string; readonly machineId?: string };
type FoundedEvent = ReturnType<typeof createServerEnrollmentFoundedEvent>;

/** Runs the handler with each named step answering from `answers`, and records what it waited for and sent. */
async function enroll(answers: Readonly<Record<string, StepAnswer>>) {
  const waitedFor: string[] = [];
  const sent: FoundedEvent[][] = [];
  const step = {
    run: async (name: string) => {
      if (!(name in answers)) throw new Error(`unanswered step ${name}`);
      return answers[name];
    },
    waitForEvent: async (name: string, options: { event: string }) => {
      waitedFor.push(options.event);
      return answers[name] ?? null;
    },
    sendEvent: async (_id: string, events: FoundedEvent[]) => { sent.push(events); },
  };
  const ended = await executeServerEnrollment(
    { event: { data }, step: asTestDouble<Parameters<typeof executeServerEnrollment>[0]["step"]>()(step), runId: "run-1" },
    asTestDouble<typeof runInngestEffect>()(() => undefined),
  );
  return { ended, waitedFor, sent };
}

describe("executeServerEnrollment", () => {
  it("hears its founder and its own completion on the one event each round waits for", async () => {
    const { ended, waitedFor } = await enroll({
      "normalize-request": data,
      "bind-run": { kind: "bound", role: "joiner", state: "awaiting_founder" },
      "founded-0": false,
      "founder-wait-0": heard({ founded: true }),
      "founded-1": true,
      release: true,
      "report-0": heard({ stage: "installing" }),
      "stage-0": undefined,
      "report-1": heard({ completed: true }),
      "check-1": { kind: "joined", machineId },
      "join-1": true,
    });
    expect(ended).toEqual({ attemptId, state: "joined", machineId });
    expect(waitedFor).toEqual([serverEnrollmentReportedEvent, serverEnrollmentReportedEvent, serverEnrollmentReportedEvent]);
  });

  it("ends failed on the failure a check-in reports", async () => {
    const { ended } = await enroll({
      "normalize-request": data,
      "bind-run": { kind: "bound", role: "founder", state: "instructed" },
      "report-0": heard({ stage: "storage", failure: { stage: "storage", message: "Docker install failed" } }),
      "fail-0": undefined,
    });
    expect(ended).toEqual({ attemptId, state: "failed", message: "Docker install failed" });
  });

  it("tells each joiner still waiting that the founder it waited for joined", async () => {
    const { ended, sent } = await enroll({
      "normalize-request": data,
      "bind-run": { kind: "bound", role: "founder", state: "instructed" },
      "report-0": heard({ completed: true }),
      "check-0": { kind: "joined", machineId },
      "join-0": true,
      "awaiting-joiners": [joinerId],
    });
    expect(ended).toEqual({ attemptId, state: "joined", machineId });
    expect(sent).toEqual([[{
      id: `organization-founded-${attemptId}-${joinerId}`,
      name: serverEnrollmentReportedEvent,
      data: { attemptId: joinerId, founded: true },
    }]]);
  });
});
