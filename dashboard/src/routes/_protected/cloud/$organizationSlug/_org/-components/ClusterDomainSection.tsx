"use client";

import { useEffect, useState } from "react";
import { Link } from "@tanstack/react-router";
import { toast } from "sonner";
import { CopyButton } from "#/components/copy-button";
import { SettingsRow, SettingsSection } from "#/components/settings/settings-section";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { buttonVariants } from "#/components/ui/button-variants";
import { Spinner } from "#/components/ui/spinner";
import { toErrorMessage } from "#/lib/error-message";
import { cn } from "#/lib/utils";
import {
  type ClusterDomainRow,
  type ClusterDomainState,
  clusterDomainStatus,
  type ExcludedServer,
} from "#/modules/cluster-domain/cluster-domain";

/** A check that never lands (a lost event, a failed sync) stops spinning after this. */
const CHECK_TIMEOUT_MS = 60_000;

/** What the user reads for each status, and the one thing they can do about it. */
function statusCopy(status: ClusterDomainState) {
  if (status.kind === "ready") return { message: null, action: null } as const;
  if (status.kind === "setting_up") return { message: "This usually takes a few minutes.", action: null } as const;
  switch (status.reason) {
    case "no_servers":
      return { message: "No server receives traffic.", action: "servers" } as const;
    case "no_public_ip":
      return { message: "None of your servers has a public IP address.", action: "servers" } as const;
    case "port_80":
      return { message: `Traffic can’t reach ${status.addresses.join(", ")}. Make sure port 80 is open.`, action: "check" } as const;
    case "port_80_taken":
      return { message: `Something else answers on port 80 of ${status.addresses.join(", ")}. Free the port for Ployz.`, action: "check" } as const;
    case "ingress_down":
      return { message: `Ingress isn’t running on ${status.servers.join(", ")}. Deploy to bring it back.`, action: "check" } as const;
    case "https_down":
      return { message: "HTTPS isn’t working right now. We’re fixing it.", action: null } as const;
  }
}

/** The Servers traffic skips while others carry it: the redundancy the user can win back. */
function excludedCopy(excluded: readonly ExcludedServer[]) {
  const on = (reason: ExcludedServer["reason"]) =>
    excluded.flatMap((server) => server.reason !== reason ? [] : "address" in server ? [server.address] : [server.server]).join(", ");
  const closed = on("port_80");
  const taken = on("port_80_taken");
  const down = on("ingress_down");
  return [closed && `Port 80 closed on ${closed}.`, taken && `Port 80 taken on ${taken}.`, down && `Ingress isn’t running on ${down}.`]
    .filter(Boolean).join(" ");
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
  const copy = status === null ? { message: "You’ll get one on your first deploy.", action: null } as const : statusCopy(status.state);
  const excluded = status === null ? "" : excludedCopy(status.excluded);

  return (
    <SettingsSection id="cluster-domain" title="Domain" description="Your services get free addresses under this domain.">
      <SettingsRow
        label={domain === null ? "No domain yet" : <span className="font-mono">{domain.name}</span>}
        hint={copy.message === null && excluded === "" ? undefined : (
          <>
            {copy.message === null ? null : <span>{copy.message}</span>}
            {copy.message !== null && excluded !== "" ? " " : null}
            {excluded === "" ? null : <span>{excluded}</span>}
          </>
        )}
        commit="none"
        control={domain === null || status === null ? undefined : (
          <>
            <CopyButton value={domain.name} label="Copy domain" size="icon-xs" />
            <StatusBadge status={status.state} />
            {copy.action === "check" || excluded !== "" ? (
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
          </>
        )}
      />
    </SettingsSection>
  );
}

function StatusBadge({ status }: { status: ClusterDomainState }) {
  switch (status.kind) {
    case "ready":
      return <Badge variant="success">Ready</Badge>;
    case "attention":
      return <Badge variant="warning">Needs attention</Badge>;
    case "setting_up":
      return <Badge variant="info"><Spinner data-icon="inline-start" />Setting up</Badge>;
  }
}
