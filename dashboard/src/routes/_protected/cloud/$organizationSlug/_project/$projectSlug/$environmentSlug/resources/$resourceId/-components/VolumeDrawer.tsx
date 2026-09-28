import { useEnvironmentDocumentEditor } from "#/modules/environment-design/environment-document-edit";
import { useRef, useState } from "react";
import { Trash2Icon } from "lucide-react";
import { toast } from "sonner";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useServerFn } from "@tanstack/react-start";
import { DeletionDialog, type DeletionItem } from "#/components/deletion-dialog";
import { Alert, AlertAction, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Button } from "#/components/ui/button";
import { Separator } from "#/components/ui/separator";
import { Spinner } from "#/components/ui/spinner";
import { toErrorMessage } from "#/lib/error-message";
import { createEnvironmentNodeNameSchema } from "#/modules/environment-design/environment-node-names";
import type { DataLossList } from "#/modules/runtime/data-loss-confirm";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { environmentDesignFields } from "#/modules/environment-design/fields";
import {
  deleteVolumeResourceServerFn,
  updateVolumeResourceServerFn,
} from "#/modules/environment-design/resource-functions";
import {
  confirmVolumeRemoveServerFn,
  loadVolumeRemoveDataLossServerFn,
  retryVolumeRemoveServerFn,
} from "#/modules/runtime/volume-removal.functions";
import {
  latestVolumeRemoveAttemptQueryOptions,
  rememberLatestVolumeRemoveAttempt,
} from "#/modules/runtime/volume-removal.queries";
import {
  volumeRemoveIsBusy,
  volumeRemoveIsRetryable,
  type VolumeRemoveAttemptStatus,
} from "#/modules/runtime/volume-removal";
import { CanvasInspectorHeader } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorHeader";
import { CanvasInspectorNameEditor } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorNameEditor";
import { VolumeAttachmentsTab } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/resources/$resourceId/-components/VolumeAttachmentsTab";
import type {
  VolumeDrawerState,
  VolumeResourceRouteParams,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/resources/$resourceId/-components/useVolumeDrawerState";
import { ENVIRONMENT_INDEX_ROUTE_TO } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/environment-route-paths";
import { useEnvironmentPlace } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/deletion-items";

const resourceNameSchema = environmentDesignFields.resource.name;

export function VolumeDrawer({
  params,
  state,
}: {
  params: VolumeResourceRouteParams;
  state: VolumeDrawerState;
}) {
  const navigate = useNavigate();
  const editDocument = useEnvironmentDocumentEditor(state.organizationSlug);
  const deleteVolume = useServerFn(deleteVolumeResourceServerFn);
  const updateVolume = useServerFn(updateVolumeResourceServerFn);
  const resourceId = state.resource.resource.id;
  const isRemoved = !state.resource.isAuthored;
  const mountedCount = state.attachments.filter(
    (attachment) => attachment.volumeResourceId === resourceId,
  ).length;
  const nameSchema = createEnvironmentNodeNameSchema({
    schema: resourceNameSchema,
    nodes: state.environmentNodes,
    excludeNode: { type: "volume", id: resourceId },
  });

  function handleDelete() {
    const { environmentId, organizationSlug } = state;
    // Deleting only stages the removal, so the drawer closes at once and a failed save rolls it back.
    editDocument({
      environmentId,
      apply: (intent) => {
        intent.volumes = intent.volumes.filter((volume) => volume.resourceId !== resourceId);
        for (const service of intent.services) {
          service.volumeAttachments = service.volumeAttachments.filter((attachment) => attachment.volumeResourceId !== resourceId);
        }
      },
      save: (revision) => deleteVolume({ data: { organizationSlug, environmentId, revision, resourceId } }),
      failureMessage: "The volume couldn’t be deleted. Try again.",
    });
    void navigate({
      to: ENVIRONMENT_INDEX_ROUTE_TO,
      params: {
        organizationSlug: params.organizationSlug,
        projectSlug: params.projectSlug,
        environmentSlug: params.environmentSlug,
      },
      search: (prev) => prev,
    });
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      <CanvasInspectorHeader params={params}>
        <CanvasInspectorNameEditor
          value={state.resource.resource.name}
          schema={nameSchema}
          editTitle="Edit volume name"
          editDescription="Rename this volume."
          placeholder="Volume name"
          onRename={(name) => {
            const { environmentId, organizationSlug } = state;
            editDocument({
              environmentId,
              apply: (intent) => {
                const volume = intent.volumes.find((volume) => volume.resourceId === resourceId);
                if (volume) volume.name = name;
              },
              save: (revision) => updateVolume({ data: { organizationSlug, environmentId, revision, resourceId, name } }),
              failureMessage: "Could not rename this volume.",
            });
          }}
        />
        <p className="truncate text-sm text-muted-foreground">Named volume</p>
      </CanvasInspectorHeader>
      <div className="min-h-0 flex-1 overflow-y-auto px-6 py-6">
        <section aria-labelledby="volume-mounts-heading">
          <h2 id="volume-mounts-heading" className="text-lg font-semibold">
            Mounts
          </h2>
          <div className="mt-4">
            <VolumeAttachmentsTab state={state} />
          </div>
        </section>
        {isRemoved ? (
          <VolumeRemoveDanger state={state} />
        ) : (
          <>
            <div className="py-8">
              <Separator />
            </div>
            <section aria-labelledby="volume-danger-heading">
              <h2
                id="volume-danger-heading"
                className="text-lg font-semibold text-destructive"
              >
                Danger
              </h2>
              {/* Deleting is staged: the Review lists it, Deploy asks if it holds data, and Discard undoes it. */}
              <div className="mt-4 flex flex-col items-start justify-between gap-4 rounded-xl border border-destructive-border bg-destructive-soft p-4 sm:flex-row sm:items-center">
                <div className="min-w-0">
                  <div className="text-sm font-semibold text-destructive">
                    Delete this volume
                  </div>
                  <p className="mt-1 text-sm text-destructive/85">
                    {mountedCount > 0
                      ? `Deleted on your next deploy, with its ${mountedCount} mount${mountedCount === 1 ? "" : "s"}.`
                      : "Deleted on your next deploy."}
                  </p>
                </div>
                <Button variant="destructive" className="shrink-0" onClick={handleDelete}>
                  <Trash2Icon data-icon="inline-start" />
                  Delete volume
                </Button>
              </div>
            </section>
          </>
        )}
      </div>
    </div>
  );
}

type VolumeRemoveAttemptSummary = {
  id: string;
  status: VolumeRemoveAttemptStatus;
  failureMessage: string | null;
};

function VolumeRemoveDanger({ state }: { state: VolumeDrawerState }) {
  const resourceId = state.resource.resource.id;
  const loadDataLoss = useServerFn(loadVolumeRemoveDataLossServerFn);
  const confirmRemove = useServerFn(confirmVolumeRemoveServerFn);
  const retryRemove = useServerFn(retryVolumeRemoveServerFn);
  const queryClient = useQueryClient();
  const [open, setOpen] = useState(false);
  const [retrying, setRetrying] = useState(false);
  const identities = useRef<DataLossList["rust"]>([]);
  const place = useEnvironmentPlace(state.organizationSlug, state.environmentId);
  const { machines } = useRuntimeLens(state.organizationSlug);
  const name = state.resource.resource.name;
  const input = {
    organizationSlug: state.organizationSlug,
    environmentId: state.environmentId,
    resourceId,
  };
  const latestQuery = latestVolumeRemoveAttemptQueryOptions(input);
  const latest = useQuery(latestQuery);
  const attempt = latest.data ?? null;
  const busy = attempt != null && volumeRemoveIsBusy(attempt.status);

  async function handleRetry() {
    if (!attempt || retrying) return;
    setRetrying(true);
    try {
      await rememberLatestVolumeRemoveAttempt(
        queryClient,
        latestQuery.queryKey,
        () =>
          retryRemove({
            data: {
              organizationSlug: state.organizationSlug,
              attemptId: attempt.id,
            },
          }),
      );
    } catch (error) {
      toast.error(toErrorMessage(error, "Couldn't try again."));
    } finally {
      setRetrying(false);
    }
  }

  return (
    <>
      <div className="py-8">
        <Separator />
      </div>
      <section aria-labelledby="volume-remove-heading">
        <h2
          id="volume-remove-heading"
          className="text-lg font-semibold text-destructive"
        >
          Danger
        </h2>
        <div className="mt-4 flex flex-col gap-4">
          {attempt ? (
            <VolumeRemoveStatusAlert
              name={name}
              attempt={attempt}
              retrying={retrying}
              onRetry={() => {
                void handleRetry();
              }}
            />
          ) : null}
          <div className="flex flex-col items-start justify-between gap-4 rounded-xl border border-destructive-border bg-destructive-soft p-4 sm:flex-row sm:items-center">
            <div className="min-w-0">
              <div className="text-sm font-semibold text-destructive">
                Delete its data
              </div>
              <p className="mt-1 text-sm text-destructive/85">
                Its files are still on your servers.
              </p>
            </div>
            <Button
              variant="destructive"
              className="shrink-0"
              disabled={busy}
              onClick={() => setOpen(true)}
            >
              <Trash2Icon data-icon="inline-start" />
              Delete data
            </Button>
          </div>
        </div>
      </section>
      <DeletionDialog
        open={open}
        onOpenChange={setOpen}
        title={`Delete what's left of ${name}?`}
        place={place}
        confirmLabel="Delete"
        callbacks={{
          load: async () => {
            const dataLoss = await loadDataLoss({ data: input });
            identities.current = dataLoss.rust;
            return dataLoss.rust.map((identity): DeletionItem => ({
              kind: "volume",
              name: identity.id.name,
              detail: machines.find((machine) => machine.id === identity.id.machine_id)?.name,
            }));
          },
          confirm: async () => {
            await rememberLatestVolumeRemoveAttempt(queryClient, latestQuery.queryKey,
              () => confirmRemove({ data: { ...input, identities: identities.current } }));
            toast(`Deleting what's left of ${name}`);
          },
        }}
      />
    </>
  );
}

function volumeRemoveStatusCopy(attempt: VolumeRemoveAttemptSummary, name: string) {
  switch (attempt.status) {
    case "awaiting_deployment":
      return { title: "Waiting for the deploy", description: `${name}'s data is deleted once the deploy stops using it.` };
    case "pending":
    case "running":
      return { title: `Deleting what's left of ${name}…`, description: undefined };
    case "partial":
      return { title: "Some servers didn't delete it", description: "Retry to finish." };
    case "unknown":
      return {
        title: `Not sure ${name}'s data is gone`,
        description: attempt.failureMessage ?? "Check your servers before trying again.",
      };
    case "cancelled":
      return { title: "Cancelled", description: attempt.failureMessage };
    case "failed":
      return { title: `Couldn't delete ${name}'s data`, description: attempt.failureMessage };
    case "completed":
      return { title: `${name}'s data is gone`, description: undefined };
    default: {
      const exhaustive: never = attempt.status;
      return exhaustive;
    }
  }
}

function VolumeRemoveStatusAlert({
  name,
  attempt,
  retrying,
  onRetry,
}: {
  name: string;
  attempt: VolumeRemoveAttemptSummary;
  retrying: boolean;
  onRetry: () => void;
}) {
  const retryable = volumeRemoveIsRetryable(attempt.status);
  const { title, description } = volumeRemoveStatusCopy(attempt, name);

  return (
    <Alert variant={retryable ? "destructive" : "default"}>
      <AlertTitle>{title}</AlertTitle>
      {description ? <AlertDescription>{description}</AlertDescription> : null}
      {retryable ? (
        <AlertAction>
          <Button
            variant="outline"
            size="sm"
            disabled={retrying}
            onClick={onRetry}
          >
            {retrying ? <Spinner data-icon="inline-start" /> : null}
            Retry
          </Button>
        </AlertAction>
      ) : null}
    </Alert>
  );
}
