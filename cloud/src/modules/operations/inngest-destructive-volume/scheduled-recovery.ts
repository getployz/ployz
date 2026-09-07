import type { PloyzInngest } from "#/modules/inngest/client";
import { listUnpublishedDestructiveVolumeAttempts } from "#/modules/operations/destructive-volume-attempt.repository";
import { dispatchDestructiveVolumeAttempt } from "#/modules/operations/destructive-volume-dispatch.server";
import { runInngestEffect } from "#/server/run.server";

export const createRecoverDestructiveVolumeOutbox = (inngest: PloyzInngest) =>
  inngest.createFunction(
  {
    id: "recover-destructive-volume-outbox",
    retries: 3,
    triggers: [{ cron: "* * * * *" }],
    concurrency: [{ limit: 1 }],
  },
  async ({ step }) => {
    const attempts = await step.run(
      "list-released-destructive-volume-attempts",
      () => runInngestEffect(listUnpublishedDestructiveVolumeAttempts(50)),
    );
    await Promise.all(
      attempts.map((attempt) =>
        step.run(`publish-destructive-volume-${attempt.id}`, () =>
          runInngestEffect(dispatchDestructiveVolumeAttempt(attempt.id))),
      ),
    );
    return { published: attempts.length };
  },
  );
