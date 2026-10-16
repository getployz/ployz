import { ChartLineIcon } from "lucide-react";
import type { ReactNode } from "react";
import { Button } from "#/components/ui/button";
import { Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "#/components/ui/empty";
import type { ClosedGate } from "./analytics-model";
import { type AnalyticsScope, useTurnOnAnalytics } from "./analytics.queries";

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
  return gate === "self-host-off" ? <SelfHostOff /> : <SiteOff scope={scope} />;
}

function SelfHostOff() {
  return (
    <Gate title="Analytics is off">
      Set <code className="font-mono">ANALYTICS_ENABLED=true</code> on this Cloud and restart it to count visits.
    </Gate>
  );
}

function SiteOff({ scope }: { scope: AnalyticsScope }) {
  const turnOn = useTurnOnAnalytics(scope);
  return (
    <Gate title="Analytics is off for this Environment"
      action={<Button disabled={turnOn.isPending} onClick={() => turnOn.mutate()}>Turn on</Button>}>
      Counts visits to this Environment&apos;s domains from the moment it is on, without cookies.
    </Gate>
  );
}
