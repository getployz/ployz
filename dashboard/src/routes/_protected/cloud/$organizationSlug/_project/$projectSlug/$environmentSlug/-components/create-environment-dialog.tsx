import { useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useStillHere } from "#/hooks/use-still-here";
import { getDashboardDestination } from "#/components/dashboard-navigation-model";
import { Button } from "#/components/ui/button";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "#/components/ui/dialog";
import { Field, FieldError, FieldGroup, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { Spinner } from "#/components/ui/spinner";
import type { EnvironmentId } from "@ployz/sdk";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { dnsLabelError } from "#/modules/config-store/store-services";

/** Creates an empty root Environment and opens its canvas. */
export function CreateEnvironmentDialog({
  onOpenChange,
  organizationSlug,
  projectSlug,
}: {
  onOpenChange: (open: boolean) => void;
  organizationSlug: string;
  projectSlug: string;
}) {
  const [name, setName] = useState("");
  const markHere = useStillHere();
  const navigate = useNavigate();
  const writer = useStoreWriter(organizationSlug);
  const mutation = useMutation({
    mutationFn: async (input: {
      organizationSlug: string;
      projectSlug: string;
      name: string;
      stillHere: () => boolean;
    }) => {
      const invalid = dnsLabelError(input.name);
      if (invalid) throw new Error(invalid);
      // The Store names it as typed, or says why not in the form.
      // SAFETY: an Environment id is a UUID the caller mints; the Store checks it.
      const id = crypto.randomUUID() as EnvironmentId;
      await writer.commit({ command: "create_environment", id, project: input.projectSlug, name: input.name },
        ["invalid_argument", "conflict"]).isPersisted.promise;
      return input.name;
    },
    onSuccess: async (environmentSlug, input) => {
      // A completed creation still belongs to its original scope after navigation.
      if (!input.stillHere()) return;
      onOpenChange(false);
      await navigate(
        getDashboardDestination(
          {
            kind: "environment",
            organizationSlug: input.organizationSlug,
            projectSlug: input.projectSlug,
            environmentSlug,
          },
          "architecture",
        ),
      );
    },
  });

  return (
    <Dialog open onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Add environment</DialogTitle>
          <DialogDescription>
            Create an empty environment with no services or variables.
          </DialogDescription>
        </DialogHeader>
        <form
          onSubmit={(event) => {
            event.preventDefault();
            if (name.trim() && !mutation.isPending)
              mutation.mutate({
                organizationSlug,
                projectSlug,
                name: name.trim(),
                stillHere: markHere(),
              });
          }}
        >
          <FieldGroup>
            <Field>
              <FieldLabel htmlFor="env-name">Name</FieldLabel>
              <Input
                id="env-name"
                placeholder="staging"
                value={name}
                onChange={(event) => setName(event.target.value)}
                autoFocus
              />
            </Field>
            {mutation.isError && (
              <FieldError>{mutation.error.message}</FieldError>
            )}
          </FieldGroup>
          <DialogFooter className="mt-4">
            <DialogClose render={<Button variant="outline" />}>
              Cancel
            </DialogClose>
            <Button type="submit" disabled={!name.trim() || mutation.isPending}>
              {mutation.isPending && <Spinner data-icon="inline-start" />}
              Add environment
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
