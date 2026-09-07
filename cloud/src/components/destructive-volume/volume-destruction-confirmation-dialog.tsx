"use client";

import { useEffect, useEffectEvent, useId, useState } from "react";
import {
  AlertTriangleIcon,
  DatabaseIcon,
  RefreshCwIcon,
} from "lucide-react";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogMedia,
  AlertDialogTitle,
} from "#/components/ui/alert-dialog";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import {
  Card,
  CardAction,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "#/components/ui/card";
import {
  Field,
  FieldDescription,
  FieldError,
  FieldLabel,
} from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { Skeleton } from "#/components/ui/skeleton";
import { Spinner } from "#/components/ui/spinner";
import type {
  DestructiveVolumeEvidence,
  PreparedNamespaceDestructiveEvidence,
} from "#/modules/operations/destructive-volume-evidence";
import type { DestructiveVolumeReview } from "#/modules/deployments/deployment-contract";
import type { EnvironmentSavedStateBasis } from "#/modules/environment-design/saved-state";

export type PreparedDestructiveReview = PreparedNamespaceDestructiveEvidence & {
  reviews: DestructiveVolumeReview[];
  reviewedMutation?: {
    savedStateBasis: EnvironmentSavedStateBasis;
    workingStateFingerprint: string;
    serviceIds: string[];
    volumeIds: string[];
  };
};

/** Compatibility name for the deployment-history volume-only workflow. */
export type PreparedVolumeDestruction = PreparedDestructiveReview;

export type DestructiveConfirmationCallbacks = {
  load: () => Promise<PreparedDestructiveReview>;
  confirm: (
    preparation: PreparedDestructiveReview,
  ) => Promise<
    | void
    | { state: "submitted" }
    | {
        state: "review_updated_evidence";
        preparation: PreparedDestructiveReview;
      }
  >;
};

type DialogState =
  | { status: "gathering" }
  | { status: "failed"; message: string }
  | {
      status: "ready";
      preparation: PreparedDestructiveReview;
      evidenceChanged: boolean;
    };

const byteFormatter = new Intl.NumberFormat("en-US", {
  maximumFractionDigits: 0,
});
const dateTimeFormatter = new Intl.DateTimeFormat("en-US", {
  year: "numeric",
  month: "short",
  day: "numeric",
  hour: "numeric",
  minute: "2-digit",
  timeZone: "UTC",
  timeZoneName: "short",
});

type DestructiveConfirmationDialogProps = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  confirmPhrase: string;
  callbacks: DestructiveConfirmationCallbacks;
  serviceNames?: string[];
  title?: string;
  description?: string;
  actionLabel?: string;
  pendingActionLabel?: string;
};

export function DestructiveConfirmationDialog(
  props: DestructiveConfirmationDialogProps,
) {
  if (!props.open) return null;
  return <OpenDestructiveConfirmationDialog {...props} />;
}

export function VolumeDestructionConfirmationDialog(
  props: DestructiveConfirmationDialogProps,
) {
  return (
    <DestructiveConfirmationDialog
      title="Delete deployed volume data?"
      description="Review the latest machine testimony before deploying this permanent deletion. This cannot be undone."
      actionLabel="Deploy and delete"
      pendingActionLabel="Deploying..."
      {...props}
    />
  );
}

function OpenDestructiveConfirmationDialog({
  onOpenChange,
  confirmPhrase,
  callbacks,
  serviceNames = [],
  title = "Confirm destructive changes?",
  description = "Review every destructive change before continuing.",
  actionLabel = "Confirm changes",
  pendingActionLabel = "Confirming...",
}: Omit<DestructiveConfirmationDialogProps, "open">) {
  const [state, setState] = useState<DialogState>({ status: "gathering" });
  const [typedPhrase, setTypedPhrase] = useState("");
  const [isSubmitting, setIsSubmitting] = useState(false);
  const inputId = useId();
  const prepareOnOpen = useEffectEvent(callbacks.load);

  useEffect(() => {
    let active = true;
    void prepareOnOpen().then(
      (preparation) => {
        if (active) {
          setState({ status: "ready", preparation, evidenceChanged: false });
        }
      },
      (error) => {
        if (active) {
          setState({ status: "failed", message: errorMessage(error) });
        }
      },
    );

    return () => {
      active = false;
    };
  }, []);

  async function reloadEvidence() {
    const previousFingerprint =
      state.status === "ready" ? state.preparation.fingerprint : null;
    setTypedPhrase("");
    setState({ status: "gathering" });
    try {
      const preparation = await callbacks.load();
      setState({
        status: "ready",
        preparation,
        evidenceChanged:
          previousFingerprint != null &&
          previousFingerprint !== preparation.fingerprint,
      });
    } catch (error) {
      setState({ status: "failed", message: errorMessage(error) });
    }
  }

  async function confirmDestruction() {
    if (
      state.status !== "ready" ||
      typedPhrase !== confirmPhrase ||
      isSubmitting
    ) {
      return;
    }

    setIsSubmitting(true);
    try {
      const result = await callbacks.confirm(state.preparation);
      if (result?.state === "review_updated_evidence") {
        setTypedPhrase("");
        setState({
          status: "ready",
          preparation: result.preparation,
          evidenceChanged: true,
        });
        return;
      }
      onOpenChange(false);
    } catch (error) {
      setTypedPhrase("");
      setState({ status: "failed", message: errorMessage(error) });
    } finally {
      setIsSubmitting(false);
    }
  }

  const canConfirm =
    state.status === "ready" &&
    typedPhrase === confirmPhrase &&
    !isSubmitting;

  return (
    <AlertDialog open onOpenChange={onOpenChange}>
      <AlertDialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-2xl">
        <AlertDialogHeader>
          <AlertDialogMedia>
            <AlertTriangleIcon />
          </AlertDialogMedia>
          <AlertDialogTitle>{title}</AlertDialogTitle>
          <AlertDialogDescription>
            {description}
          </AlertDialogDescription>
        </AlertDialogHeader>

        {state.status === "gathering" ? <GatheringEvidence /> : null}
        {state.status === "failed" ? (
          <Alert variant="destructive">
            <AlertTriangleIcon />
            <AlertTitle>Destructive review could not be prepared</AlertTitle>
            <AlertDescription>{state.message}</AlertDescription>
          </Alert>
        ) : null}
        {state.status === "ready" ? (
          <>
            {state.evidenceChanged ? (
              <Alert>
                <RefreshCwIcon />
                <AlertTitle>Volume evidence changed</AlertTitle>
                <AlertDescription>
                  Review the refreshed evidence and type the confirmation phrase
                  again.
                </AlertDescription>
              </Alert>
            ) : null}
            {serviceNames.length > 0 ? (
              <ServiceRemovals services={serviceNames} />
            ) : null}
            {state.preparation.volumes.length > 0 ? (
              <section
                className="flex flex-col gap-2"
                aria-label="Volumes to remove"
              >
                <strong>Volumes to remove</strong>
                <div className="flex max-h-72 flex-col gap-3 overflow-y-auto">
                  {state.preparation.volumes.map((volume) => (
                    <VolumeEvidenceCard
                      key={volume.fingerprint}
                      evidence={volume.evidence}
                      fingerprint={volume.fingerprint}
                    />
                  ))}
                </div>
              </section>
            ) : null}
            {state.preparation.volumes.length > 0 ? (
              <ServiceReferences
                services={state.preparation.referencingServices}
              />
            ) : null}
          </>
        ) : null}

        <Field data-invalid={state.status === "failed" || undefined}>
          <FieldLabel htmlFor={inputId}>
            Type <strong className="font-mono">{confirmPhrase}</strong> to
            confirm
          </FieldLabel>
          <Input
            id={inputId}
            value={typedPhrase}
            onChange={(event) => setTypedPhrase(event.target.value)}
            placeholder={confirmPhrase}
            disabled={state.status !== "ready" || isSubmitting}
            aria-invalid={state.status === "failed" || undefined}
            onKeyDown={(event) => {
              if (event.key === "Enter" && canConfirm) {
                event.preventDefault();
                void confirmDestruction();
              }
            }}
          />
          <FieldDescription>
            The phrase must exactly match the environment namespace.
          </FieldDescription>
          {state.status === "failed" ? (
            <FieldError>Reload evidence before trying again.</FieldError>
          ) : null}
        </Field>

        <AlertDialogFooter>
          <AlertDialogCancel disabled={isSubmitting}>Cancel</AlertDialogCancel>
          <Button
            variant="outline"
            disabled={state.status === "gathering" || isSubmitting}
            onClick={() => void reloadEvidence()}
          >
            <RefreshCwIcon data-icon="inline-start" />
            Reload evidence
          </Button>
          <AlertDialogAction
            variant="destructive"
            disabled={!canConfirm}
            onClick={() => void confirmDestruction()}
          >
            {isSubmitting ? <Spinner /> : null}
            {isSubmitting ? pendingActionLabel : actionLabel}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}

function ServiceRemovals({ services }: { services: string[] }) {
  return (
    <section className="flex flex-col gap-2" aria-label="Services to remove">
      <strong>Services to remove</strong>
      <ul className="flex flex-wrap gap-2">
        {services.map((service) => (
          <li key={service}>
            <Badge variant="destructive">{service}</Badge>
          </li>
        ))}
      </ul>
    </section>
  );
}

function GatheringEvidence() {
  return (
    <div aria-label="Preparing destructive review" className="flex flex-col gap-3">
      <div className="flex items-center gap-2 text-sm text-muted-foreground">
        <Spinner />
        Preparing destructive review…
      </div>
      <Skeleton className="h-24 w-full" />
    </div>
  );
}

function VolumeEvidenceCard({
  evidence,
  fingerprint,
}: {
  evidence: DestructiveVolumeEvidence;
  fingerprint: string;
}) {
  const availability = evidence.availability;
  const available = availability.status === "available";

  return (
    <Card size="sm" aria-label={evidence.volumeName}>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <DatabaseIcon />
          {evidence.volumeName}
        </CardTitle>
        <CardDescription>
          Runtime testimony from the pinned machine
        </CardDescription>
        <CardAction>
          <Badge variant={available ? "success" : "warning"}>
            {availabilityLabel(availability.status)}
          </Badge>
        </CardAction>
      </CardHeader>
      <CardContent>
        <dl className="grid grid-cols-[max-content_1fr] gap-x-3 gap-y-1">
          <dt className="text-muted-foreground">Fingerprint</dt>
          <dd className="break-all font-mono">{fingerprint}</dd>
          <dt className="text-muted-foreground">Kind</dt>
          <dd>
            {evidence.kind.kind === "provisioned" ? "Provisioned" : "Plain"}
          </dd>
          {evidence.kind.kind === "provisioned" ? (
            <>
              <dt className="text-muted-foreground">Dataset</dt>
              <dd className="break-all font-mono">{evidence.kind.dataset}</dd>
              <dt className="text-muted-foreground">Quota</dt>
              <dd>{formatBytes(evidence.kind.maxSizeBytes)}</dd>
            </>
          ) : null}
          <dt className="text-muted-foreground">Pinned machine</dt>
          <dd className="break-all font-mono">{evidence.machineId}</dd>
          <dt className="text-muted-foreground">Used</dt>
          <dd>
            {availability.status === "available"
              ? formatBytes(availability.usedBytes)
              : "Unknown"}
          </dd>
          <dt className="text-muted-foreground">Last write</dt>
          <dd>
            {availability.status === "available"
              ? dateTimeFormatter.format(
                  new Date(availability.lastWriteUnixSeconds * 1_000),
                )
              : "Unknown"}
          </dd>
        </dl>
        {!available ? (
          <Alert className="mt-3">
            <AlertTriangleIcon />
            <AlertTitle>{evidence.machineId}</AlertTitle>
            <AlertDescription>
              {availability.status === "no_answer"
                ? "The machine did not answer. Size and last-write time are unknown."
                : "The machine reported this volume unavailable. Size and last-write time are unknown."}
            </AlertDescription>
          </Alert>
        ) : null}
      </CardContent>
    </Card>
  );
}

function ServiceReferences({ services }: { services: string[] }) {
  return (
    <section className="flex flex-col gap-2" aria-label="Referencing services">
      <strong>Referencing services</strong>
      {services.length > 0 ? (
        <ul className="flex flex-wrap gap-2">
          {services.map((service) => (
            <li key={service}>
              <Badge variant="outline">{service}</Badge>
            </li>
          ))}
        </ul>
      ) : (
        <p className="text-sm text-muted-foreground">
          No services reference these volumes.
        </p>
      )}
    </section>
  );
}

function formatBytes(bytes: number) {
  return `${byteFormatter.format(bytes)} bytes`;
}

function availabilityLabel(status: DestructiveVolumeEvidence["availability"]["status"]) {
  if (status === "available") return "Available";
  return status === "no_answer" ? "No answer" : "Unavailable";
}

function errorMessage<T>(error: T) {
  return error instanceof Error
    ? error.message
    : "The destructive review is unavailable.";
}
