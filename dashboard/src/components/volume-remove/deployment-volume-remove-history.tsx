import { Link } from "@tanstack/react-router";
import {
  Alert,
  AlertDescription,
  AlertTitle,
} from "#/components/ui/alert";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "#/components/ui/card";
import type { EnvironmentDeploymentSummary } from "#/modules/deployments/deployment-contract";
import { ENVIRONMENT_RESOURCE_ROUTE_TO } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/environment-route-paths";

type Attempt = EnvironmentDeploymentSummary["volumeRemoveAttempts"][number];
type BadgeVariant =
  | "secondary"
  | "info"
  | "success"
  | "warning"
  | "destructive";

export function VolumeRemoveAttemptHistory({
  attempts,
  organizationSlug,
  projectSlug,
  environmentSlug,
  availableVolumeResourceIds,
}: {
  attempts: Attempt[];
  organizationSlug: string;
  projectSlug: string;
  environmentSlug: string;
  availableVolumeResourceIds: ReadonlySet<string>;
}) {
  return (
    <section
      aria-label="Volume removal history"
      className="flex flex-col gap-2"
    >
      <strong>Volume removal</strong>
      {attempts.map((attempt) => {
        const recoveryResourceId = volumeRemoveRecoveryResourceId(
          attempt,
          availableVolumeResourceIds,
        );
        return (
          <Card key={attempt.id}>
            <CardHeader>
              <CardTitle className="flex flex-wrap items-center gap-2">
                <Badge variant={volumeRemoveBadge(attempt.status)}>
                  {volumeRemoveStatusLabel(attempt.status)}
                </Badge>
                <span className="font-mono">
                  {attempt.volumes.map((volume) => volume.name).join(", ")}
                </span>
              </CardTitle>
              <CardDescription>
                {attempt.volumes.map((volume) => volume.machine_id).join(", ")}
              </CardDescription>
            </CardHeader>
            {attempt.failureMessage || attempt.outcome || recoveryResourceId ? (
              <CardContent className="flex flex-col gap-2">
                {attempt.failureMessage ? (
                  <Alert variant="destructive">
                    <AlertTitle>Volume removal needs attention</AlertTitle>
                    <AlertDescription>{attempt.failureMessage}</AlertDescription>
                  </Alert>
                ) : null}
                {attempt.outcome ? (
                  <Alert>
                    <AlertTitle>Removal outcome</AlertTitle>
                    <AlertDescription>
                      {outcomeSummary(attempt.outcome)}
                    </AlertDescription>
                  </Alert>
                ) : null}
                {recoveryResourceId ? (
                  <Button
                    nativeButton={false}
                    variant="link"
                    size="sm"
                    render={
                      <Link
                        to={ENVIRONMENT_RESOURCE_ROUTE_TO}
                        params={{
                          organizationSlug,
                          projectSlug,
                          environmentSlug,
                          resourceId: recoveryResourceId,
                        }}
                        search={(prev) => prev}
                      />
                    }
                  >
                    Review volume removal
                  </Button>
                ) : null}
              </CardContent>
            ) : null}
          </Card>
        );
      })}
    </section>
  );
}

function volumeRemoveRecoveryResourceId(
  attempt: Attempt,
  availableVolumeResourceIds: ReadonlySet<string>,
) {
  if (
    attempt.status === "completed" ||
    attempt.environmentResourceId === null ||
    !availableVolumeResourceIds.has(attempt.environmentResourceId)
  ) {
    return null;
  }
  return attempt.environmentResourceId;
}

function outcomeSummary(outcome: NonNullable<Attempt["outcome"]>) {
  const parts = [
    outcome.destroyed.length > 0
      ? `${outcome.destroyed.length} removed`
      : null,
    outcome.failed.length > 0 ? `${outcome.failed.length} failed` : null,
    outcome.omitted.length > 0 ? `${outcome.omitted.length} omitted` : null,
  ].filter((part): part is string => part !== null);
  return parts.length > 0 ? parts.join(" · ") : "No volume outcome returned.";
}

function volumeRemoveStatusLabel(status: Attempt["status"]) {
  return status.replaceAll("_", " ");
}

function volumeRemoveBadge(status: Attempt["status"]): BadgeVariant {
  switch (status) {
    case "completed":
      return "success";
    case "awaiting_deployment":
    case "pending":
    case "running":
      return "info";
    case "partial":
    case "unknown":
      return "warning";
    case "failed":
      return "destructive";
    case "cancelled":
      return "secondary";
  }
}
