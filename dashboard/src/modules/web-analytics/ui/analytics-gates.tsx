import { Link } from "@tanstack/react-router";
import { ChartLineIcon } from "lucide-react";
import type { ReactNode } from "react";
import { ObserveOffState } from "#/components/observe/ObserveOffState";
import { Button } from "#/components/ui/button";
import { Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "#/components/ui/empty";
import type { ClosedGate } from "./analytics-model";
import { type AnalyticsScope, useTurnOnAnalytics } from "./analytics.queries";

const OBSERVE_SETUP_DOCS = "https://ployz.dev/docs/self-hosting#keep-logs-metrics-and-traces-in-cloud-optional";

export function Gate({ title, children, action }: { title: string; children?: ReactNode; action?: ReactNode }) {
  return (
    <Empty className="border" data-analytics-gate={title}>
      <EmptyHeader>
        <EmptyMedia variant="icon"><ChartLineIcon /></EmptyMedia>
        <EmptyTitle>{title}</EmptyTitle>
        {children ? <EmptyDescription>{children}</EmptyDescription> : null}
      </EmptyHeader>
      {action ? <EmptyContent>{action}</EmptyContent> : null}
    </Empty>
  );
}

/** Why the page reads nothing: the plan or the Environment's switch. */
export function ClosedGateView({ gate, scope }: { gate: ClosedGate; scope: AnalyticsScope }) {
  if (gate === "self-host-off") return <SelfHostOff />;
  if (gate === "no-clickhouse") return <NoClickhouse />;
  return gate === "production" ? <NeedsProductionView scope={scope} /> : <SiteOff scope={scope} />;
}

function NeedsProductionView({ scope }: { scope: AnalyticsScope }) {
  return (
    <Gate title="Analytics is part of Production"
      action={
        <Button nativeButton={false}
          render={<Link to="/cloud/$organizationSlug/~/billing" params={{ organizationSlug: scope.organizationSlug }} />}>
          See Production
        </Button>
      }>
      Add Production to see visits. Environments with analytics on keep counting meanwhile.
    </Gate>
  );
}

function SelfHostOff() {
  return (
    <Gate title="Analytics is off">
      Set <code className="font-mono">ANALYTICS_ENABLED=true</code> on this Cloud and restart it to count visits.
    </Gate>
  );
}

function NoClickhouse() {
  return (
    <ObserveOffState title="Analytics needs ClickHouse">
      Run the <code className="font-mono">observe</code> profile and set{" "}
      <code className="font-mono">OBSERVE_CLICKHOUSE_URL</code> and{" "}
      <code className="font-mono">CLICKHOUSE_PASSWORD</code> on this Cloud, then restart it to read visits.{" "}
      <a className="underline" href={OBSERVE_SETUP_DOCS} target="_blank" rel="noreferrer">See self-hosting</a>.
    </ObserveOffState>
  );
}

function SiteOff({ scope }: { scope: AnalyticsScope }) {
  const turnOn = useTurnOnAnalytics(scope);
  return (
    <Gate title="Analytics are off for this site"
      action={<Button disabled={turnOn.isPending} onClick={() => turnOn.mutate()}>Turn on</Button>}>
      Counts visits to this Environment&apos;s domains, without cookies. Counting starts within five minutes of turning it on.
    </Gate>
  );
}
