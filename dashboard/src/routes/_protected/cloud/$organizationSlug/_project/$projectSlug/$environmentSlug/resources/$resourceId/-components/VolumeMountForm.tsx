import { PlusIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { Field, FieldGroup, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "#/components/ui/select";
import type { VolumeDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/resources/$resourceId/-components/useVolumeDrawerState";

type VolumeService = VolumeDrawerState["services"][number];

export function VolumeMountForm({
  addError,
  addMountPath,
  addServiceId,
  availableServices,
  pending,
  onAttach,
  onMountPathChange,
  onServiceChange,
}: {
  addError: string | null;
  addMountPath: string;
  addServiceId: string;
  availableServices: VolumeService[];
  pending: boolean;
  onAttach: () => void;
  onMountPathChange: (value: string) => void;
  onServiceChange: (value: string | null) => void;
}) {
  if (availableServices.length === 0) {
    return (
      <p className="text-sm text-muted-foreground">
        Every service in this environment already mounts this volume.
      </p>
    );
  }

  return (
    <FieldGroup>
      <Field data-invalid={addError ? true : undefined}>
        <FieldLabel>Mount on a service</FieldLabel>
        <div className="flex gap-2">
          <Select value={addServiceId} onValueChange={onServiceChange}>
            <SelectTrigger className="flex-1">
              <SelectValue placeholder="Select a service" />
            </SelectTrigger>
            <SelectContent>
              <SelectGroup>
                {availableServices.map((service) => (
                  <SelectItem key={service.id} value={service.id}>
                    {service.name}
                  </SelectItem>
                ))}
              </SelectGroup>
            </SelectContent>
          </Select>
          <Input
            className="flex-1"
            value={addMountPath}
            aria-invalid={addError ? true : undefined}
            placeholder="/data"
            onChange={(event) => onMountPathChange(event.target.value)}
          />
          <Button disabled={pending} onClick={onAttach}>
            <PlusIcon data-icon="inline-start" />
            Mount
          </Button>
        </div>
        {addError ? (
          <p className="text-sm text-destructive">{addError}</p>
        ) : null}
      </Field>
    </FieldGroup>
  );
}
