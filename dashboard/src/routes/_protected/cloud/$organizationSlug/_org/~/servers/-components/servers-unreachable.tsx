import { WifiOffIcon } from "lucide-react";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Empty, EmptyHeader, EmptyMedia, EmptyTitle } from "#/components/ui/empty";

/** There is nothing to show: Cloud can't reach the cluster, or the watch dropped or came back incomplete with no Servers. */
export function ServersUnreachable() {
  return (
    <Empty variant="first-run">
      <EmptyHeader>
        <EmptyMedia variant="icon"><WifiOffIcon /></EmptyMedia>
        <EmptyTitle>Can’t reach your servers right now</EmptyTitle>
      </EmptyHeader>
    </Empty>
  );
}

/** The evidence is uncertain: the watch dropped or its observation is incomplete, so what shows may be out of date. */
export function ServersStaleAlert() {
  return (
    <Alert>
      <WifiOffIcon />
      <AlertTitle>Can’t reach your servers right now</AlertTitle>
      <AlertDescription>Showing what they last reported.</AlertDescription>
    </Alert>
  );
}
