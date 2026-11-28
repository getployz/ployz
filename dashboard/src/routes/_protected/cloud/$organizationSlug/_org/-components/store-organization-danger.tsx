"use client";

import { refusalFor } from "#/modules/organization/permissions";
import { useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { useServerFn } from "@tanstack/react-start";
import { ServerOffIcon, Trash2Icon } from "lucide-react";
import { DeletionDialog } from "#/components/deletion-dialog";
import { type DeletionItem } from "#/modules/config-store/deletion";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { Button } from "#/components/ui/button";
import { toErrorMessage } from "#/lib/error-message";
import { projectsQuery, requireView, useStoreView } from "#/modules/config-store/store-view.queries";
import { useRoleGate } from "#/modules/organization/members.queries";
import { removeOrganizationServerFn } from "#/modules/organization-teardown/organization-removal.functions";
import { SettingsDanger, SettingsRow, SettingsSection } from "#/components/settings/settings-section";
import { ForgetServersDialog, useForgetServersGate } from "./forget-servers-dialog";

/**
 * Delete organization over the Config Store, as `ployz org rm`: once it has no Project, whose Environments all left the
 * Servers through the one teardown path. Every Server is unpaired; one that doesn't confirm keeps the Organization,
 * disabled, until trying again confirms it. Forget Servers sits beside it, for Servers that were deleted.
 */
/** Where a deletion that some Servers didn't confirm stands. Each state keeps the Servers still unconfirmed. */
type Teardown =
  | { readonly at: "none" }
  | { readonly at: "unconfirmed"; readonly servers: readonly string[] }
  | { readonly at: "retrying"; readonly servers: readonly string[] }
  | { readonly at: "retry_failed"; readonly servers: readonly string[]; readonly message: string };

export function StoreOrganizationDanger({ organizationSlug }: { organizationSlug: string }) {
  const navigate = useNavigate();
  const remove = useServerFn(removeOrganizationServerFn);
  const { projects } = requireView(useStoreView(organizationSlug, projectsQuery()));
  // With no Project left, what goes is its hold on each Server.
  const { machines } = useRuntimeLens(organizationSlug);
  const servers = machines.map((machine): DeletionItem => ({ kind: "server", name: machine.name, detail: "unpaired" }));
  const [dialog, setDialog] = useState<"delete" | "forget" | null>(null);
  const [teardown, setTeardown] = useState<Teardown>({ at: "none" });

  async function removeOrganization() {
    const result = await remove({ data: { organizationSlug } });
    if (!result.ok) throw new Error(result.refusal.message);
    if (result.value.removed) {
      // A fresh session: it acts in no Organization now.
      void navigate({ to: "/cloud", replace: true, reloadDocument: true });
      return;
    }
    setTeardown({ at: "unconfirmed", servers: result.value.servers.unconfirmed });
  }

  async function retry(servers: readonly string[]) {
    setTeardown({ at: "retrying", servers });
    try {
      await removeOrganization();
    } catch (error) {
      setTeardown({ at: "retry_failed", servers, message: toErrorMessage(error, "Couldn't try again.") });
    }
  }

  const forget = useForgetServersGate(organizationSlug);
  const mayDelete = useRoleGate(organizationSlug, "org:delete", refusalFor("org:delete", "delete it"));
  const disabledReason = mayDelete.reason ?? (projects.length > 0
    ? `Delete its projects first: ${projects.map((project) => project.name).join(", ")}.`
    : undefined);

  return (
    <>
      <SettingsSection id="organization-teardown" title="Danger">
        {teardown.at === "none" ? null : (
          <SettingsRow
            label={`Deleting ${organizationSlug} didn't finish`}
            hint={`Some servers didn't confirm they let go of it (${teardown.servers.join(", ")}). It stays, disabled, until they do.`}
            commit="change"
            control={<Button variant="outline" size="sm" disabled={teardown.at === "retrying"} onClick={() => void retry(teardown.servers)}>Try again</Button>}
          >
            {teardown.at === "retry_failed" ? <p role="alert" className="text-sm text-destructive">{teardown.message}</p> : null}
          </SettingsRow>
        )}
        <SettingsDanger
          label="Forget all servers"
          hint={`Use this when your servers were deleted. Your projects and settings stay.${forget.reason ? ` ${forget.reason}` : ""}`}
          action={
            <Button variant="destructive" disabled={!forget.allowed} onClick={() => setDialog("forget")}>
              <ServerOffIcon data-icon="inline-start" />
              Forget servers
            </Button>
          }
        />
        <SettingsDanger
          label="Delete this organization"
          hint={disabledReason ? `Its servers are unpaired from it. ${disabledReason}` : "Its servers are unpaired from it."}
          action={
            <Button variant="destructive" disabled={!mayDelete.allowed || disabledReason !== undefined} onClick={() => setDialog("delete")}>
              <Trash2Icon data-icon="inline-start" />
              Delete organization
            </Button>
          }
        />
      </SettingsSection>
      <DeletionDialog open={dialog === "delete"} onOpenChange={(open) => setDialog(open ? "delete" : null)} title={`Delete ${organizationSlug}?`} place={organizationSlug}
        confirmLabel="Delete" sentence={() => <>You're <span className="text-destructive">deleting</span> <span className="text-foreground">{organizationSlug}</span>, and unpairing its servers from it.</>}
        callbacks={{ load: () => Promise.resolve({ items: servers, evidence: null }), confirm: removeOrganization }} />
      <ForgetServersDialog organizationSlug={organizationSlug} open={dialog === "forget"} onOpenChange={(open) => setDialog(open ? "forget" : null)} />
    </>
  );
}
