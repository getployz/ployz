import { Data } from "effect";
import { describeFailureCause } from "#/lib/error-message";

export class DestructiveVolumeEnvironmentNotFound extends Data.TaggedError(
  "DestructiveVolumeEnvironmentNotFound",
)<{ readonly environmentId: string; readonly message: string }> {
  readonly publicErrorCategory = "not-found" as const;

  constructor(args: { readonly environmentId: string }) {
    super({
      ...args,
      message: `Environment not found: ${args.environmentId}`,
    });
  }
}

export class DestructiveVolumePersistenceFailure extends Data.TaggedError(
  "DestructiveVolumePersistenceFailure",
)<{ readonly cause: unknown; readonly message: string }> {
  readonly publicErrorCategory = "internal" as const;

  constructor(args: { readonly cause: unknown }) {
    super({
      ...args,
      message: `Destructive volume persistence failed: ${describeFailureCause(args.cause)}`,
    });
  }
}

export class DestructiveVolumeProviderFailure extends Data.TaggedError(
  "DestructiveVolumeProviderFailure",
)<{ readonly cause: unknown; readonly message: string }> {
  readonly publicErrorCategory = "internal" as const;

  constructor(args: { readonly cause: unknown }) {
    super({
      ...args,
      message: `Destructive volume provider failed: ${describeFailureCause(args.cause)}`,
    });
  }
}

export class CoreOperationWatchConflict extends Data.TaggedError(
  "CoreOperationWatchConflict",
)<{ readonly message: string }> {
  readonly publicErrorCategory = "conflict" as const;
}

export class CoreOperationEvidencePersistenceFailure extends Data.TaggedError(
  "CoreOperationEvidencePersistenceFailure",
)<{ readonly cause: unknown; readonly message: string }> {
  readonly publicErrorCategory = "internal" as const;

  constructor(args: { readonly cause: unknown }) {
    super({
      ...args,
      message: `Core operation evidence persistence failed: ${describeFailureCause(args.cause)}`,
    });
  }
}
