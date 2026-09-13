import { getVolumeRemoveAttemptsCollection } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import { useState } from "react";
import { Trash2Icon } from "lucide-react";
import { toast } from "sonner";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useServerFn } from "@tanstack/react-start";
import { getEnvironmentsCollection } from "#/collections/collections";
import { VolumeRemoveDataLossDialog } from "#/components/data-loss/data-loss-confirm-dialog";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "#/components/ui/alert-dialog";
import { Alert, AlertAction, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Button } from "#/components/ui/button";
import { Separator } from "#/components/ui/separator";
import { Spinner } from "#/components/ui/spinner";
import { createEnvironmentNodeNameSchema } from "#/modules/environment-design/environment-node-names";
import { environmentDesignFields } from "#/modules/environment-design/fields";
import {
  deleteVolumeResourceServerFn,
  updateVolumeResourceServerFn,
} from "#/modules/environment-design/resource-functions";
import {
  confirmVolumeRemoveServerFn,
  loadLatestVolumeRemoveAttemptServerFn,
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

const resourceNameSchema = environmentDesignFields.resource.name;

export function VolumeDrawer({
  params,
  state,
}: {
  params: VolumeResourceRouteParams;
  state: VolumeDrawerState;
}) {
  const collectionScope = useCollectionScope();
  const navigate = useNavigate();
  const deleteVolume = useServerFn(deleteVolumeResourceServerFn);
  const updateVolume = useServerFn(updateVolumeResourceServerFn);
  const [isDeleting, setIsDeleting] = useState(false);
  const document = useEnvironmentDocument(state.organizationSlug, state.environmentId);
  function revision() {
    if (!document) throw new Error("Environment is not loaded.");
    return document.revision;
  }
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

  async function handleDelete() {
    setIsDeleting(true);
    try {
      const result = await deleteVolume({
        data: {
          organizationSlug: state.organizationSlug,
          environmentId: state.environmentId,
          revision: revision(),
          resourceId,
        },
      });
      await getEnvironmentsCollection(state.organizationSlug, collectionScope).writeCommitted(result.data);
      await navigate({
        to: ENVIRONMENT_INDEX_ROUTE_TO,
        params: {
          organizationSlug: params.organizationSlug,
          projectSlug: params.projectSlug,
          environmentSlug: params.environmentSlug,
        },
        search: (prev) => prev,
      });
    } catch (error) {
      toast.error(
        error instanceof Error
          ? error.message
          : "The volume couldn’t be deleted. Try again.",
      );
    } finally {
      setIsDeleting(false);
    }
  }

  return (
    <div className="flex h-full min-h-0 flex-col bg-background">
      <CanvasInspectorHeader params={params}>
        <CanvasInspectorNameEditor
          value={state.resource.resource.name}
          schema={nameSchema}
          editTitle="Edit volume name"
          editDescription="Rename this volume."
          placeholder="Volume name"
          onRename={async (value) => {
            const result = await updateVolume({ data: {
              organizationSlug: state.organizationSlug,
              environmentId: state.environmentId,
              revision: revision(),
              resourceId,
              name: value,
            } });
            await getEnvironmentsCollection(state.organizationSlug, collectionScope).writeCommitted(result.data);
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
              <div className="mt-4 flex flex-col items-start justify-between gap-4 rounded-xl border border-destructive-border bg-destructive-soft p-4 sm:flex-row sm:items-center">
                <div className="min-w-0">
                  <div className="text-sm font-semibold text-destructive">
                    Delete this volume
                  </div>
                  <p className="mt-1 text-sm text-destructive/85">
                    {mountedCount > 0
                      ? `Stages deletion and removes ${mountedCount} service mount${mountedCount === 1 ? "" : "s"} on the next deploy.`
                      : "Stages deletion until you deploy or discard the change."}
                  </p>
                </div>
                <AlertDialog>
                  <AlertDialogTrigger
                    render={
                      <Button
                        variant="destructive"
                        className="shrink-0"
                        disabled={isDeleting}
                      >
                        <Trash2Icon data-icon="inline-start" />
                        Delete volume
                      </Button>
                    }
                  />
                  <AlertDialogContent>
                    <AlertDialogHeader>
                      <AlertDialogTitle>Delete this volume?</AlertDialogTitle>
                      <AlertDialogDescription>
                        {mountedCount > 0
                          ? `Deletion is staged. ${mountedCount} mounted service${
                              mountedCount === 1 ? "" : "s"
                            } will drop this mount on the next deploy, and deployed data is removed on confirmation at deploy time.`
                          : "Deletion is staged until you deploy or discard it."}
                      </AlertDialogDescription>
                    </AlertDialogHeader>
                    <AlertDialogFooter>
                      <AlertDialogCancel>Cancel</AlertDialogCancel>
                      <AlertDialogAction
                        onClick={() => {
                          void handleDelete();
                        }}
                      >
                        Delete
                      </AlertDialogAction>
                    </AlertDialogFooter>
                  </AlertDialogContent>
                </AlertDialog>
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
  const collectionScope = useCollectionScope();
  const loadDataLoss = useServerFn(loadVolumeRemoveDataLossServerFn);
  const confirmRemove = useServerFn(confirmVolumeRemoveServerFn);
  const retryRemove = useServerFn(retryVolumeRemoveServerFn);
  const loadLatest = useServerFn(loadLatestVolumeRemoveAttemptServerFn);
  const queryClient = useQueryClient();
  const [open, setOpen] = useState(false);
  const [retrying, setRetrying] = useState(false);
  const input = {
    organizationSlug: state.organizationSlug,
    environmentId: state.environmentId,
    resourceId,
  };
  const latestQuery = latestVolumeRemoveAttemptQueryOptions(input, () =>
    loadLatest({ data: input }),
  );
  const latest = useQuery(latestQuery);
  const attempt = latest.data ?? null;
  const busy = attempt != null && volumeRemoveIsBusy(attempt.status);

  async function handleRetry() {
    if (!attempt || retrying) return;
    setRetrying(true);
    try {
      const committed = await rememberLatestVolumeRemoveAttempt(
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
      await getVolumeRemoveAttemptsCollection(state.organizationSlug, collectionScope).writeCommitted(committed);
      toast.success("Volume remove retry started.");
    } catch (error) {
      toast.error(
        error instanceof Error
          ? error.message
          : "The volume remove couldn’t be retried.",
      );
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
                Remove volume data
              </div>
              <p className="mt-1 text-sm text-destructive/85">
                Deletes the Docker volumes on the machine. This cannot be
                undone.
              </p>
            </div>
            <Button
              variant="destructive"
              className="shrink-0"
              disabled={busy}
              onClick={() => setOpen(true)}
            >
              <Trash2Icon data-icon="inline-start" />
              Remove volume data
            </Button>
          </div>
        </div>
      </section>
      <VolumeRemoveDataLossDialog
        open={open}
        onOpenChange={setOpen}
        confirmPhrase={state.resource.resource.name}
        callbacks={{
          load: () => loadDataLoss({ data: input }),
          confirm: async (identities) => {
            const committed = await rememberLatestVolumeRemoveAttempt(
              queryClient,
              latestQuery.queryKey,
              () =>
                confirmRemove({
                  data: { ...input, identities },
                }),
            );
            await getVolumeRemoveAttemptsCollection(state.organizationSlug, collectionScope).writeCommitted(committed);
            toast.success("Volume remove started.");
          },
        }}
      />
    </>
  );
}

function volumeRemoveStatusCopy(attempt: VolumeRemoveAttemptSummary) {
  switch (attempt.status) {
    case "awaiting_deployment":
      return {
        title: "Waiting for deployment",
        description:
          "Volume data removal begins after the deployment removes its service references.",
      };
    case "pending":
    case "running":
      return {
        title: "Removing volume data",
        description: "Inngest is deleting the confirmed Docker volumes.",
      };
    case "partial":
      return {
        title: "Some machines failed",
        description: "Retry remaining volumes to finish.",
      };
    case "unknown":
      return {
        title: "Volume remove outcome unknown",
        description:
          attempt.failureMessage ??
          "Cloud could not determine whether Ployz removed the volume. Review it before retrying.",
      };
    case "cancelled":
      return {
        title: "Volume remove cancelled",
        description:
          attempt.failureMessage ?? "Retry to run the same volume list again.",
      };
    case "failed":
      return {
        title: "Volume remove failed",
        description:
          attempt.failureMessage ?? "Retry to run the same volume list again.",
      };
    case "completed":
      return {
        title: "Volume remove finished",
        description: "The confirmed Docker volumes were deleted.",
      };
    default: {
      const exhaustive: never = attempt.status;
      return exhaustive;
    }
  }
}

function VolumeRemoveStatusAlert({
  attempt,
  retrying,
  onRetry,
}: {
  attempt: VolumeRemoveAttemptSummary;
  retrying: boolean;
  onRetry: () => void;
}) {
  const retryable = volumeRemoveIsRetryable(attempt.status);
  const { title, description } = volumeRemoveStatusCopy(attempt);

  return (
    <Alert variant={retryable ? "destructive" : "default"}>
      <AlertTitle>{title}</AlertTitle>
      <AlertDescription>{description}</AlertDescription>
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
