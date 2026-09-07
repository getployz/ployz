import { Data } from "effect";
import { describeFailureCause } from "#/lib/error-message";

export class ServiceWorkingIntentNotFound extends Data.TaggedError(
  "ServiceWorkingIntentNotFound",
)<{ readonly message: string }> {
  readonly publicErrorCategory = "not-found" as const;
}

export class ServiceWorkingIntentConflict extends Data.TaggedError(
  "ServiceWorkingIntentConflict",
)<{ readonly message: string }> {
  readonly publicErrorCategory = "conflict" as const;
}

export class ServiceWorkingIntentPersistenceFailure extends Data.TaggedError(
  "ServiceWorkingIntentPersistenceFailure",
)<{ readonly cause: unknown; readonly message: string }> {
  readonly publicErrorCategory = "internal" as const;

  constructor(args: { readonly cause: unknown }) {
    super({
      ...args,
      message: `Service Working Intent persistence failed: ${describeFailureCause(args.cause)}`,
    });
  }
}
