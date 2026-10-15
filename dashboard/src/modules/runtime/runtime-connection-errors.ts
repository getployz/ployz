import { Data } from "effect";
import { describeFailureCause } from "#/lib/error-message";

export class RuntimeConnectionFailure extends Data.TaggedError(
  "RuntimeConnectionFailure",
)<{ readonly cause: unknown; readonly message: string; readonly waitable: boolean }> {
  readonly publicErrorCategory = "internal" as const;

  /** `waitable` marks a Cluster that may answer later: no frame came in time, or the transport dropped. */
  constructor(args: { readonly cause: unknown; readonly waitable?: boolean }) {
    super({
      cause: args.cause,
      waitable: args.waitable ?? false,
      message: `Runtime connection failed: ${describeFailureCause(args.cause)}`,
    });
  }
}
