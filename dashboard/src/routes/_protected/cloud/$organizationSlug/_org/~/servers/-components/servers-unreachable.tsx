import { useState } from "react";
import { ArrowUpCircleIcon, WifiOffIcon } from "lucide-react";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Button } from "#/components/ui/button";
import { Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "#/components/ui/empty";
import { CopyBlock } from "./copy-block";
import { ForgetServersDialog, useForgetServersGate } from "../../../-components/forget-servers-dialog";

/** What a Server on 0.3.0-beta.6 or earlier runs to upgrade, from a computer that reaches it over SSH. */
export const SSH_UPGRADE = "curl -fsSL https://ployz.sh | sh -s -- beta\nployz server upgrade beta SERVER --connect ssh://USER@HOST";

/**
 * There is nothing to show: Cloud can't reach the cluster, or the watch dropped or came back incomplete without the
 * Server this page needs. Servers that were deleted can be forgotten from here. A Server too old for the relay is up,
 * so it gets its upgrade over SSH rather than a Forget that would drop its data.
 */
export function ServersUnreachable({ organizationSlug, older = false }: { organizationSlug: string; older?: boolean }) {
  const [forgetting, setForgetting] = useState(false);
  const { allowed, reason } = useForgetServersGate(organizationSlug);
  if (older) {
    return (
      <Empty variant="first-run">
        <EmptyHeader>
          <EmptyMedia variant="icon"><ArrowUpCircleIcon /></EmptyMedia>
          <EmptyTitle>Upgrade your server over SSH</EmptyTitle>
          <EmptyDescription>
            A server runs Ployz 0.3.0-beta.6 or earlier, which Ployz Cloud can’t reach through the relay. It keeps running
            your apps. Upgrade it from your computer, with its name and SSH address:
          </EmptyDescription>
        </EmptyHeader>
        <EmptyContent className="max-w-xl text-left">
          <CopyBlock value={SSH_UPGRADE} />
        </EmptyContent>
      </Empty>
    );
  }
  return (
    <Empty variant="first-run">
      <EmptyHeader>
        <EmptyMedia variant="icon"><WifiOffIcon /></EmptyMedia>
        <EmptyTitle>Can’t reach your servers right now</EmptyTitle>
        <EmptyDescription>Servers gone for good?{reason ? ` ${reason}` : ""}</EmptyDescription>
      </EmptyHeader>
      <EmptyContent>
        <Button variant="link" disabled={!allowed} onClick={() => setForgetting(true)}>Forget them and start over.</Button>
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
