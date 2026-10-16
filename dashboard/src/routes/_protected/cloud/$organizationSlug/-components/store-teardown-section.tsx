"use client";

import { useEffect, useState } from "react";
import { Link } from "@tanstack/react-router";
import { Trash2Icon } from "lucide-react";
import type { DeploymentStatus, EnvironmentListing, Removal } from "@ployz/sdk";
import { DeletionDialog, type DeletionCheck, type DeletionItem } from "#/components/deletion-dialog";
import { SettingsDanger, SettingsRow, SettingsSection } from "#/components/settings/settings-section";
import { Button } from "#/components/ui/button";
import { Spinner } from "#/components/ui/spinner";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { fetchStoreView, removalsQuery, servicesQuery, volumesQuery } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { removeTarget, removing, type RemovalConsent, type TeardownTarget } from "#/modules/config-store/store-workspace";
import type { ConfigQuery } from "#/modules/config-store/store.contract";
import { deploymentDestination } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/deployments/run-search";
import { getServiceIcon } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/service-node-helpers";

/** How a removal that didn't apply ended, after "Deployment #N". */
function ended(status: DeploymentStatus) {
  if (status === "failed") return "failed";
  return status === "unknown" ? "may not have ended" : `was ${status}`;
}

/**
 * Deletes a Config Store Environment or Project in one `remove`: the dialog lists every Service and Volume that goes,
 * by Environment, and asks for `place`. What never ran on the Servers goes at once; the Store takes the rest off them,
 * one removal Deployment per Environment, and deletes it once each applied, with or without this tab. A removal that
 * didn't finish shows here, with its Deployment, until it's tried again.
 */
export function StoreTeardownSection({ organizationSlug, target, environments, name, place, title, description, actionLabel,
  disabledReason, headingId, onCompleted }: {
  organizationSlug: string;
  target: TeardownTarget;
  /** The Project's Environments, as the Store lists them now. */
  environments: readonly EnvironmentListing[];
  name: string;
  place: string;
  title: string;
  description: string;
  actionLabel: string;
  disabledReason?: string;
  headingId: string;
  onCompleted: () => void;
}) {
  const scope = useCollectionScope();
  const writer = useStoreWriter(organizationSlug);
  const [open, setOpen] = useState(false);
  const [left, setLeft] = useState<Extract<Removal, { removal: "removing" }> | null>(null);
  const inScope = target.environment === null ? environments : environments.filter((row) => row.name === target.environment);
  const commit = (command: Parameters<typeof writer.commit>[0], handles: readonly string[]) =>
    writer.commit(command, handles).isPersisted.promise;

  // The Store deleted the Environment this tab is removing.
  const gone = left !== null && target.environment !== null && inScope.length === 0;
  useEffect(() => {
    if (gone) onCompleted();
  }, [gone]);

  async function load(): Promise<DeletionCheck<RemovalConsent>> {
    const read = <Q extends ConfigQuery>(query: Q) => fetchStoreView(organizationSlug, scope, query);
    const each = await Promise.all(inScope.map(async (environment) => {
      const ref = { project: target.project, environment: environment.name };
      const [services, volumes, removals] = await Promise.all([read(servicesQuery(ref)), read(volumesQuery(ref)), read(removalsQuery(ref))]);
      // What's authored goes, and what's deployed loses its data: the one the user accepts losing, by name.
      const names = new Set([...volumes.volumes.map((volume) => volume.name), ...removals.volumes.map((volume) => volume.name)]);
      return { environment, services: services.services, volumes: [...names], deployed: removals.volumes.map((volume) => volume.name) };
    }));
    // Past one Environment, each thing says where it is.
    const detail = (environment: EnvironmentListing) => target.environment === null ? environment.name : undefined;
    const items: DeletionItem[] = [
      ...target.environment === null
        ? each.map(({ environment }): DeletionItem => ({ kind: environment.parent ? "branch" : "environment", name: environment.name }))
        : [],
      ...each.flatMap(({ environment, services }) => services.map((service): DeletionItem =>
        ({ kind: "service", name: service.name, icon: getServiceIcon({ source: { type: service.source } }), detail: detail(environment) }))),
      ...each.flatMap(({ environment, volumes }) => volumes.map((volume): DeletionItem =>
        ({ kind: "volume", name: volume, detail: detail(environment) }))),
    ];
    const accept = Object.fromEntries(each.map(({ environment, deployed }) => [environment.name, deployed]));
    return { items, evidence: { accept, version: null, everyCopy: {} } };
  }

  async function confirm(consent: RemovalConsent) {
    const answer = await removeTarget(commit, target, consent);
    if ("ask" in answer) {
      // The Store found more to lose than the dialog showed, or Servers it couldn't check: shown again, to be typed for again.
      const { items } = await load();
      const servers = answer.waitingOn.map((waiting): DeletionItem => ({ kind: "server", name: waiting.server, detail: waiting.why }));
      return { items: [...items, ...servers], evidence: answer.ask };
    }
    if (answer.sent.removal === "removed") onCompleted();
    else setLeft(answer.sent);
  }

  const busy = inScope.some(removing);
  const params = (environment: string) => ({ organizationSlug, projectSlug: target.project, environmentSlug: environment });

  return (
    <>
      <SettingsSection id={headingId} title="Danger">
          {left?.waiting_on.map((waiting) => (
            <SettingsRow key={waiting.server} label={`Waiting on ${waiting.server}`} hint={waiting.why} commit="none" />
          ))}
          {inScope.flatMap((environment) => {
            const removal = environment.removal;
            if (!removal) return [];
            const deployment = <Link {...deploymentDestination(params(environment.name), removal.id)}
              className="underline underline-offset-4">Deployment #{removal.number}</Link>;
            // Applied, while the Store is deleting it: the next sweep finishes it.
            if (removing(environment) || (left !== null && removal.status === "applied")) return [
              <SettingsRow
                key={environment.id}
                label={<><Spinner />Deleting {environment.name}…</>}
                hint={<>{deployment} takes it off your servers.</>}
                commit="none"
              />,
            ];
            if (removal.status === "applied") return [
              <SettingsRow
                key={environment.id}
                label={`${environment.name} is off your servers`}
                hint={<>{deployment} removed it. Nothing is left to lose: finish deleting it.</>}
                commit="none"
                control={<Button variant="outline" onClick={() => setOpen(true)}>Finish deleting</Button>}
              />,
            ];
            return [
              <SettingsRow
                key={environment.id}
                label={`Deleting ${environment.name} didn't finish`}
                hint={<span role="alert" className="text-destructive">{deployment} {ended(removal.status)}. Some of it may still be on your servers.</span>}
                commit="none"
                control={<Button variant="outline" disabled={busy} onClick={() => setOpen(true)}>Try again</Button>}
              />,
            ];
          })}
          <SettingsDanger
            label={title}
            hint={description}
            action={
              <Button variant="destructive" disabled={busy || disabledReason !== undefined} onClick={() => setOpen(true)}>
                <Trash2Icon data-icon="inline-start" />
                {actionLabel}
              </Button>
            }
          >
            {disabledReason && <p className="text-sm text-muted-foreground">{disabledReason}</p>}
          </SettingsDanger>
      </SettingsSection>
      <DeletionDialog open={open} onOpenChange={setOpen} title={`Delete ${name}?`} place={place} confirmLabel="Delete"
        callbacks={{ load, confirm }} />
    </>
  );
}
