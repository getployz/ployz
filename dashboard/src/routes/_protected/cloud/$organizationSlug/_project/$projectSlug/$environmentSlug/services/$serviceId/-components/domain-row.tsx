import { type ReactNode, useState } from "react";
import { Link } from "@tanstack/react-router";
import { AlertTriangleIcon, ArrowUpRightIcon, GlobeIcon, PencilIcon, Trash2Icon } from "lucide-react";
import { CopyButton } from "#/components/copy-button";
import { Button } from "#/components/ui/button";
import { buttonVariants } from "#/components/ui/button-variants";
import { Spinner } from "#/components/ui/spinner";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "#/components/ui/table";
import { SettingsListItem } from "#/components/settings/settings-list";
import type { DnsRecord, DomainRow } from "@ployz/sdk";

export function DomainTitle({
  hostname,
  copyLabel = `Copy ${hostname}`,
  live = false,
}: {
  hostname: string;
  copyLabel?: string;
  /** A live domain's name opens it. */
  live?: boolean;
}) {
  return (
    <div className="flex min-w-0 items-center gap-1">
      {live ? (
        <a href={`https://${hostname}`} target="_blank" rel="noreferrer" className="flex min-w-0 items-center gap-1 font-mono text-sm hover:underline">
          <span className="truncate">{hostname}</span>
          <ArrowUpRightIcon className="size-3.5 shrink-0 text-muted-foreground" />
        </a>
      ) : (
        <span className="truncate font-mono text-sm">{hostname}</span>
      )}
      <CopyButton value={hostname} label={copyLabel} size="icon-xs" />
    </div>
  );
}

export function PendingLabel({ children = "Saving…" }: { children?: ReactNode }) {
  return (
    <span className="flex items-center gap-1 text-muted-foreground text-xs">
      <Spinner />
      {children}
    </span>
  );
}

/** The icon is the status; one short phrase and at most one link say what's next. */
export type DomainStatusView = { icon: ReactNode; phrase: ReactNode; action: "dns" | "organization_settings" | "servers" | null };

/** A Store domain's status: the Store words the phrase and names the action. */
export function storeStatusView(row: Pick<DomainRow, "status" | "reason" | "action">): DomainStatusView {
  const action = row.action?.type === "dns" ? "dns" : row.action?.type === "add_server" ? "servers" : null;
  switch (row.status) {
    case "ready":
      return { icon: <GlobeIcon />, phrase: row.reason, action };
    case "setting_up":
      return { icon: row.action?.type === "deploy" ? <GlobeIcon className="opacity-50" /> : <Spinner />, phrase: row.reason, action };
    case "needs_attention":
      return { icon: <AlertTriangleIcon className="text-warning" />, phrase: row.reason, action };
  }
}

function DnsRecords({ records }: { records: DnsRecord[] }) {
  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>Type</TableHead>
          <TableHead>Name</TableHead>
          <TableHead>Value</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {records.map((record) => (
          <TableRow key={`${record.type}-${record.value}`}>
            <TableCell>{record.type}</TableCell>
            <TableCell>
              {record.name}
              <CopyButton value={record.name} label={`Copy ${record.type} name`} size="icon-xs" />
            </TableCell>
            <TableCell>
              {record.value}
              <CopyButton value={record.value} label={`Copy ${record.type} value`} size="icon-xs" />
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  );
}

const LINK = buttonVariants({ variant: "link", size: "sm", className: "h-auto p-0" });

/** A public domain on a Service: the icon is its status, and one line says what's next. */
export function PublicDomainRow({
  organizationSlug,
  title,
  label,
  portLabel,
  view,
  dnsRecords = [],
  changed,
  pending = false,
  onEdit,
  onDelete,
}: {
  organizationSlug: string;
  /** The hostname, linked while live, or a placeholder while it has none. */
  title: ReactNode;
  /** Names the domain in the edit and remove buttons. */
  label: string;
  portLabel: string;
  view: DomainStatusView;
  /** The records that point the domain here; only custom domains have them. */
  dnsRecords?: DnsRecord[];
  changed: boolean;
  /** A write names this domain; the row waits for the Store's answer. */
  pending?: boolean;
  onEdit: () => void;
  onDelete: () => void;
}) {
  const [showDns, setShowDns] = useState(false);
  const action = view.action === "dns" && dnsRecords.length === 0 ? null : view.action;
  return (
    <SettingsListItem
      media={view.icon}
      label={<span aria-busy={pending} className={pending ? "opacity-60" : undefined}>{title}</span>}
      hint={
        <>
          → {portLabel}
          {view.phrase ? <> · {view.phrase}</> : null}
          {action ? " · " : null}
          {action === "dns" ? (
            <button type="button" className={LINK} onClick={() => setShowDns(!showDns)}>
              {showDns ? "Hide DNS records" : "Show DNS records"}
            </button>
          ) : null}
          {action === "organization_settings" ? (
            <Link to="/cloud/$organizationSlug/~/settings" params={{ organizationSlug }} className={LINK}>Organization › General</Link>
          ) : null}
          {action === "servers" ? (
            <Link to="/cloud/$organizationSlug/~/servers" params={{ organizationSlug }} className={LINK}>Add a server</Link>
          ) : null}
        </>
      }
      commit="change"
      actions={pending ? <PendingLabel /> : (
        <>
          <Button type="button" variant="ghost" size="icon-sm" aria-label={`Edit ${label}`} data-changed={changed || undefined} onClick={onEdit}>
            <PencilIcon />
          </Button>
          <Button type="button" variant="ghost" size="icon-sm" aria-label={`Remove ${label}`} onClick={onDelete}>
            <Trash2Icon />
          </Button>
        </>
      )}
    >
      {action === "dns" && showDns ? <DnsRecords records={dnsRecords} /> : null}
    </SettingsListItem>
  );
}
