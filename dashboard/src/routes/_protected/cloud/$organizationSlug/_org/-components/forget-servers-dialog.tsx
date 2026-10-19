"use client";

import { useServerFn } from "@tanstack/react-start";
import { toast } from "sonner";
import { DeletionDialog, type DeletionItem } from "#/components/deletion-dialog";
import { forgetServersServerFn, previewForgetServersServerFn } from "#/modules/machines/forget-servers.functions";
import { useYouMay } from "#/modules/organization/members.queries";

/** Whether you may forget the Servers, and why not once your role says so. Closed while your role is unknown. */
export function useForgetServersGate(organizationSlug: string) {
  const may = useYouMay(organizationSlug, "servers:remove");
  return { allowed: may === true, refusal: may === false ? "Only an owner or admin can forget them." : undefined };
}

/**
 * Forget Servers, for Servers that were deleted: Cloud tries each as the dialog opens and again on confirm, lists what
 * goes by what it observed, and forgets the Cluster only when none answers. Projects and settings stay.
 */
export function ForgetServersDialog({ organizationSlug, open, onOpenChange }: {
  organizationSlug: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const preview = useServerFn(previewForgetServersServerFn);
  const forget = useServerFn(forgetServersServerFn);

  async function load() {
    const checked = await preview({ data: { organizationSlug } });
    if (!checked.ok) throw new Error(checked.refusal.message);
    // Each Server with what Cloud saw when it tried, and each Volume a Deploy put on them.
    const items: DeletionItem[] = [
      ...checked.value.servers.map((server): DeletionItem => ({
        kind: "server", name: server.name, detail: server.reach === "no_connection" ? "no connection to try" : "didn't answer",
      })),
      ...checked.value.volumes.map((volume): DeletionItem => ({
        kind: "volume", name: volume.volume, detail: `${volume.project}/${volume.environment}`,
      })),
    ];
    return { items, evidence: null };
  }

  async function confirm() {
    const forgotten = await forget({ data: { organizationSlug } });
    if (!forgotten.ok) throw new Error(forgotten.refusal.message);
    toast.success("Your servers are forgotten. Add a server to deploy again.");
  }

  return (
    <DeletionDialog open={open} onOpenChange={onOpenChange} title="Forget all servers?" place={organizationSlug}
      confirmLabel="Forget servers"
      sentence={<>Use this when your servers were deleted. Your projects and settings stay. Volume data on these servers
        can't be recovered.</>}
      callbacks={{ load, confirm }} />
  );
}
