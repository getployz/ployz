import { useState } from "react";
import { NetworkIcon, PencilIcon } from "lucide-react";
import { Result, Schema } from "effect";
import { Button } from "#/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "#/components/ui/dialog";
import { Field, FieldDescription, FieldLabel } from "#/components/ui/field";
import { servicePrivateDnsSchema } from "#/modules/environment-design/services";
import { strictParseOptions } from "#/modules/environment-design/schema";
import { SERVICE_DEPLOYMENT_DIFF_PATHS } from "#/modules/services/service-deployment-diff/fields";
import { ServiceSettingInput } from "./ServiceSettingInput";
import type { ServiceDrawerState } from "./useServiceDrawerState";
import { DomainRowShell, DomainTitle } from "./domain-row";

export function PrivateEndpointField({ state }: { state: ServiceDrawerState }) {
  const [editing, setEditing] = useState(false);
  const { service, collection, diff } = state;
  const privateDnsDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.privateDns);
  const privateHostname = `${service.privateDns}.internal`;
  return (
    <Field data-changed={privateDnsDiff.changed || undefined}>
      <FieldLabel>Private Networking</FieldLabel>
      <FieldDescription>
        Communicate with this service from within the environment.
      </FieldDescription>
      <DomainRowShell
        icon={<NetworkIcon />}
        changed={privateDnsDiff.changed}
        actions={
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            aria-label="Edit private endpoint"
            onClick={() => setEditing(true)}
          >
            <PencilIcon />
          </Button>
        }
      >
        <DomainTitle
          hostname={privateHostname}
          copyLabel="Copy private hostname"
        />
        <div className="truncate text-muted-foreground text-sm">
          → or just <span className="font-mono">{service.privateDns}</span>
        </div>
      </DomainRowShell>
      {editing ? (
        <Dialog open onOpenChange={(open) => !open && setEditing(false)}>
          <DialogContent>
            <DialogHeader>
              <DialogTitle>Edit private endpoint</DialogTitle>
              <DialogDescription>
                The name other services in this environment use to reach it.
              </DialogDescription>
            </DialogHeader>
            <ServiceSettingInput
              ariaLabel="Private endpoint name"
              placeholder="api"
              value={service.privateDns}
              isChanged={privateDnsDiff.changed}
              baselineLabel={privateDnsDiff.baselineLabel}
              baselineValue={privateDnsDiff.baselineValue}
              validate={(raw) => {
                const parsed = Schema.decodeUnknownResult(
                  servicePrivateDnsSchema
                )(raw, strictParseOptions);
                return Result.isFailure(parsed)
                  ? parsed.failure instanceof Error
                    ? parsed.failure.message
                    : "Invalid value"
                  : null;
              }}
              onCommit={(raw) => {
                const tx = collection.update(service.id, (draft) => {
                  draft.privateDns = raw;
                });
                setEditing(false);
                return tx;
              }}
            />
          </DialogContent>
        </Dialog>
      ) : null}
    </Field>
  );
}
