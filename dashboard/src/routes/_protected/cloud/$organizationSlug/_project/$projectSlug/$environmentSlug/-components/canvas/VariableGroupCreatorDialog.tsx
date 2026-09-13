import { useState } from "react";
import { Loader2Icon } from "lucide-react";
import { toast } from "sonner";
import { Button } from "#/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "#/components/ui/dialog";
import {
  Field,
  FieldGroup,
  FieldLabel,
} from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import type { FlowPosition } from "./types";

export function VariableGroupCreatorDialog({
  open,
  onOpenChange,
  position,
  onCreate,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  position: FlowPosition;
  onCreate: (input: { name: string; position: FlowPosition }) => Promise<void>;
}) {
  const [name, setName] = useState("Database");
  const [isSubmitting, setIsSubmitting] = useState(false);
  const trimmedName = name.trim();

  async function handleSubmit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!trimmedName || isSubmitting) {
      return;
    }

    setIsSubmitting(true);
    try {
      await onCreate({ name: trimmedName, position });
      onOpenChange(false);
      setName("Database");
    } catch (error) {
      toast.error(
        error instanceof Error
          ? error.message
          : "The Variable Group couldn’t be created. Check the name and try again.",
      );
    } finally {
      setIsSubmitting(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <form onSubmit={(event) => void handleSubmit(event)}>
          <DialogHeader>
            <DialogTitle>Create Variable Group</DialogTitle>
          </DialogHeader>
          <FieldGroup className="py-4">
            <Field>
              <FieldLabel htmlFor="variable-group-name">Name</FieldLabel>
              <Input
                id="variable-group-name"
                value={name}
                onChange={(event) => setName(event.target.value)}
                autoComplete="off"
                autoFocus
              />
            </Field>
          </FieldGroup>
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
              disabled={isSubmitting}
            >
              Cancel
            </Button>
            <Button type="submit" disabled={!trimmedName || isSubmitting}>
              {isSubmitting ? <Loader2Icon data-icon="inline-start" /> : null}
              {isSubmitting ? "Creating…" : "Create Variable Group"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
