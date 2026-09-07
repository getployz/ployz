import { useState } from "react";
import { Link, useParams, useRouter } from "@tanstack/react-router";
import {
  CheckIcon,
  ChevronDownIcon,
  LoaderIcon,
  MinusIcon,
  MoreVerticalIcon,
  XIcon,
} from "lucide-react";
import { Result } from "effect";
import { toast } from "sonner";
import { Badge } from "#/components/ui/badge";
import {
  DestructiveVolumeAttemptHistory,
  useDestructiveVolumeRetryControl,
} from "#/components/destructive-volume/deployment-destructive-volume";
import { Button } from "#/components/ui/button";
import { buttonVariants } from "#/components/ui/button-variants";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "#/components/ui/collapsible";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "#/components/ui/dropdown-menu";
import { Separator } from "#/components/ui/separator";
import {
  Alert,
  AlertDescription,
  AlertTitle,
} from "#/components/ui/alert";
import type { EnvironmentDeploymentStatus } from "#/modules/deployments/tables";
import { asString } from "#/lib/json";
import { getEnvironmentDeploymentsCollection } from "#/electric/collections";
import { cn } from "#/lib/utils";
import type { EnvironmentDeploymentSummary } from "#/modules/deployments/deployment-contract";
import {
  dispatchQueuedEnvironmentDeploymentServerFn,
  retryEnvironmentDeploymentServerFn,
  confirmEnvironmentDeploymentServerFn,
} from "#/modules/deployments/deployment.functions";
import {
  deployEventForDeployment,
  sdkDeployOperationKind,
  type SdkDeployProgressEvent,
} from "#/modules/deployments/deployment-presentation";
import { formatRelativeTime } from "#/utils/relative-time";
import { Spinner } from "#/components/ui/spinner";
import {
  managedNetworkingFailureLabel,
  managedNetworkingProgressLabel,
} from "#/components/deployment-networking-presentation";
import {
  useDeploymentOperationEvidence,
  type ParsedOperationEvidenceRow,
} from "#/modules/deployments/use-deployment-operation-evidence";

type BadgeVariant =
  | "secondary"
  | "info"
  | "success"
  | "warning"
  | "destructive";
const STATUS_VARIANT = {
  queued: "info",
  planning: "info",
  deploying: "info",
  applied: "success",
  failed: "destructive",
  cancelled: "secondary",
} as const satisfies Record<EnvironmentDeploymentStatus, BadgeVariant>;

const STATUS_LABEL = {
  queued: "Queued",
  planning: "Planning",
  deploying: "Deploying",
  applied: "Applied",
  failed: "Failed",
  cancelled: "Cancelled",
} as const satisfies Record<EnvironmentDeploymentStatus, string>;

type DeploymentOutcome = "success" | "failure" | "neutral" | "progress";

const STATUS_OUTCOME = {
  queued: "progress",
  planning: "progress",
  deploying: "progress",
  applied: "success",
  failed: "failure",
  cancelled: "neutral",
} as const satisfies Record<EnvironmentDeploymentStatus, DeploymentOutcome>;

const OUTCOME_HEADLINE = {
  success: "Deployment successful",
  failure: "Deployment failed",
  neutral: "Deployment cancelled",
  progress: "Deployment in progress",
} as const satisfies Record<DeploymentOutcome, string>;

const OUTCOME_TEXT_CLASS = {
  success: "text-success",
  failure: "text-destructive",
  neutral: "text-muted-foreground",
  progress: "text-info",
} as const satisfies Record<DeploymentOutcome, string>;

function evidenceLabel(event: ParsedOperationEvidenceRow) {
  if (event.schemaVersion !== 1) {
    return "Unsupported historical operation evidence";
  }
  const parsed = event.parsed;
  if (!parsed) return "Unsupported historical operation evidence";
  if (Result.isFailure(parsed)) return `Unsupported operation event: ${event.eventType}`;
  switch (parsed.success.eventType) {
    case "deploy_submitted":
      return "Deployment submitted";
    case "deploy_planning_started":
      return "Planning deployment";
    case "deploy_image_resolved":
      return "Image resolved";
    case "deploy_plan_created":
      return "Deployment plan created";
    case "deploy_running":
      return (
        managedNetworkingProgressLabel(parsed.success.payload.stage) ??
        `Deploying: ${parsed.success.payload.stage}`
      );
    case "deploy_image_availability_verified":
      return "Image availability verified";
    case "deploy_container_started":
      return "Container started";
    case "deploy_health_check_started":
      return "Health check started";
    case "deploy_phase_started":
      return `Phase ${parsed.success.payload.phase} started`;
    case "deploy_phase_finished":
      if (parsed.success.payload.outcome === "failed") {
        const networkingFailure = parsed.success.payload.services
          .filter((service) => service.result === "failed")
          .map((service) => managedNetworkingFailureLabel(service.failure))
          .find((label) => label != null);
        return networkingFailure ?? "Deployment phase failed";
      }
      return "Deployment phase promoted";
    case "deploy_cleanup_finished":
      return parsed.success.payload.failedCount > 0
        ? "Cleanup completed with warnings"
        : "Cleanup completed";
    case "deploy_completed": {
      const { outcome } = parsed.success.payload;
      if (outcome.includes("partially"))
        return `Deployment partially completed: ${outcome}`;
      return outcome.includes("warnings")
        ? "Deployment completed with warnings"
        : "Deployment completed";
    }
    case "deploy_failed": {
      return (
        managedNetworkingFailureLabel(parsed.success.payload.failure) ??
        `Deployment failed: ${parsed.success.payload.failure.kind}`
      );
    }
    case "cancelled":
      return "Core operation cancelled";
  }
}

function evidenceVariant(event: ParsedOperationEvidenceRow): BadgeVariant {
  const parsed = event.parsed;
  if (!parsed) return "secondary";
  if (Result.isFailure(parsed)) return "secondary";
  if (parsed.success.eventType === "deploy_failed") return "destructive";
  if (
    parsed.success.eventType === "deploy_cleanup_finished" &&
    parsed.success.payload.failedCount > 0
  )
    return "warning";
  if (parsed.success.eventType === "deploy_completed") {
    return parsed.success.payload.outcome === "completed" ? "success" : "warning";
  }
  return "info";
}

function progressStatusVariant(
  type: SdkDeployProgressEvent["rows"][number]["status"]["type"],
): BadgeVariant {
  switch (type) {
    case "completed":
      return "success";
    case "failed":
      return "destructive";
    case "running":
      return "info";
    case "pending":
    case "unexecuted":
      return "secondary";
    default:
      return "secondary";
  }
}

function DeployProgressStepper({ event }: { event: SdkDeployProgressEvent }) {
  if (event.rows.length === 0) return null;
  return (
    <ol className="flex flex-col gap-2">
      {event.rows.map((row) => (
        <li key={row.index} className="flex items-center gap-2">
          <Badge variant={progressStatusVariant(row.status.type)}>
            {row.status.type}
          </Badge>
          <span className="min-w-0 flex-1 truncate">
            {sdkDeployOperationKind(row.operation)}
          </span>
        </li>
      ))}
    </ol>
  );
}

function OutcomeIcon({ outcome }: { outcome: DeploymentOutcome }) {
  switch (outcome) {
    case "success":
      return <CheckIcon className="size-4 text-success" />;
    case "failure":
      return <XIcon className="size-4 text-destructive" />;
    case "neutral":
      return <MinusIcon className="size-4 text-muted-foreground" />;
    case "progress":
      return <LoaderIcon className="size-4 animate-spin text-info" />;
  }
}

export function DeploymentRow({
  deployment,
}: {
  deployment: EnvironmentDeploymentSummary;
}) {
  const [isOpen, setIsOpen] = useState(false);
  const [isRetrying, setIsRetrying] = useState(false);
  const [isDispatching, setIsDispatching] = useState(false);
  const [isConfirming, setIsConfirming] = useState(false);
  const router = useRouter();
  const { organizationSlug } = useParams({ strict: false });
  const outcome = STATUS_OUTCOME[deployment.status];
  const parsedPreview = deployment.deployPreview;
  const deployProgress = parsedPreview
    ? deployEventForDeployment(parsedPreview, deployment.status)
    : null;
  const hasDurableEvidence = Boolean(
    deployment.coreDeployId &&
      !deployment.coreDeployId.startsWith("local-preview:"),
  );
  const evidenceQuery = useDeploymentOperationEvidence({
    organizationSlug: organizationSlug ?? "",
    deploymentId: deployment.id,
    enabled: Boolean(isOpen && organizationSlug && hasDurableEvidence),
  });
  const evidence = evidenceQuery.events;
  const isLoadingEvidence = evidenceQuery.isFetching;
  const destructiveVolumeRetry = useDestructiveVolumeRetryControl({
    attempts: deployment.destructiveVolumeAttempts,
    organizationSlug: organizationSlug ?? null,
    environmentSlug: deployment.environmentSlug,
    deploymentStatus: deployment.status,
  });
  const queuedForNextTrigger =
    deployment.status === "queued" && !deployment.dispatchRequestedAt;
  const statusLabel =
    deployment.status === "queued"
      ? queuedForNextTrigger
        ? "Queued for next trigger"
        : "Waiting to deploy"
      : STATUS_LABEL[deployment.status];

  async function deployQueuedTarget() {
    if (!organizationSlug || !queuedForNextTrigger) return;
    setIsDispatching(true);
    try {
      await dispatchQueuedEnvironmentDeploymentServerFn({
          data: {
            organizationSlug,
            projectSlug: deployment.projectSlug,
            environmentSlug: deployment.environmentSlug,
          },
        });
      await router.invalidate();
      toast.success("Deployment requested.");
    } catch {
      toast.error("Could not request this deployment.");
    } finally {
      setIsDispatching(false);
    }
  }

  async function retryDeployment() {
    if (!organizationSlug || !deployment.canRetry) return;
    setIsRetrying(true);
    try {
      const receipt = await retryEnvironmentDeploymentServerFn({
          data: {
            organizationSlug,
            projectSlug: deployment.projectSlug,
            environmentSlug: deployment.environmentSlug,
            failedDeploymentId: deployment.id,
          },
        });
      await getEnvironmentDeploymentsCollection(
        organizationSlug,
      ).utils.awaitTxId(receipt.txid);
      toast.success("Deployment retry queued.");
    } catch {
      toast.error("Could not retry this deployment.");
    } finally {
      setIsRetrying(false);
    }
  }

  async function confirmDeploy() {
    if (!organizationSlug || deployment.status !== "planning") return;
    setIsConfirming(true);
    try {
      await confirmEnvironmentDeploymentServerFn({
          data: {
            organizationSlug,
            projectSlug: deployment.projectSlug,
            environmentSlug: deployment.environmentSlug,
            environmentDeploymentId: deployment.id,
          },
        });
      await router.invalidate();
      toast.success("Deploy confirmed.");
    } catch (error) {
      toast.error(
        error instanceof Error
          ? error.message
          : "Could not confirm this deployment.",
      );
    } finally {
      setIsConfirming(false);
    }
  }

  return (
    <>
    <Collapsible
      open={isOpen}
      onOpenChange={setIsOpen}
      className="rounded-lg border"
    >
      <div className="flex items-center gap-3 p-3">
          <Badge
            variant={
              queuedForNextTrigger
                ? "secondary"
                : STATUS_VARIANT[deployment.status]
            }
          >
            {statusLabel}
        </Badge>
        <div className="min-w-0 flex-1">
          <p className="truncate font-medium">
            {deployment.message ?? "Deployment"}
          </p>
          <p className="text-sm text-muted-foreground">
            {formatRelativeTime(deployment.createdAt)} ·{" "}
            {deployment.serviceCount}{" "}
            {deployment.serviceCount === 1 ? "service" : "services"}
          </p>
          {deployment.status === "failed" && deployment.failureMessage ? (
            <p className="truncate text-sm text-destructive">
              {deployment.failureMessage}
            </p>
          ) : null}
        </div>
        {organizationSlug && hasDurableEvidence ? (
          <Link
            to="/cloud/$organizationSlug/$projectSlug/$environmentSlug/logs"
            params={{
              organizationSlug,
              projectSlug: deployment.projectSlug,
              environmentSlug: deployment.environmentSlug,
            }}
            search={(prev) => prev}
            className={buttonVariants({ variant: "outline", size: "sm" })}
          >
            View logs
          </Link>
        ) : null}
        <DropdownMenu>
            <DropdownMenuTrigger
              render={<Button variant="ghost" size="icon" />}
            >
            <MoreVerticalIcon />
            <span className="sr-only">Deployment actions</span>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            <DropdownMenuGroup>
              <DropdownMenuItem
                onClick={() =>
                  void navigator.clipboard.writeText(deployment.id)
                }
              >
                Copy deployment ID
              </DropdownMenuItem>
              {deployment.coreDeployId ? (
                <DropdownMenuItem
                  onClick={() =>
                    void navigator.clipboard.writeText(
                      deployment.coreDeployId ?? "",
                    )
                  }
                >
                  Copy core deploy ID
                </DropdownMenuItem>
              ) : null}
              {deployment.canRetry ? (
                <DropdownMenuItem
                  disabled={isRetrying}
                  onClick={() => void retryDeployment()}
                >
                  Retry deployment
                </DropdownMenuItem>
              ) : null}
                {queuedForNextTrigger ? (
                  <DropdownMenuItem
                    disabled={isDispatching}
                    onClick={() => void deployQueuedTarget()}
                  >
                    Deploy now
                  </DropdownMenuItem>
                ) : null}
              {destructiveVolumeRetry.menuItem}
              {!deployment.canRetry &&
              deployment.status === "failed" &&
              deployment.destructiveVolumeAttempts.length > 0 ? (
                <DropdownMenuItem disabled>
                  Review volume deletion from the canvas
                </DropdownMenuItem>
              ) : null}
            </DropdownMenuGroup>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>

      {parsedPreview ? (
        <>
          <Separator />
          <div className="flex flex-col gap-3 p-3">
            <Alert>
            <AlertTitle>Deploy preview</AlertTitle>
            <AlertDescription className="flex flex-col gap-2">
              <span>
                {parsedPreview.operations.length === 0
                  ? "No changes."
                  : `${parsedPreview.operations.length} ${
                      parsedPreview.operations.length === 1
                        ? "operation"
                        : "operations"
                    }`}
              </span>
              {parsedPreview.warnings.length > 0 ? (
                <ul className="flex flex-col gap-1">
                  {parsedPreview.warnings.map((warning, index) => (
                    <li key={index}>
                      {asString(warning) ?? JSON.stringify(warning)}
                    </li>
                  ))}
                </ul>
              ) : null}
            </AlertDescription>
          </Alert>
          {deployProgress ? (
            <DeployProgressStepper event={deployProgress} />
          ) : null}
          {organizationSlug && deployment.status === "planning" ? (
            <Button
              disabled={isConfirming}
              onClick={() => void confirmDeploy()}
            >
              {isConfirming ? <Spinner data-icon="inline-start" /> : null}
              Confirm deploy
            </Button>
          ) : null}
        </div>
        </>
      ) : null}

      {deployment.coreDeployId ||
      deployment.destructiveVolumeAttempts.length > 0 ? (
        <>
          <Separator />
          <CollapsibleTrigger
            render={
              <Button
                variant="ghost"
                className="h-auto w-full justify-start rounded-none rounded-b-lg px-3 py-2.5 font-normal hover:bg-muted/50"
              />
            }
          >
            <OutcomeIcon outcome={outcome} />
            <span className={cn("font-medium", OUTCOME_TEXT_CLASS[outcome])}>
              {OUTCOME_HEADLINE[outcome]}
            </span>
            <ChevronDownIcon
              className={cn(
                "ml-auto size-4 text-muted-foreground transition-transform",
                isOpen ? "rotate-180" : "rotate-0",
              )}
            />
          </CollapsibleTrigger>
          <CollapsibleContent>
            <div className="flex flex-col px-3 pb-2">
              {deployment.destructiveVolumeAttempts.length > 0 ? (
                <DestructiveVolumeAttemptHistory
                  attempts={deployment.destructiveVolumeAttempts}
                />
              ) : null}
              {evidence.map((event) => (
                <div
                  key={event.sequence}
                  className="flex items-center gap-3 py-2"
                >
                  <Badge variant={evidenceVariant(event)}>
                    {event.sequence}
                  </Badge>
                  <span className="min-w-0 flex-1 truncate">
                    {evidenceLabel(event)}
                  </span>
                </div>
              ))}
              {evidenceQuery.hasNextPage ? (
                <Button
                  variant="ghost"
                  size="sm"
                  disabled={isLoadingEvidence}
                  onClick={() => void evidenceQuery.fetchNextPage()}
                >
                  {isLoadingEvidence ? <Spinner /> : null}
                  {isLoadingEvidence
                    ? "Loading evidence"
                    : "Load more evidence"}
                </Button>
              ) : null}
            </div>
          </CollapsibleContent>
        </>
      ) : null}
    </Collapsible>
    {destructiveVolumeRetry.dialog}
    </>
  );
}
