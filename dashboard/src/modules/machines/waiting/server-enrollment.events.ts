import { Schema } from "effect";
import { defineEvent } from "#/modules/inngest/define-event";

export const serverEnrollmentRequested = defineEvent({
  name: "server/enrollment.requested",
  schema: Schema.Struct({ attemptId: Schema.String.check(Schema.isUUID()), organizationId: Schema.String.check(Schema.isNonEmpty()) }),
  id: ({ attemptId }) => `server-enrollment-${attemptId}`,
});

const CheckinReport = Schema.Struct({
  attemptId: Schema.String,
  stage: Schema.NullOr(Schema.String),
  failure: Schema.optionalKey(Schema.Struct({ stage: Schema.NullOr(Schema.String), message: Schema.String })),
});
export type ServerEnrollmentCheckinReport = typeof CheckinReport.Type;

/** What a waiting Server's run hears: a check-in's stage, its completion, or the Organization's founding. */
export const serverEnrollmentReported = defineEvent({
  name: "server/enrollment.reported",
  schema: Schema.Union([
    CheckinReport,
    Schema.Struct({ attemptId: Schema.String, completed: Schema.Literal(true) }),
    Schema.Struct({ attemptId: Schema.String, founded: Schema.Literal(true) }),
  ]),
});

/** Keyed by the check-in, not the stage: a report the run missed while busy comes again on the next one. */
export const createServerEnrollmentReportedEvent = (data: ServerEnrollmentCheckinReport) =>
  ({ id: `server-enrollment-reported-${crypto.randomUUID()}`, ...serverEnrollmentReported.create(data) }) as const;

export const createServerEnrollmentCompletedEvent = (attemptId: string) =>
  ({ id: `server-enrollment-completed-${attemptId}`, ...serverEnrollmentReported.create({ attemptId, completed: true }) }) as const;

export const createServerEnrollmentFoundedEvent = (founderAttemptId: string, attemptId: string) =>
  ({ id: `organization-founded-${founderAttemptId}-${attemptId}`, ...serverEnrollmentReported.create({ attemptId, founded: true }) }) as const;

export const serverEnrollmentCancelled = defineEvent({
  name: "server/enrollment.cancel",
  schema: Schema.Struct({ attemptId: Schema.String }),
  id: ({ attemptId }) => `server-enrollment-cancel-${attemptId}`,
});
