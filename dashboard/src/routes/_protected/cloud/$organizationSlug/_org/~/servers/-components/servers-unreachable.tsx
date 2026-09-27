import { WifiOffIcon } from "lucide-react";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Empty, EmptyHeader, EmptyMedia, EmptyTitle } from "#/components/ui/empty";

/** The Runtime Watch can't reach the cluster and has nothing cached to show. */
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

/** The Runtime Watch dropped; what shows is its last observation. */
export function ServersStaleAlert() {
  return (
    <Alert>
      <WifiOffIcon />
      <AlertTitle>Can’t reach your servers right now</AlertTitle>
      <AlertDescription>Showing what they last reported.</AlertDescription>
    </Alert>
  );
}
