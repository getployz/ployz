"use client";

import { useTransition } from "react";
import { GlobeIcon } from "lucide-react";
import { toast } from "sonner";
import { CopyButton } from "#/components/copy-button";
import { Button } from "#/components/ui/button";
import { Item, ItemActions, ItemContent, ItemDescription, ItemMedia, ItemTitle } from "#/components/ui/item";
import { Spinner } from "#/components/ui/spinner";
import type { ClusterDomainRow } from "#/modules/cluster-domain/cluster-domain";
import { formatRelativeTime } from "#/utils/relative-time";

export function ClusterDomainSection({ domain, onPublish }: {
  domain: Pick<ClusterDomainRow, "name" | "recordsSyncedAt" | "recordAddresses" | "unreachable" | "certificateNotAfter"> | null;
  onPublish: () => Promise<void>;
}) {
  const [publishing, startPublish] = useTransition();
  const name = domain?.name ?? null;
  const servers = [
    ...(domain?.recordAddresses ?? []).map((server) => ({ ...server, status: "in the set" })),
    ...(domain?.unreachable ?? []).map((server) => ({ ...server, status: "not reachable on port 80" })),
  ];

  function handlePublish() {
    startPublish(async () => {
      try {
        await onPublish();
      } catch (error) {
        toast.error(error instanceof Error ? error.message : "The Cluster Domain couldn’t be published.");
      }
    });
  }

  return (
    <section aria-labelledby="cluster-domain-heading">
      <h2 id="cluster-domain-heading" className="sr-only">Cluster Domain</h2>
      <Item variant="outline">
        <ItemMedia variant="icon">
          <GlobeIcon />
        </ItemMedia>
        <ItemContent>
          <ItemTitle>Cluster Domain</ItemTitle>
          <ItemDescription className={name === null ? undefined : "font-mono"}>{name ?? "Not reserved yet"}</ItemDescription>
          {domain && (
            <>
              <ItemDescription>
                {domain.recordsSyncedAt === null ? "Records not published yet" : `Records published ${formatRelativeTime(domain.recordsSyncedAt)}`}
              </ItemDescription>
              <ItemDescription>
                {domain.certificateNotAfter === null
                  ? "No wildcard certificate yet"
                  : `Wildcard certificate expires ${formatRelativeTime(domain.certificateNotAfter)}`}
              </ItemDescription>
            </>
          )}
          {servers.length === 0 ? null : (
            <ul aria-label="Ingress servers" className="text-xs text-muted-foreground">
              {servers.map((server) => (
                <li key={`${server.status}-${server.machineId}`}>
                  <span className="font-mono">{server.address}</span> · {server.status}
                </li>
              ))}
            </ul>
          )}
        </ItemContent>
        <ItemActions>
          {name === null ? null : <CopyButton value={name} label="Copy Cluster Domain" />}
          <Button type="button" variant="outline" disabled={publishing} onClick={handlePublish}>
            {publishing ? <Spinner data-icon="inline-start" /> : null}
            Publish now
          </Button>
        </ItemActions>
      </Item>
    </section>
  );
}
