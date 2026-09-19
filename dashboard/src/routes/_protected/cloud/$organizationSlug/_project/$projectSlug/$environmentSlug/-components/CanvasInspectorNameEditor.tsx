import { useState } from "react";
import { Button } from "#/components/ui/button";
import { CommandDialog } from "#/components/ui/command";
import { FieldError } from "#/components/ui/field";
import { InputGroupAddon, InputGroupInput } from "#/components/ui/input-group";
import { SourcePickerInput, SourcePickerLayout } from "#/components/source-picker-layout";
import { Spinner } from "#/components/ui/spinner";
import { Result, Schema } from "effect";
import {
  strictParseOptions,
  type StringSchema,
} from "#/modules/environment-design/schema";

/**
 * Click-to-edit heading used in the canvas inspector overlay. Shared by the
 * service drawer and the Variable Group drawer so renaming behaves identically;
 * callers supply the validation schema and the persistence callback.
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
  onRename: (value: string) => Promise<void>;
  editTitle: string;
  editDescription: string;
  placeholder: string;
  isChanged?: boolean;
  baselineValue?: string;
}) {
  const [isOpen, setIsOpen] = useState(false);
  const [draftValue, setDraftValue] = useState("");
  const [isPending, setIsPending] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const parsed = Schema.decodeUnknownResult(schema)(draftValue, strictParseOptions);
  const error = Result.isFailure(parsed)
    ? parsed.failure instanceof Error
      ? parsed.failure.message
      : "Invalid value"
    : null;
  const isDirty = draftValue !== value;

  function openEditor() {
    setDraftValue(value);
    setIsPending(false);
    setSaveError(null);
    setIsOpen(true);
  }

  async function handleSubmit() {
    if (Result.isFailure(parsed) || !isDirty || isPending) {
      return;
    }

    setIsPending(true);
    setSaveError(null);

    try {
      await onRename(parsed.success);
      setDraftValue(parsed.success);
      setIsOpen(false);
    } catch {
      setSaveError("Could not save the name. Try again.");
    } finally {
      setIsPending(false);
    }
  }

  function handleClose() {
    if (isPending) {
      return;
    }

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
        <SourcePickerLayout title={editTitle}>
          <SourcePickerInput onBack={handleClose} disabled={isPending}>
              <InputGroupInput
                aria-label={editTitle}
                aria-invalid={Boolean(error || saveError) || undefined}
                value={draftValue}
                placeholder={placeholder}
                disabled={isPending}
                data-changed={isChanged}
                autoFocus
                onFocus={(event) => event.currentTarget.select()}
                onChange={(event) => {
                  setDraftValue(event.target.value);
                  setSaveError(null);
                }}
                onKeyDown={(event) => {
                  if (event.key === "Escape") {
                    event.preventDefault();
                    event.stopPropagation();
                    event.nativeEvent.stopImmediatePropagation();
                    handleClose();
                    return;
                  }

                  if (event.key === "Enter") {
                    event.preventDefault();
                    event.stopPropagation();
                    if (!event.nativeEvent.isComposing && !event.repeat) void handleSubmit();
                  }
                }}
              />
              {isPending ? <InputGroupAddon align="inline-end"><Spinner /></InputGroupAddon> : null}
          </SourcePickerInput>
          {error || saveError ? <FieldError>{error ?? saveError}</FieldError> : null}
        </SourcePickerLayout>
      </CommandDialog>
    </>
  );
}
