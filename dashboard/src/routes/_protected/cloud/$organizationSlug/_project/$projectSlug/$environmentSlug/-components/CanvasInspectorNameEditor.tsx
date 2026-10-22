import { useState } from "react";
import { PencilIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { CommandDialog } from "#/components/ui/command";
import { FieldError } from "#/components/ui/field";
import { DnsNameHint } from "#/components/dns-name-hint";
import { dnsName } from "#/utils/dns-name";
import { InputGroupInput } from "#/components/ui/input-group";
import { SourcePickerInput, SourcePickerLayout } from "#/components/source-picker-layout";
import { Result, Schema } from "effect";
import {
  strictParseOptions,
  type StringSchema,
} from "#/lib/schema";

/**
 * Click-to-edit h2 used in the canvas inspector overlay. Shared by the
 * service, volume and config drawers so renaming behaves identically. Their
 * names are DNS labels, so the draft is converted and the saved name previewed;
 * callers supply the schema the converted name must pass (such as "not taken")
 * and an optimistic rename that owns its failure toast.
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
  const converted = dnsName(draftValue);
  const parsed = Schema.decodeUnknownResult(schema)(converted.name, strictParseOptions);
  const error = converted.error ?? (converted.name && Result.isFailure(parsed)
    ? parsed.failure instanceof Error
      ? parsed.failure.message
      : "Invalid value"
    : null);
  const isDirty = converted.name !== "" && converted.name !== value;

  function openEditor() {
    setDraftValue(value);
    setIsOpen(true);
  }

  function handleSubmit() {
    if (error || Result.isFailure(parsed) || !isDirty) {
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
      <h2 className="min-w-0">
        <Button
          type="button"
          variant="ghost"
          size="lg"
          className="w-full min-w-0 justify-start text-left text-base font-semibold hover:cursor-text"
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
      </h2>

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
          {error ? <FieldError>{error}</FieldError> : <DnsNameHint name={converted} />}
          <Button type="submit" disabled={Boolean(error) || !isDirty}>Save</Button>
        </SourcePickerLayout>
        </form>
      </CommandDialog>
    </>
  );
}
