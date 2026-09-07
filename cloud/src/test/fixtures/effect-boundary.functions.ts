import { createServerFn } from "@tanstack/react-start";
import { Data, Effect, Schema } from "effect";
import {
  actorMiddleware,
  publicErrorMiddleware,
  runActor,
  strictValidator,
} from "#/server/tanstack";

const BoundaryInput = Schema.Struct({
  action: Schema.Literals(["actor", "fail", "dispose"]),
});

class DatabaseFailure extends Data.TaggedError("DatabaseFailure")<{
  readonly cause: Error;
  readonly providerBody: string;
  readonly secret: string;
}> {}

export const effectBoundaryServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(BoundaryInput))
  .handler(({ context, data }) => {
    if (data.action === "actor") {
      return { userId: context.actor.userId };
    }
    if (data.action === "dispose") {
      return import("#/server/runtime.server").then(({ AppRuntime }) =>
        AppRuntime.dispose().then(() => ({ disposed: true as const }))
      );
    }
    return runActor(
      context,
      Effect.fail(
        new DatabaseFailure({
          cause: new Error("database host db.internal.test"),
          providerBody: "provider-token",
          secret: "application-secret",
        }),
      ),
    );
  });
