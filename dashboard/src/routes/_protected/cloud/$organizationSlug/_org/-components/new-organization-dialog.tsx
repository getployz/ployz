import { useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { toast } from "sonner";
import { Button } from "#/components/ui/button";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "#/components/ui/dialog";
import { Field, FieldGroup, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { InputGroup, InputGroupAddon, InputGroupInput, InputGroupText } from "#/components/ui/input-group";
import { Spinner } from "#/components/ui/spinner";
import { createOrganizationServerFn } from "#/modules/organization/organization-state.functions";
import { slugifySegment } from "#/utils/slug";

/** Creates an Organization the user owns and opens it. Its URL follows the name until edited. */
export function NewOrganizationDialog({ onOpenChange }: { onOpenChange: (open: boolean) => void }) {
  const navigate = useNavigate();
  const [name, setName] = useState("");
  const [editedSlug, setEditedSlug] = useState<string | null>(null);
  const slug = editedSlug ?? slugifySegment(name);
  const mutation = useMutation({
    mutationFn: (data: { name: string; slug: string }) => createOrganizationServerFn({ data }),
    // The session now acts in the new Organization, so the whole document reloads into it.
    onSuccess: (organizationSlug) =>
      navigate({ to: "/cloud/$organizationSlug/~", params: { organizationSlug }, reloadDocument: true }),
    onError: () => toast.error("Couldn't create the organization. Try again."),
  });
  const ready = name.trim() !== "" && slugifySegment(slug) !== "";

  return (
    <Dialog open onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>New organization</DialogTitle>
        </DialogHeader>
        <form
          onSubmit={(event) => {
            event.preventDefault();
            if (ready && !mutation.isPending) mutation.mutate({ name: name.trim(), slug });
          }}
        >
          <FieldGroup>
            <Field>
              <FieldLabel htmlFor="organization-name">Name</FieldLabel>
              <Input
                id="organization-name"
                placeholder="Acme"
                maxLength={64}
                value={name}
                onChange={(event) => setName(event.target.value)}
                autoFocus
              />
            </Field>
            <Field>
              <FieldLabel htmlFor="organization-slug">URL</FieldLabel>
              <InputGroup>
                <InputGroupAddon>
                  <InputGroupText>{window.location.origin}/cloud/</InputGroupText>
                </InputGroupAddon>
                <InputGroupInput
                  id="organization-slug"
                  maxLength={64}
                  spellCheck={false}
                  value={slug}
                  onChange={(event) => setEditedSlug(event.target.value)}
                  onBlur={() => editedSlug !== null && setEditedSlug(slugifySegment(editedSlug))}
                />
              </InputGroup>
            </Field>
          </FieldGroup>
          <DialogFooter className="mt-4">
            <DialogClose render={<Button variant="outline" />}>Cancel</DialogClose>
            <Button type="submit" disabled={!ready || mutation.isPending}>
              {mutation.isPending && <Spinner data-icon="inline-start" />}
              Create
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
