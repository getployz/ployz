import { Data } from "effect";
import { describeFailureCause } from "#/lib/error-message";

export class RuntimeConnectionFailure extends Data.TaggedError(
  "RuntimeConnectionFailure",
)<{ readonly cause: unknown; readonly message: string }> {
  readonly publicErrorCategory = "internal" as const;

  constructor(args: { readonly cause: unknown }) {
    super({
      ...args,
      message: `Runtime connection failed: ${describeFailureCause(args.cause)}`,
    });
  }
}
