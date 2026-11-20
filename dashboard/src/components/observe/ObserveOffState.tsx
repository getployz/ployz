import type { ReactNode } from "react";
import { Card } from "#/components/ui/card";

export const METRICS_DOCS = "https://ployz.dev/docs/observe/metrics";

const LINK_CLASS = "underline underline-offset-4";

/** What an observe feature shows when it has nothing to draw: what is off and what to do about it. */
export function ObserveOffState({ title, hint, action }: { title: string; hint?: ReactNode; action?: ReactNode }) {
  return (
    <Card size="sm" role="status" className="items-center gap-1 px-3 py-10 text-center">
      <h3 className="text-sm font-medium">{title}</h3>
      {hint === undefined ? null : <p className="text-sm text-muted-foreground">{hint}</p>}
      {action === undefined ? null : <p className="text-sm text-muted-foreground">{action}</p>}
    </Card>
  );
}

/** ClickHouse is not set up, so Cloud keeps no metrics. */
export function MetricsNotEnabled({ subject }: { subject: "Service" | "Server" }) {
  return (
    <ObserveOffState
      title={`Metrics aren’t enabled on this ${subject}`}
      hint={<>Start the observe profile and set <span className="font-mono">OBSERVE_CLICKHOUSE_URL</span>.</>}
      action={<a href={METRICS_DOCS} target="_blank" rel="noreferrer" className={LINK_CLASS}>Set up metrics</a>}
    />
  );
}

/** A Machine that is up has sent no metrics for a minute. */
export function MetricsNotReporting({ server, action }: { server: string | null; action?: ReactNode }) {
  return (
    <ObserveOffState
      title={server === null ? "Not reporting" : `${server} isn’t reporting`}
      hint="No metrics have arrived in the last minute."
      action={action}
    />
  );
}
