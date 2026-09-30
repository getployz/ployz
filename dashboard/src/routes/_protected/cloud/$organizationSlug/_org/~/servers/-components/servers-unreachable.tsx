import { useState } from "react";
import { WifiOffIcon } from "lucide-react";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Button } from "#/components/ui/button";
import { Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "#/components/ui/empty";
import { ForgetServersDialog } from "../../../-components/forget-servers-dialog";

/**
 * There is nothing to show: Cloud can't reach the cluster, or the watch dropped or came back incomplete without the
 * Server this page needs. Servers that were deleted can be forgotten from here.
 */
export function ServersUnreachable({ organizationSlug }: { organizationSlug: string }) {
  const [forgetting, setForgetting] = useState(false);
  return (
    <Empty variant="first-run">
      <EmptyHeader>
        <EmptyMedia variant="icon"><WifiOffIcon /></EmptyMedia>
        <EmptyTitle>Can’t reach your servers right now</EmptyTitle>
        <EmptyDescription>Servers gone for good?</EmptyDescription>
      </EmptyHeader>
      <EmptyContent>
        <Button variant="link" onClick={() => setForgetting(true)}>Forget them and start over.</Button>
      </EmptyContent>
      <ForgetServersDialog organizationSlug={organizationSlug} open={forgetting} onOpenChange={setForgetting} />
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
