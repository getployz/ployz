const DAY_MS = 24 * 60 * 60 * 1000;
const WARN_AFTER_DAYS = 5;
const CLOSE_AFTER_DAYS = 7;

/** What the idle rule needs, read from the Org Store in the browser and from Postgres by the sweep. */
export type IdleCloseInputs = {
  readonly branches: ReadonlyArray<{ environmentId: string; parentEnvironmentId: string; kept: boolean }>;
  /** Each Environment's latest Cloud Deployment Attempt. */
  readonly latestAttemptAt: ReadonlyMap<string, Date>;
  readonly defaultEnvironmentIds: ReadonlySet<string>;
};

export type IdleClose = { kind: "none" } | { kind: "warn"; daysLeft: number } | { kind: "due" };

/**
 * A Branch warns from day 5 and is due at day 7 after its latest Cloud Deployment Attempt. It never closes when kept,
 * never deployed, a Parent of open Branches, or the Default Environment.
 */
export function idleClose(environmentId: string, inputs: IdleCloseInputs, now: Date): IdleClose {
  const { branches } = inputs;
  const branch = branches.find((row) => row.environmentId === environmentId);
  const latest = inputs.latestAttemptAt.get(environmentId);
  if (
    branch === undefined || branch.kept || latest === undefined
    || inputs.defaultEnvironmentIds.has(environmentId)
    || branches.some((row) => row.parentEnvironmentId === environmentId)
  ) return { kind: "none" };
  const idleMs = now.getTime() - latest.getTime();
  if (idleMs >= CLOSE_AFTER_DAYS * DAY_MS) return { kind: "due" };
  if (idleMs >= WARN_AFTER_DAYS * DAY_MS) return { kind: "warn", daysLeft: Math.ceil((CLOSE_AFTER_DAYS * DAY_MS - idleMs) / DAY_MS) };
  return { kind: "none" };
}

/** The Branches the sweep closes now. */
export function dueForIdleClose(inputs: IdleCloseInputs, now: Date): string[] {
  return inputs.branches
    .filter((branch) => idleClose(branch.environmentId, inputs, now).kind === "due")
    .map((branch) => branch.environmentId);
}
