import { useState, type ReactNode } from "react";
import { useBlocker } from "@tanstack/react-router";
import { PlusIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import type { ButtonVariants } from "#/components/ui/button-variants";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { useClusterName } from "#/modules/cluster-name/cluster-name.hooks";
import { useRoleGate } from "#/modules/organization/members.queries";
import { InstallCommand } from "./install-command";
import { ADD_SERVERS_REFUSED, useMintInstallLine } from "./use-install-line";

/** One Server's install line in a dialog. Each line adds one Server; run Add server again for the next. */
export function AddServerDialog({ organizationSlug, label = "Add server", variant = "ink", firstServer = false }: {
  organizationSlug: string;
  label?: ReactNode;
  variant?: ButtonVariants["variant"];
  /** No Server yet: the line founds the Cluster, with its choices under it. */
  firstServer?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const mint = useMintInstallLine(organizationSlug);
  const gate = useRoleGate(organizationSlug, "servers:add", ADD_SERVERS_REFUSED);
  useBlocker({ shouldBlockFn: () => true, disabled: !mint.holding });
  return (
    <>
      <Button type="button" variant={variant} disabled={!gate.allowed} title={gate.reason} onClick={() => { mint.reset(); setOpen(true); }}>
        <PlusIcon data-icon="inline-start" />
        {label}
      </Button>
      <Dialog open={open} onOpenChange={(next) => mint.holding || setOpen(next)}>
        <DialogContent className="sm:max-w-xl">
          <DialogHeader>
            <DialogTitle>{firstServer ? "Add your first server" : "Add a server"}</DialogTitle>
            <DialogDescription>Run one command as root on any Linux server. It installs Ployz, and Docker if it's missing.</DialogDescription>
          </DialogHeader>
          {open && firstServer ? <FoundingCommand organizationSlug={organizationSlug} mint={mint} /> : null}
          {open && !firstServer ? <InstallCommand organizationSlug={organizationSlug} cluster={null} mint={mint} steps={false} /> : null}
        </DialogContent>
      </Dialog>
    </>
  );
}

function FoundingCommand({ organizationSlug, mint }: { organizationSlug: string; mint: ReturnType<typeof useMintInstallLine> }) {
  return <InstallCommand organizationSlug={organizationSlug} cluster={useClusterName(organizationSlug)} mint={mint} steps={false} />;
}
