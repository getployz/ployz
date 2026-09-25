"use client";

import { useEffect, useState } from "react";
import { Link } from "@tanstack/react-router";
import { toast } from "sonner";
import { CopyButton } from "#/components/copy-button";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { buttonVariants } from "#/components/ui/button-variants";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemTitle } from "#/components/ui/item";
import { Spinner } from "#/components/ui/spinner";
import { toErrorMessage } from "#/lib/error-message";
import { cn } from "#/lib/utils";
import { type ClusterDomainRow, clusterDomainStatus, type ClusterDomainStatus } from "#/modules/cluster-domain/cluster-domain";

/** A check that never lands (a lost event, a failed sync) stops spinning after this. */
const CHECK_TIMEOUT_MS = 60_000;

/** What the user reads for each status, and the one thing they can do about it. */
function statusCopy(status: ClusterDomainStatus) {
  if (status.kind === "ready") return { message: null, action: null } as const;
  if (status.kind === "setting_up") return { message: "This usually takes a few minutes.", action: null } as const;
  switch (status.reason) {
    case "no_servers":
      return { message: "Add a server to start receiving traffic.", action: "servers" } as const;
    case "no_public_ip":
      return { message: "None of your servers has a public IP address.", action: "servers" } as const;
    case "port_80":
      return { message: `Traffic can’t reach ${status.addresses.join(", ")}. Make sure port 80 is open.`, action: "check" } as const;
    case "https_down":
      return { message: "HTTPS isn’t working right now. We’re fixing it.", action: null } as const;
  }
}

export function ClusterDomainSection({ organizationSlug, domain, onCheck }: {
  organizationSlug: string;
  domain: Omit<ClusterDomainRow, "id"> | null;
  onCheck: () => Promise<void>;
}) {
  // The check is running until the row's checkedAt moves past the value it had when clicked.
  const [pendingCheck, setPendingCheck] = useState<{ readonly before: number | null } | null>(null);
  const checkedAt = domain?.checkedAt?.getTime() ?? null;
  const checking = pendingCheck !== null && pendingCheck.before === checkedAt;
  useEffect(() => {
    if (pendingCheck === null) return;
    const timeout = setTimeout(() => setPendingCheck(null), CHECK_TIMEOUT_MS);
    return () => clearTimeout(timeout);
  }, [pendingCheck]);

  function handleCheck() {
    setPendingCheck({ before: checkedAt });
    onCheck().catch((error) => {
      setPendingCheck(null);
      toast.error(toErrorMessage(error, "The domain couldn’t be checked."));
    });
  }

  const status = domain === null ? null : clusterDomainStatus(domain, new Date());
  const copy = status === null ? { message: "You’ll get one on your first deploy.", action: null } as const : statusCopy(status);

  return (
    <section aria-labelledby="cluster-domain-heading">
      <ItemGroup>
        <ItemContent>
          <ItemTitle>
            <h2 id="cluster-domain-heading">Domain</h2>
          </ItemTitle>
          <ItemDescription>Your services get free addresses under this domain.</ItemDescription>
        </ItemContent>
        <Item variant="outline">
          <ItemContent>
            {domain === null ? null : (
              <ItemTitle className="font-mono">
                {domain.name}
                <CopyButton value={domain.name} label="Copy domain" size="icon-xs" />
              </ItemTitle>
            )}
            {copy.message === null ? null : <ItemDescription>{copy.message}</ItemDescription>}
          </ItemContent>
          {status === null ? null : (
            <ItemActions>
              <StatusBadge status={status} />
              {copy.action === "check" ? (
                <Button type="button" variant="outline" size="sm" disabled={checking} onClick={handleCheck}>
                  {checking ? <Spinner data-icon="inline-start" /> : null}
                  Check again
                </Button>
              ) : copy.action === "servers" ? (
                <Link
                  to="/cloud/$organizationSlug/~/servers"
                  params={{ organizationSlug }}
                  className={cn(buttonVariants({ variant: "outline", size: "sm" }))}
                >
                  View servers
                </Link>
              ) : null}
            </ItemActions>
          )}
        </Item>
      </ItemGroup>
    </section>
  );
}

function StatusBadge({ status }: { status: ClusterDomainStatus }) {
  switch (status.kind) {
    case "ready":
      return <Badge variant="success">Ready</Badge>;
    case "attention":
      return <Badge variant="warning">Needs attention</Badge>;
    case "setting_up":
      return <Badge variant="info"><Spinner data-icon="inline-start" />Setting up</Badge>;
  }
}
