import * as React from "react"
import { CheckIcon, XIcon } from "lucide-react"
import { cn } from "#/lib/utils"
import { FieldError } from "#/components/ui/field"
import { Spinner } from "#/components/ui/spinner"
import {
  InputGroup,
  InputGroupAddon,
  InputGroupButton,
  InputGroupInput,
  InputGroupText,
  InputGroupTextarea,
} from "#/components/ui/input-group"

/** The last confirmed write: still running, or refused with why and a way to send it again. */
export type ConfirmStatus =
  | { phase: "saving" }
  | { phase: "failed"; reason: React.ReactNode; onRetry: () => void }

export type ConfirmableInputProps = Omit<
  React.ComponentProps<"input"> & React.ComponentProps<"textarea">,
  "defaultValue" | "onChange" | "onKeyDown" | "onSubmit" | "value"
> & {
  value: string
  onKeyDown?: React.KeyboardEventHandler<HTMLInputElement | HTMLTextAreaElement>
  onValueChange: (value: string) => void
  onConfirm?: () => void
  onCancel?: () => void
  isDirty?: boolean
  isChanged?: boolean
  suffix?: React.ReactNode
  error?: React.ReactNode
  status?: ConfirmStatus
  multiline?: boolean
  startAddon?: React.ReactNode
  endAddon?: React.ReactNode
  renderInput?: (
    props: Omit<React.ComponentProps<typeof InputGroupInput>, "value" | "onChange">,
  ) => React.ReactNode
}

function ConfirmableInput({
  value,
  onValueChange,
  onConfirm,
  onCancel,
  isDirty = false,
  isChanged = false,
  suffix,
  error,
  status,
  className,
  multiline = false,
  startAddon,
  endAddon,
  renderInput,
  onKeyDown,
  ...props
}: ConfirmableInputProps) {
  function updateDraftValue(
    e: React.ChangeEvent<HTMLInputElement | HTMLTextAreaElement>,
  ) {
    onValueChange(e.target.value)
  }

  function handleConfirm() {
    if (!isDirty) {
      return
    }

    onConfirm?.()
  }

  function handleCancel() {
    if (!isDirty) {
      return
    }

    onCancel?.()
  }

  function handleKeyDown(
    e: React.KeyboardEvent<HTMLInputElement | HTMLTextAreaElement>,
  ) {
    if (e.key === "Enter" && isDirty) {
      if (multiline && !e.metaKey && !e.ctrlKey) {
        onKeyDown?.(e)
        return
      }
      e.preventDefault()
      handleConfirm()
      return
    }
    if (e.key === "Escape" && isDirty) {
      e.preventDefault()
      e.stopPropagation()
      e.nativeEvent.stopImmediatePropagation()
      handleCancel()
      return
    }
    onKeyDown?.(e)
  }

  const saving = status?.phase === "saving" && !isDirty
  const failed = !error && status?.phase === "failed" ? status : undefined

  return (
    <div className={cn("flex flex-col gap-1", className)}>
      <InputGroup data-changed={isChanged || undefined} aria-busy={saving || undefined}>
        {startAddon ? <InputGroupAddon>{startAddon}</InputGroupAddon> : null}
        {multiline ? (
          <InputGroupTextarea
            {...props}
            value={value}
            aria-invalid={props["aria-invalid"] ?? !!error}
            disabled={props.disabled}
            onChange={updateDraftValue}
            onKeyDown={handleKeyDown}
          />
        ) : renderInput ? (
          renderInput({
            ...props,
            "aria-invalid": props["aria-invalid"] ?? !!error,
            disabled: props.disabled,
            onKeyDown: handleKeyDown,
          })
        ) : (
          <InputGroupInput
            {...props}
            value={value}
            aria-invalid={props["aria-invalid"] ?? !!error}
            disabled={props.disabled}
            onChange={updateDraftValue}
            onKeyDown={handleKeyDown}
          />
        )}
        {suffix || endAddon || isDirty || saving ? (
          <InputGroupAddon align="inline-end">
            {suffix ? <InputGroupText>{suffix}</InputGroupText> : null}
            {endAddon}
            {saving ? <Spinner aria-label="Saving" className="mx-1 size-3.5 text-muted-foreground" /> : null}
            {isDirty ? (
              <>
                <InputGroupButton
                  size="icon-xs"
                  variant="ghost"
                  onClick={handleCancel}
                  aria-label="Cancel"
                >
                  <XIcon />
                </InputGroupButton>
                <InputGroupButton
                  size="icon-xs"
                  variant="ghost"
                  onClick={handleConfirm}
                  aria-label="Confirm"
                >
                  <CheckIcon />
                </InputGroupButton>
              </>
            ) : null}
          </InputGroupAddon>
        ) : null}
      </InputGroup>
      {error ? <FieldError>{error}</FieldError> : null}
      {failed ? (
        <FieldError className="flex items-center gap-2">
          <span>{failed.reason}</span>
          <button type="button" onClick={failed.onRetry} className="font-medium underline underline-offset-2">
            Retry
          </button>
        </FieldError>
      ) : null}
    </div>
  )

}

export { ConfirmableInput }
