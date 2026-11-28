import { useState } from "react";
import { ArrowUpCircleIcon, KeyRoundIcon, WifiOffIcon } from "lucide-react";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Button } from "#/components/ui/button";
import { Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "#/components/ui/empty";
import { SSH_UPGRADE } from "#/modules/machines/installer";
import { CopyBlock } from "#/modules/machines/add-server/copy-block";
import type { UnreachableReason } from "#/modules/runtime/runtime.collection";
import { ForgetServersDialog, useForgetServersGate } from "../../../-components/forget-servers-dialog";

/** Why Cloud can't use a Server that answers, by which older Ployz it runs, and what its upgrade brings. */
const OLDER = {
  older_relay: "A server runs an older Ployz that Ployz Cloud can’t reach through the relay. Upgrade it from your computer, "
    + "then upgrade the rest from the Servers page.",
  predates_sealing: "A server runs a Ployz older than 0.3.3, which can’t show Ployz Cloud that it belongs to your cluster. Upgrade "
    + "it from your computer. Ployz Cloud uses it once a server that holds your cluster’s key is back online.",
} satisfies Record<Extract<UnreachableReason, "older_relay" | "predates_sealing">, string>;

/**
 * There is nothing to show: Cloud can't reach the cluster, or the watch dropped or came back incomplete without the
 * Server this page needs. Servers that were deleted can be forgotten from here. A Server on an older Ployz is up, so
 * it gets its upgrade over SSH rather than a Forget that would drop its data. A Server waiting for the cluster's key
 * is up too, and is used once a Server that holds the key is back.
 */
export function ServersUnreachable({ organizationSlug, reason = null }: {
  organizationSlug: string; reason?: UnreachableReason | null;
}) {
  const [forgetting, setForgetting] = useState(false);
  const { allowed, reason: forgetRefused } = useForgetServersGate(organizationSlug);
  if (reason === "awaits_key") {
    return (
      <Empty variant="first-run">
        <EmptyHeader>
          <EmptyMedia variant="icon"><KeyRoundIcon /></EmptyMedia>
          <EmptyTitle>Waiting for a server that holds your cluster’s key</EmptyTitle>
          <EmptyDescription>
            A server answers but doesn’t hold your cluster’s key yet, so Ployz Cloud can’t tell it belongs to your cluster.
            Ployz Cloud uses it once a server that holds the key is back online. Keep it running.
            If this stays while a server that holds the key is online, check the server’s daemon logs.
          </EmptyDescription>
        </EmptyHeader>
      </Empty>
    );
  }
  if (reason === "older_relay" || reason === "predates_sealing") {
    return (
      <Empty variant="first-run">
        <EmptyHeader>
          <EmptyMedia variant="icon"><ArrowUpCircleIcon /></EmptyMedia>
          <EmptyTitle>Upgrade your server over SSH</EmptyTitle>
          <EmptyDescription>
            {OLDER[reason]} On your computer, run:
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
        <EmptyDescription>Servers gone for good?{forgetRefused ? ` ${forgetRefused}` : ""}</EmptyDescription>
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
