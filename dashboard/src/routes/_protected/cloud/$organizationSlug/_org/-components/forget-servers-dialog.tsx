"use client";

import { useServerFn } from "@tanstack/react-start";
import { toast } from "sonner";
import { DeletionDialog, type DeletionItem } from "#/components/deletion-dialog";
import { forgetServersServerFn, previewForgetServersServerFn } from "#/modules/machines/forget-servers.functions";

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
    toast.success("Your Servers are forgotten. Add a server to deploy again.");
  }

  return (
    <DeletionDialog open={open} onOpenChange={onOpenChange} title="Forget all Servers?" place={organizationSlug}
      confirmLabel="Forget Servers"
      sentence={<>Use this when your Servers were deleted. Your projects and settings stay. Volume data on these Servers
        can't be recovered.</>}
      callbacks={{ load, confirm }} />
  );
}
