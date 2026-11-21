import type { ReactNode } from "react";
import { ObserveOffState } from "#/components/observe/ObserveOffState";

export const METRICS_DOCS = "https://ployz.dev/docs/observe/metrics";

/** ClickHouse is not set up, so Cloud keeps no metrics. */
export function MetricsNotEnabled({ subject }: { subject: "Service" | "Server" }) {
  return (
    <ObserveOffState
      title={`Metrics aren’t enabled on this ${subject}`}
      action={<a href={METRICS_DOCS} target="_blank" rel="noreferrer" className="underline underline-offset-4">Set up metrics</a>}
    >
      Start the observe profile and set <span className="font-mono">OBSERVE_CLICKHOUSE_URL</span>.
    </ObserveOffState>
  );
}

/** A Machine that is up has sent no metrics for a minute. */
export function MetricsNotReporting({ server, action }: { server: string | null; action?: ReactNode }) {
  return (
    <ObserveOffState title={server === null ? "Not reporting" : `${server} isn’t reporting`} action={action}>
      No metrics have arrived in the last minute.
    </ObserveOffState>
  );
}
