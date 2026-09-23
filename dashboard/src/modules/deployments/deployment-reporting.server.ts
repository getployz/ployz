import "@tanstack/react-start/server-only";
import { Cause, Clock, Effect } from "effect";
import { Database, ReportingDatabase } from "#/server/database.server";

/** Reporting is lossy. Retry on later events, without queuing or replaying output. */
export function deploymentReporting() {
  let incomplete = false;
  let retryAt = 0;
  return {
    get incomplete() { return incomplete; },
    write: <A, E>(program: Effect.Effect<A, E, Database>) => Effect.gen(function* () {
      const now = yield* Clock.currentTimeMillis;
      if (now < retryAt) return;
      const database = yield* ReportingDatabase;
      yield* program.pipe(
        Effect.provideService(Database, database),
        Effect.timeout("250 millis"),
        Effect.catchCause((cause) => {
          if (Cause.hasInterrupts(cause)) return Effect.failCause(cause);
          incomplete = true;
          retryAt = now + 1_000;
          return Effect.logWarning("Deployment logs incomplete; reporting will retry on a later event");
        }),
      );
    }),
  };
}
