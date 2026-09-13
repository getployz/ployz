import { useState } from "react";
import { Button } from "#/components/ui/button";
import { Command, CommandSeparator } from "#/components/ui/command";
import { CommandDialog } from "#/components/ui/command";
import { FieldError } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { Result, Schema } from "effect";
import { cn } from "#/lib/utils";
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
    setIsOpen(true);
  }

  async function handleSubmit() {
    if (Result.isFailure(parsed) || !isDirty || isPending) {
      return;
    }

    setIsPending(true);

    try {
      await onRename(parsed.success);
      setDraftValue(parsed.success);
      setIsOpen(false);
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
        className={cn(
          "-mx-3 h-auto w-full min-w-0 justify-start px-3 py-2 text-left text-3xl font-semibold tracking-tight hover:cursor-text",
          isChanged &&
            "border-changed-border bg-changed-soft ring-3 ring-changed/10",
        )}
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
        <div className="flex w-full min-w-0 flex-col gap-2">
          <Command shouldFilter={false}>
            <div className="p-1">
              <Input
                aria-label={editTitle}
                value={draftValue}
                placeholder={placeholder}
                disabled={isPending}
                variant="title"
                data-changed={isChanged}
                autoFocus
                onFocus={(event) => event.currentTarget.select()}
                onChange={(event) => setDraftValue(event.target.value)}
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
                    void handleSubmit();
                  }
                }}
              />
            </div>
            {error ? (
              <>
                <CommandSeparator />
                <div className="px-3 pb-3 pt-2">
                  <FieldError>{error}</FieldError>
                </div>
              </>
            ) : null}
          </Command>
        </div>
      </CommandDialog>
    </>
  );
}
