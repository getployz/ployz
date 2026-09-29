"use client";

import { useEffect, useState } from "react";
import { Link } from "@tanstack/react-router";
import { Trash2Icon } from "lucide-react";
import type { EnvironmentListing } from "@ployz/sdk";
import { DeletionDialog, type DeletionCheck, type DeletionItem } from "#/components/deletion-dialog";
import { Alert, AlertAction, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Button } from "#/components/ui/button";
import { Spinner } from "#/components/ui/spinner";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { toErrorMessage } from "#/lib/error-message";
import { removalsQuery, requireView, servicesQuery, storeViewOptions } from "#/modules/config-store/store-view.queries";
import { StoreRefused, useStoreWriter } from "#/modules/config-store/store-write";
import { removing, teardownStep, type AcceptedLoss, type TeardownStep, type TeardownTarget } from "#/modules/config-store/store-workspace";
import type { ConfigQuery } from "#/modules/config-store/store.contract";
import { DEPLOYMENT_PAGE_ROUTE_TO } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/deployment-page";
import { getServiceIcon } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/service-node-helpers";
import { DangerRow } from "./danger-row";

type Waiting = Extract<TeardownStep, { done: false }>;

/**
 * Deletes a Config Store Environment or Project through the one teardown path: the dialog lists every Service and Volume
 * that goes, by Environment, and asks for `place`. What never ran on the Servers goes at once; the rest comes off them
 * first, one removal Deployment per Environment, and this tab carries on once each applied. A removal that didn't
 * finish shows here, with its Deployment, until it's tried again.
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
  const [run, setRun] = useState<{ accepted: AcceptedLoss; waiting: Waiting } | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const inScope = target.environment === null ? environments : environments.filter((row) => row.name === target.environment);
  const commit = (command: Parameters<typeof writer.commit>[0], handles: readonly string[]) =>
    writer.commit(command, handles).isPersisted.promise;

  async function advance(accepted: AcceptedLoss, at: TeardownTarget = target) {
    const step = await teardownStep(commit, at, accepted);
    if (!step.done) setRun({ accepted, waiting: step });
    else if (at === target) onCompleted();
    else setRun(null);
  }

  // The removal this tab waits on applied: carry on with what its user confirmed.
  const awaited = run ? environments.find((row) => row.name === run.waiting.environment)?.removal : undefined;
  const applied = run !== null && awaited?.id === run.waiting.deployment && awaited.status === "applied";
  useEffect(() => {
    if (!applied || !run) return;
    advance(run.accepted).catch((error: Error) => {
      setRun(null);
      setFailure(toErrorMessage(error, "Couldn't finish deleting."));
    });
  }, [applied]);

  async function load(): Promise<DeletionCheck<AcceptedLoss>> {
    const read = <Q extends ConfigQuery>(query: Q) =>
      scope.queryClient.fetchQuery({ ...storeViewOptions(organizationSlug, scope, query), staleTime: 0 }).then(requireView);
    const each = await Promise.all(inScope.map(async (environment) => {
      const ref = { project: target.project, environment: environment.name };
      const [services, removals] = await Promise.all([read(servicesQuery(ref)), read(removalsQuery(ref))]);
      return { environment, services: services.services, volumes: removals.volumes };
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
        ({ kind: "volume", name: volume.name, detail: detail(environment) }))),
    ];
    return { items, evidence: Object.fromEntries(each.map(({ environment, volumes }) => [environment.name, volumes.map((volume) => volume.name)])) };
  }

  async function confirm(accepted: AcceptedLoss) {
    setFailure(null);
    try {
      await advance(accepted);
    } catch (error) {
      // The Volumes deployed changed since the dialog read them: it shows them again, to be typed for again.
      if (error instanceof StoreRefused && (error.code === "confirmation_required" || error.code === "invalid_argument")) return load();
      throw error;
    }
  }

  function finish(environment: EnvironmentListing) {
    // The Default Environment comes off its Servers last, when its whole Project goes.
    const at = environment.default ? { project: target.project, environment: null } : { project: target.project, environment: environment.name };
    const same = at.environment === target.environment;
    advance({}, same ? target : at).catch((error: Error) => setFailure(toErrorMessage(error, "Couldn't finish deleting.")));
  }

  const busy = run !== null || inScope.some(removing);
  const params = (environment: string) => ({ organizationSlug, projectSlug: target.project, environmentSlug: environment });

  return (
    <>
      <section aria-labelledby={headingId}>
        <h2 id={headingId} className="text-lg font-semibold text-destructive">Danger</h2>
        <div className="mt-4 flex flex-col gap-4">
          {failure && <Alert variant="destructive"><AlertTitle>Deleting {name} didn't finish</AlertTitle><AlertDescription>{failure}</AlertDescription></Alert>}
          {inScope.flatMap((environment) => {
            const removal = environment.removal;
            if (!removal) return [];
            const deployment = <Link to={DEPLOYMENT_PAGE_ROUTE_TO} params={{ ...params(environment.name), deploymentId: removal.id }}
              className="underline underline-offset-4">Deployment #{removal.number}</Link>;
            const waited = run?.waiting.deployment === removal.id;
            if (removing(environment) || (waited && removal.status === "applied")) return [
              <Alert key={environment.id}>
                <Spinner />
                <AlertTitle>Deleting {environment.name}…</AlertTitle>
                <AlertDescription><span>{deployment} takes it off your servers.</span></AlertDescription>
              </Alert>,
            ];
            if (removal.status === "applied") return [
              <Alert key={environment.id}>
                <AlertTitle>{environment.name} is off your servers</AlertTitle>
                <AlertDescription><span>{deployment} removed it. Nothing is left to lose: finish deleting it.</span></AlertDescription>
                <AlertAction><Button variant="outline" size="sm" disabled={run !== null} onClick={() => finish(environment)}>Finish deleting</Button></AlertAction>
              </Alert>,
            ];
            return [
              <Alert key={environment.id} variant="destructive">
                <AlertTitle>Deleting {environment.name} didn't finish</AlertTitle>
                <AlertDescription><span>{deployment} {removal.status === "unknown" ? "may not have ended" : `was ${removal.status}`}. Some of it may still be on your servers.</span></AlertDescription>
                <AlertAction><Button variant="outline" size="sm" disabled={busy} onClick={() => setOpen(true)}>Try again</Button></AlertAction>
              </Alert>,
            ];
          })}
          <DangerRow
            title={title}
            description={description}
            action={
              <Button variant="destructive" className="shrink-0" disabled={busy || disabledReason !== undefined} onClick={() => setOpen(true)}>
                <Trash2Icon data-icon="inline-start" />
                {actionLabel}
              </Button>
            }
          >
            {disabledReason && <p className="mt-1 text-sm text-muted-foreground">{disabledReason}</p>}
          </DangerRow>
        </div>
      </section>
      <DeletionDialog open={open} onOpenChange={setOpen} title={`Delete ${name}?`} place={place} confirmLabel="Delete"
        callbacks={{ load, confirm }} />
    </>
  );
}
