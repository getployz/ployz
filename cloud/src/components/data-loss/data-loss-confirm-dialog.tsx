"use client";

import { useEffect, useEffectEvent, useId, useState, type ReactNode } from "react";
import { AlertTriangleIcon, RefreshCwIcon } from "lucide-react";
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
import { Empty, EmptyDescription, EmptyHeader, EmptyTitle } from "#/components/ui/empty";
import {
  Field,
  FieldDescription,
  FieldError,
  FieldGroup,
  FieldLabel,
} from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import {
  Item,
  ItemContent,
  ItemGroup,
  ItemMedia,
  ItemTitle,
} from "#/components/ui/item";
import { Skeleton } from "#/components/ui/skeleton";
import { Spinner } from "#/components/ui/spinner";
import { toErrorMessage } from "#/lib/error-message";
import {
  cloudRowKey,
  withMissingDataLossIdentities,
  type CloudRowLoss,
  type DataLossList,
} from "#/modules/runtime/data-loss-confirm";
import {
  dataLossIdentityKey,
  dataLossIdentityLabel,
  type DataLossIdentity,
} from "#/modules/runtime/data-loss-identity";

export type DataLossConfirmResult = {
  state: "missing_identities";
  identities: DataLossIdentity[];
};

export type DataLossConfirmCallbacks = {
  load: () => Promise<DataLossList>;
  confirm: (
    rust: DataLossIdentity[],
  ) => Promise<void | DataLossConfirmResult>;
};

export type DataLossConfirmDialogProps = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  confirmPhrase: string;
  callbacks: DataLossConfirmCallbacks;
  title?: ReactNode;
  description?: ReactNode;
  actionLabel?: string;
  pendingLabel?: string;
};

type DialogState =
  | { status: "gathering" }
  | { status: "failed"; message: string }
  | { status: "ready"; list: DataLossList; stale: boolean };

export function DataLossConfirmDialog({
  open,
  title = "Confirm Data Loss?",
  description = "Review every named identity before continuing.",
  actionLabel = "Confirm",
  pendingLabel = "Confirming...",
  ...props
}: DataLossConfirmDialogProps) {
  if (!open) return null;
  return (
    <OpenDataLossConfirmDialog
      title={title}
      description={description}
      actionLabel={actionLabel}
      pendingLabel={pendingLabel}
      {...props}
    />
  );
}

export function VolumeRemoveDataLossDialog(props: DataLossConfirmDialogProps) {
  return (
    <DataLossConfirmDialog
      title="Remove volume data?"
      description="These Docker volumes will be deleted. This cannot be undone."
      actionLabel="Remove volumes"
      pendingLabel="Removing..."
      {...props}
    />
  );
}

export function MachineRemoveDataLossDialog(props: DataLossConfirmDialogProps) {
  return (
    <DataLossConfirmDialog
      title="Remove this machine?"
      description="Named Data Loss on this machine will be destroyed, then the machine will be reset."
      actionLabel="Remove machine"
      pendingLabel="Removing..."
      {...props}
    />
  );
}

export function TeardownDataLossDialog(props: DataLossConfirmDialogProps) {
  return (
    <DataLossConfirmDialog
      title="Tear down?"
      description="Named Data Loss and Cloud records below will be destroyed."
      actionLabel="Tear down"
      pendingLabel="Tearing down..."
      {...props}
    />
  );
}

function OpenDataLossConfirmDialog({
  onOpenChange,
  confirmPhrase,
  callbacks,
  title,
  description,
  actionLabel,
  pendingLabel,
}: Omit<DataLossConfirmDialogProps, "open"> & {
  title: ReactNode;
  actionLabel: string;
  pendingLabel: string;
}) {
  const [state, setState] = useState<DialogState>({ status: "gathering" });
  const [typedPhrase, setTypedPhrase] = useState("");
  const [pending, setPending] = useState(false);
  const inputId = useId();
  const loadOnOpen = useEffectEvent(callbacks.load);

  useEffect(() => {
    let active = true;
    void loadOnOpen().then(
      (list) => {
        if (active) setState({ status: "ready", list, stale: false });
      },
      (error) => {
        if (active) {
          setState({
            status: "failed",
            message: toErrorMessage(error, "Data Loss is unavailable."),
          });
        }
      },
    );
    return () => {
      active = false;
    };
  }, []);

  async function confirmDataLoss() {
    if (
      state.status !== "ready" ||
      typedPhrase !== confirmPhrase ||
      pending
    ) {
      return;
    }

    setPending(true);
    try {
      const result = await callbacks.confirm(state.list.rust);
      if (result?.state === "missing_identities") {
        setTypedPhrase("");
        setState({
          status: "ready",
          list: withMissingDataLossIdentities(state.list, result.identities),
          stale: true,
        });
        return;
      }
      onOpenChange(false);
    } catch (error) {
      setTypedPhrase("");
      setState({
        status: "failed",
        message: toErrorMessage(error, "Data Loss is unavailable."),
      });
    } finally {
      setPending(false);
    }
  }

  const canConfirm =
    state.status === "ready" && typedPhrase === confirmPhrase && !pending;

  return (
    <AlertDialog open onOpenChange={onOpenChange}>
      <AlertDialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-lg">
        <AlertDialogHeader>
          <AlertDialogMedia>
            <AlertTriangleIcon className="text-destructive" />
          </AlertDialogMedia>
          <AlertDialogTitle>{title}</AlertDialogTitle>
          {description ? (
            <AlertDialogDescription>{description}</AlertDialogDescription>
          ) : null}
        </AlertDialogHeader>

        {state.status === "gathering" ? <GatheringDataLoss /> : null}
        {state.status === "failed" ? (
          <Alert variant="destructive">
            <AlertTriangleIcon />
            <AlertTitle>Data Loss could not be prepared</AlertTitle>
            <AlertDescription>{state.message}</AlertDescription>
          </Alert>
        ) : null}
        {state.status === "ready" ? (
          <ReadyDataLoss list={state.list} stale={state.stale} />
        ) : null}

        <FieldGroup>
          <Field
            data-invalid={state.status === "failed" || undefined}
            data-disabled={
              state.status !== "ready" || pending ? true : undefined
            }
          >
            <FieldLabel htmlFor={inputId}>
              Type <strong className="font-mono">{confirmPhrase}</strong> to
              confirm
            </FieldLabel>
            <Input
              id={inputId}
              value={typedPhrase}
              onChange={(event) => setTypedPhrase(event.target.value)}
              placeholder={confirmPhrase}
              disabled={state.status !== "ready" || pending}
              aria-invalid={state.status === "failed" || undefined}
              onKeyDown={(event) => {
                if (event.key === "Enter" && canConfirm) {
                  event.preventDefault();
                  void confirmDataLoss();
                }
              }}
            />
            <FieldDescription>
              The phrase must match exactly. Named Data Loss is what Inngest
              will send to rust.
            </FieldDescription>
            {state.status === "failed" ? (
              <FieldError>Fix the error before trying again.</FieldError>
            ) : null}
          </Field>
        </FieldGroup>

        <AlertDialogFooter>
          <AlertDialogCancel disabled={pending}>Cancel</AlertDialogCancel>
          <AlertDialogAction
            variant="destructive"
            disabled={!canConfirm}
            onClick={() => void confirmDataLoss()}
          >
            {pending ? <Spinner data-icon="inline-start" /> : null}
            {pending ? pendingLabel : actionLabel}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}

function ReadyDataLoss({
  list,
  stale,
}: {
  list: DataLossList;
  stale: boolean;
}) {
  const empty = list.rust.length === 0 && list.cloud.length === 0;

  return (
    <div className="flex flex-col gap-4">
      {stale ? (
        <Alert>
          <RefreshCwIcon />
          <AlertTitle>Data Loss changed</AlertTitle>
          <AlertDescription>
            Rust reported identities that were not in this confirmation. Review
            the updated list and type the phrase again.
          </AlertDescription>
        </Alert>
      ) : null}
      {empty ? (
        <Empty variant="no-results">
          <EmptyHeader>
            <EmptyTitle>Nothing to destroy</EmptyTitle>
            <EmptyDescription>
              There is no named Data Loss and no Cloud record to remove.
            </EmptyDescription>
          </EmptyHeader>
        </Empty>
      ) : null}
      {list.rust.length > 0 ? (
        <section className="flex flex-col gap-2" aria-label="Named Data Loss">
          <strong>Named Data Loss</strong>
          <ItemGroup className="gap-2">
            {list.rust.map((identity) => (
              <RustIdentityItem
                key={dataLossIdentityKey(identity)}
                identity={identity}
              />
            ))}
          </ItemGroup>
        </section>
      ) : null}
      {list.cloud.length > 0 ? (
        <section
          className="flex flex-col gap-2"
          aria-label="Cloud records to remove (not sent to rust)"
        >
          <strong>Cloud records to remove (not sent to rust)</strong>
          <ItemGroup className="gap-2">
            {list.cloud.map((row) => (
              <CloudRowItem key={cloudRowKey(row)} row={row} />
            ))}
          </ItemGroup>
        </section>
      ) : null}
    </div>
  );
}

function RustIdentityItem({ identity }: { identity: DataLossIdentity }) {
  return (
    <Item variant="outline" size="sm">
      <ItemMedia>
        <Badge variant="outline">{identity.kind}</Badge>
      </ItemMedia>
      <ItemContent>
        <ItemTitle>{dataLossIdentityLabel(identity)}</ItemTitle>
      </ItemContent>
    </Item>
  );
}

function CloudRowItem({ row }: { row: CloudRowLoss }) {
  return (
    <Item variant="outline" size="sm">
      <ItemMedia>
        <Badge variant="secondary">{row.kind}</Badge>
      </ItemMedia>
      <ItemContent>
        <ItemTitle>{row.name}</ItemTitle>
      </ItemContent>
    </Item>
  );
}

function GatheringDataLoss() {
  return (
    <div aria-label="Preparing Data Loss" className="flex flex-col gap-3">
      <div className="flex items-center gap-2 text-muted-foreground">
        <Spinner />
        Preparing Data Loss…
      </div>
      <Skeleton className="h-24 w-full" />
    </div>
  );
}
