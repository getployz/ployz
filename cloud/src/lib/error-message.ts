export function toErrorMessage<T>(error: T, fallback: string): string {
  return error instanceof Error ? error.message : fallback;
}

export function describeFailureCause(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}
