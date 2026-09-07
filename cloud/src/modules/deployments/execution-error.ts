import { Data } from "effect";

export class DeploymentExecutionError extends Data.TaggedError(
  "DeploymentExecutionError",
)<{
  readonly message: string;
  readonly failureCode: string;
  readonly cause?: unknown;
}> {
  readonly retriable = false as const;
}
