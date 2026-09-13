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

function ConfirmableInput({
  value,
  onValueChange,
  onConfirm,
  onCancel,
  isDirty = false,
  isPending = false,
  isChanged = false,
  suffix,
  error,
  className,
  multiline = false,
  ...props
}: Omit<
  React.ComponentProps<"input"> & React.ComponentProps<"textarea">,
  "defaultValue" | "onChange" | "onSubmit" | "value"
> & {
  value: string
  onValueChange: (value: string) => void
  onConfirm?: () => void
  onCancel?: () => void
  isDirty?: boolean
  isPending?: boolean
  isChanged?: boolean
  suffix?: React.ReactNode
  error?: React.ReactNode
  multiline?: boolean
}) {
  // SAFETY: input and textarea onKeyDown handlers are the same function; their event element types don't unify.
  const onKeyDown = props.onKeyDown as
    | ((event: React.KeyboardEvent<HTMLInputElement | HTMLTextAreaElement>) => void)
    | undefined

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

  return (
    <div className={cn("flex flex-col gap-1", className)}>
      <InputGroup data-changed={isChanged || undefined}>
        {multiline ? (
          <InputGroupTextarea
            {...props}
            value={value}
            aria-invalid={props["aria-invalid"] ?? !!error}
            disabled={props.disabled}
            onChange={updateDraftValue}
            onKeyDown={handleKeyDown}
          />
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
        {suffix || isDirty ? (
          <InputGroupAddon align="inline-end">
            {suffix ? <InputGroupText>{suffix}</InputGroupText> : null}
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
        {isPending ? (
          <InputGroupAddon align="inline-end">
            <Spinner />
          </InputGroupAddon>
        ) : null}
      </InputGroup>
      {error ? <FieldError>{error}</FieldError> : null}
    </div>
  )
}

export { ConfirmableInput }
