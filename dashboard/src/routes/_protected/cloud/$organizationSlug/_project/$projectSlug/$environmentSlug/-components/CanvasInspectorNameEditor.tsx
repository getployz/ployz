import { useState } from "react";
import { PencilIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { CommandDialog } from "#/components/ui/command";
import { FieldError } from "#/components/ui/field";
import { InputGroupInput } from "#/components/ui/input-group";
import { SourcePickerInput, SourcePickerLayout } from "#/components/source-picker-layout";
import { Result, Schema } from "effect";
import {
  strictParseOptions,
  type StringSchema,
} from "#/modules/environment-design/schema";

/**
 * Click-to-edit heading used in the canvas inspector overlay. Shared by the
 * service and volume drawers so renaming behaves identically; callers supply
 * the validation schema and an optimistic rename that owns its failure toast.
 */
export function CanvasInspectorNameEditor({
  value,
  schema,
  onRename,
  editTitle,
  editDescription,
  placeholder,
  isChanged = false,
  baselineValue,
}: {
  value: string;
  schema: StringSchema;
  onRename: (value: string) => void;
  editTitle: string;
  editDescription: string;
  placeholder: string;
  isChanged?: boolean;
  baselineValue?: string;
}) {
  const [isOpen, setIsOpen] = useState(false);
  const [draftValue, setDraftValue] = useState("");
  const parsed = Schema.decodeUnknownResult(schema)(draftValue, strictParseOptions);
  const error = Result.isFailure(parsed)
    ? parsed.failure instanceof Error
      ? parsed.failure.message
      : "Invalid value"
    : null;
  const isDirty = draftValue !== value;

  function openEditor() {
    setDraftValue(value);
    setIsOpen(true);
  }

  function handleSubmit() {
    if (Result.isFailure(parsed) || !isDirty) {
      return;
    }
    onRename(parsed.success);
    setIsOpen(false);
  }

  function handleClose() {
    setDraftValue(value);
    setIsOpen(false);
  }

  return (
    <>
      <Button
        type="button"
        variant="ghost"
        size="lg"
        className="w-full min-w-0 justify-start text-left hover:cursor-text"
        data-changed={isChanged}
        title={
          isChanged && baselineValue != null
            ? `Deployed: ${baselineValue}`
            : editTitle
        }
        onClick={() => {
          openEditor();
        }}
      >
        <span className="truncate">{value}</span>
        <PencilIcon data-icon="inline-end" />
      </Button>

      <CommandDialog
        open={isOpen}
        onOpenChange={(nextOpen) => {
          if (nextOpen) {
            openEditor();
            return;
          }

          handleClose();
        }}
        title={editTitle}
        description={editDescription}
        className="max-w-md overflow-visible"
        surface="unstyled"
        showCloseButton={false}
      >
        <form onSubmit={(event) => { event.preventDefault(); handleSubmit(); }}>
        <SourcePickerLayout title={editTitle}>
          <SourcePickerInput onBack={handleClose}>
              <InputGroupInput
                aria-label={editTitle}
                aria-invalid={Boolean(error) || undefined}
                value={draftValue}
                placeholder={placeholder}
                data-changed={isChanged}
                autoFocus
                onFocus={(event) => event.currentTarget.select()}
                onChange={(event) => setDraftValue(event.target.value)}

              />
          </SourcePickerInput>
          {error ? <FieldError>{error}</FieldError> : null}
        </SourcePickerLayout>
        <button type="submit" hidden disabled={Boolean(error) || !isDirty}>Save</button>
        </form>
      </CommandDialog>
    </>
  );
}
