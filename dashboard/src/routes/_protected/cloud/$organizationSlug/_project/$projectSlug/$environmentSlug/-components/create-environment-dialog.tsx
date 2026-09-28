import { useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useStillHere } from "#/hooks/use-still-here";
import { useServerFn } from "@tanstack/react-start";
import { getEnvironmentsCollection, getEnvironmentSummariesCollection, environmentSummary } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
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
import { createEnvironmentServerFn } from "#/modules/environment-design/workspace-functions";

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
  const collectionScope = useCollectionScope();
  const [name, setName] = useState("");
  const markHere = useStillHere();
  const navigate = useNavigate();
  const createEnvironment = useServerFn(createEnvironmentServerFn);
  const mutation = useMutation({
    mutationFn: (input: {
      organizationSlug: string;
      projectSlug: string;
      name: string;
      stillHere: () => boolean;
    }) =>
      createEnvironment({
        data: {
          organizationSlug: input.organizationSlug,
          projectSlug: input.projectSlug,
          name: input.name,
        },
      }),
    onSuccess: async (receipt, input) => {
      await getEnvironmentsCollection(input.organizationSlug, collectionScope).writeCommitted(receipt.data);
      await getEnvironmentSummariesCollection(input.organizationSlug, collectionScope).writeCommitted(environmentSummary(receipt.data));
      // A completed creation still belongs to its original scope after navigation.
      if (!input.stillHere()) return;
      onOpenChange(false);
      await navigate(
        getDashboardDestination(
          {
            kind: "environment",
            organizationSlug: input.organizationSlug,
            projectSlug: input.projectSlug,
            environmentSlug: receipt.data.namespace,
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
