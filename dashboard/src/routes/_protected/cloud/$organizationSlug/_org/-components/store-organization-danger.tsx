"use client";

import { useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { useServerFn } from "@tanstack/react-start";
import { Trash2Icon } from "lucide-react";
import { DeletionDialog } from "#/components/deletion-dialog";
import { Alert, AlertAction, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Button } from "#/components/ui/button";
import { toErrorMessage } from "#/lib/error-message";
import { projectsQuery, requireView, useStoreView } from "#/modules/config-store/store-view.queries";
import { removeOrganizationServerFn } from "#/modules/organization/organization-removal.functions";
import { DangerRow } from "../../-components/danger-row";

/**
 * Delete organization over the Config Store, as `ployz org rm`: once it has no Project, whose Environments all left the
 * Servers through the one teardown path. Every Server is unpaired; one that doesn't confirm keeps the Organization,
 * disabled, until trying again confirms it.
 */
export function StoreOrganizationDanger({ organizationSlug }: { organizationSlug: string }) {
  const navigate = useNavigate();
  const remove = useServerFn(removeOrganizationServerFn);
  const { projects } = requireView(useStoreView(organizationSlug, projectsQuery()));
  const [open, setOpen] = useState(false);
  const [unconfirmed, setUnconfirmed] = useState<readonly string[] | null>(null);
  const [retrying, setRetrying] = useState(false);

  async function removeOrganization() {
    const result = await remove({ data: { organizationSlug } });
    if (!result.ok) throw new Error(result.refusal.message);
    if (result.value.removed) {
      void navigate({ to: "/cloud", replace: true });
      return;
    }
    setUnconfirmed(result.value.servers.unconfirmed);
  }

  async function retry() {
    setRetrying(true);
    try {
      await removeOrganization();
    } catch (error) {
      setUnconfirmed([toErrorMessage(error, "Couldn't try again.")]);
    } finally {
      setRetrying(false);
    }
  }

  const disabledReason = projects.length > 0
    ? `Delete its projects first: ${projects.map((project) => project.name).join(", ")}.`
    : undefined;

  return (
    <>
      <section aria-labelledby="organization-teardown-heading">
        <h2 id="organization-teardown-heading" className="text-lg font-semibold text-destructive">Danger</h2>
        <div className="mt-4 flex flex-col gap-4">
          {unconfirmed && (
            <Alert variant="destructive">
              <AlertTitle>Deleting {organizationSlug} didn't finish</AlertTitle>
              <AlertDescription>
                <span>Some servers didn't confirm they let go of it ({unconfirmed.join(", ")}). It stays, disabled, until they do.</span>
              </AlertDescription>
              <AlertAction><Button variant="outline" size="sm" disabled={retrying} onClick={() => void retry()}>Try again</Button></AlertAction>
            </Alert>
          )}
          <DangerRow
            title="Delete this organization"
            description="Its servers are unpaired from it."
            action={
              <Button variant="destructive" className="shrink-0" disabled={disabledReason !== undefined} onClick={() => setOpen(true)}>
                <Trash2Icon data-icon="inline-start" />
                Delete organization
              </Button>
            }
          >
            {disabledReason && <p className="mt-1 text-sm text-muted-foreground">{disabledReason}</p>}
          </DangerRow>
        </div>
      </section>
      <DeletionDialog open={open} onOpenChange={setOpen} title={`Delete ${organizationSlug}?`} place={organizationSlug}
        confirmLabel="Delete" sentence={<>You're <span className="text-destructive">deleting</span> <span className="text-foreground">{organizationSlug}</span>, and unpairing its servers from it.</>}
        callbacks={{ load: () => Promise.resolve({ items: [], evidence: null }), confirm: removeOrganization }} />
    </>
  );
}
