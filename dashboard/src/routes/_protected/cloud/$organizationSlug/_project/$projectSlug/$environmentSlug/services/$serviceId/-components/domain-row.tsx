import { type ReactNode, useState } from "react";
import { Link } from "@tanstack/react-router";
import { AlertTriangleIcon, ArrowUpRightIcon, GlobeIcon, PencilIcon, Trash2Icon } from "lucide-react";
import { CopyButton } from "#/components/copy-button";
import { Button } from "#/components/ui/button";
import { buttonVariants } from "#/components/ui/button-variants";
import { Spinner } from "#/components/ui/spinner";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "#/components/ui/table";
import { cn } from "#/lib/utils";
import type { DnsRecord, PublicDomainStatus } from "#/modules/services/public-domain-status";
import { RelativeTime } from "#/components/relative-time";

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

export function DomainRowShell({
  icon,
  children,
  changed,
  actions,
}: {
  icon: ReactNode;
  children: ReactNode;
  changed?: boolean;
  actions: ReactNode;
}) {
  return (
    <div
      className={cn(
        "flex items-center gap-3 rounded-lg border bg-card p-3",
        changed && "border-changed-border bg-changed-soft"
      )}
    >
      <span className="text-muted-foreground">{icon}</span>
      <div className="min-w-0 flex-1">{children}</div>
      <div className="flex items-center gap-1">{actions}</div>
    </div>
  );
}

type StatusView = { icon: ReactNode; phrase: ReactNode; action: "dns" | "server_settings" | null };

/** The icon is the status; one short phrase and at most one link say what's next. */
function statusView(status: PublicDomainStatus): StatusView {
  const warning = <AlertTriangleIcon className="text-warning" />;
  switch (status.kind) {
    case "live":
      return { icon: <GlobeIcon />, phrase: status.viaProxy ? "via proxy" : null, action: null };
    case "unknown":
      return { icon: <GlobeIcon className="opacity-50" />, phrase: null, action: null };
    case "not_deployed":
      return { icon: <GlobeIcon className="opacity-50" />, phrase: "Live after your next deploy", action: null };
    case "setting_up":
      return { icon: <Spinner />, phrase: "Setting up", action: null };
    case "issuing":
      return { icon: <Spinner />, phrase: "Issuing certificate", action: null };
    case "needs_dns":
      return { icon: warning, phrase: "Waiting for DNS update", action: "dns" };
    case "dns_elsewhere":
      return { icon: warning, phrase: "Points to another server", action: "dns" };
    case "port_closed":
      return { icon: warning, phrase: "Port 80 is closed", action: null };
    case "redirects_to_https":
      // TODO: DOCS PAGE NEEDED on custom domains behind a proxy (see refusal_reason in
      // core/crates/ployz-core/src/domain/hostname_verdict.rs); link it from this line.
      return { icon: warning, phrase: "Your proxy redirects to HTTPS. Exempt /.well-known/acme-challenge/* from HTTPS redirects.", action: null };
    case "cert_failed":
      return {
        icon: warning,
        phrase: status.retryAt ? <>Certificate failed · retrying <RelativeTime date={status.retryAt} /></> : "Certificate failed",
        action: null,
      };
    case "unreachable":
      return { icon: warning, phrase: "Servers can’t receive traffic", action: "server_settings" };
    case "https_down":
      return { icon: warning, phrase: "HTTPS is down · we’re fixing it", action: null };
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

/** A public domain on a Service: the icon is its status, and one line says what's next. */
export function PublicDomainRow({
  organizationSlug,
  title,
  label,
  portLabel,
  status,
  dnsRecords = [],
  changed,
  onEdit,
  onDelete,
}: {
  organizationSlug: string;
  /** The hostname, linked while live, or a placeholder while it has none. */
  title: ReactNode;
  /** Names the domain in the edit and remove buttons. */
  label: string;
  portLabel: string;
  status: PublicDomainStatus;
  /** The records that point the domain here; only custom domains have them. */
  dnsRecords?: DnsRecord[];
  changed: boolean;
  onEdit: () => void;
  onDelete: () => void;
}) {
  const [showDns, setShowDns] = useState(false);
  const view = statusView(status);
  const action = view.action === "dns" && dnsRecords.length === 0 ? null : view.action;
  return (
    <div className="flex flex-col gap-2">
      <DomainRowShell
        changed={changed}
        icon={view.icon}
        actions={
          <>
            <Button type="button" variant="ghost" size="icon-sm" aria-label={`Edit ${label}`} onClick={onEdit}>
              <PencilIcon />
            </Button>
            <Button type="button" variant="ghost" size="icon-sm" aria-label={`Remove ${label}`} onClick={onDelete}>
              <Trash2Icon />
            </Button>
          </>
        }
      >
        {title}
        <div className="flex flex-wrap items-center gap-1 text-muted-foreground text-sm">
          <span>
            → {portLabel}
            {view.phrase ? <> · {view.phrase}</> : null}
          </span>
          {action ? <span>·</span> : null}
          {action === "dns" ? (
            <Button type="button" variant="link" size="sm" onClick={() => setShowDns(!showDns)}>
              {showDns ? "Hide DNS records" : "Show DNS records"}
            </Button>
          ) : null}
          {action === "server_settings" ? (
            <Link
              to="/cloud/$organizationSlug/~/settings"
              params={{ organizationSlug }}
              className={buttonVariants({ variant: "link", size: "sm" })}
            >
              Server Settings
            </Link>
          ) : null}
        </div>
      </DomainRowShell>
      {action === "dns" && showDns ? <DnsRecords records={dnsRecords} /> : null}
    </div>
  );
}
