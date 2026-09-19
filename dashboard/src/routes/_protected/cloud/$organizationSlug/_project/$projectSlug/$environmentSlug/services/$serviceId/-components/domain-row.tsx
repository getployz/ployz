import type { ReactNode } from "react";
import { GlobeIcon, PencilIcon, Trash2Icon } from "lucide-react";
import { CopyButton } from "#/components/copy-button";
import { Button } from "#/components/ui/button";
import { FieldDescription } from "#/components/ui/field";
import { cn } from "#/lib/utils";
import type { ServiceRoute } from "#/modules/environment-design/tables";

export type DomainCertificateEvidence = {
  status: string | null;
  lastObserved: boolean;
  incomplete: boolean;
} | null;

export function DomainTitle({
  hostname,
  copyLabel,
}: {
  hostname: string;
  copyLabel: string;
}) {
  return (
    <div className="flex min-w-0 items-center gap-1">
      <span className="truncate font-mono text-sm">{hostname}</span>
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

export function CertificateEvidence({
  evidence,
}: {
  evidence: DomainCertificateEvidence;
}) {
  if (!evidence) return null;
  return (
    <FieldDescription>
      {evidence.status
        ? `${
            evidence.lastObserved ? "Last observed" : "Observed"
          } certificate status: ${evidence.status.replaceAll("_", " ")}.`
        : "Certificate status was not observed."}
      {evidence.incomplete
        ? " This observation also lists this certificate as incomplete."
        : null}
    </FieldDescription>
  );
}

export function CustomDomainRow({
  route,
  defaultTargetPort,
  certificateEvidence,
  changed,
  onEdit,
  onDelete,
}: {
  route: ServiceRoute;
  defaultTargetPort: number | null;
  certificateEvidence: DomainCertificateEvidence;
  changed: boolean;
  onEdit: () => void;
  onDelete: () => void;
}) {
  return (
    <div className="flex flex-col gap-1">
      <DomainRowShell
        changed={changed}
        icon={<GlobeIcon />}
        actions={
          <>
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              aria-label={`Edit ${route.hostname}`}
              onClick={onEdit}
            >
              <PencilIcon />
            </Button>
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              aria-label={`Remove ${route.hostname}`}
              onClick={onDelete}
            >
              <Trash2Icon />
            </Button>
          </>
        }
      >
        <DomainTitle
          hostname={route.hostname}
          copyLabel={`Copy ${route.hostname}`}
        />
        <div className="text-muted-foreground text-sm">
          →{" "}
          {route.targetPort === null && defaultTargetPort === null
            ? "Uses PORT"
            : `Port ${route.targetPort ?? defaultTargetPort}`}
        </div>
      </DomainRowShell>
      <CertificateEvidence evidence={certificateEvidence} />
    </div>
  );
}
