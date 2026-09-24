import * as React from "react"
import { CheckIcon, XIcon } from "lucide-react"
import { cn } from "#/lib/utils"
import { FieldError } from "#/components/ui/field"
import {
  InputGroup,
  InputGroupAddon,
  InputGroupButton,
  InputGroupInput,
  InputGroupText,
  InputGroupTextarea,
} from "#/components/ui/input-group"

export type ConfirmableInputProps = Omit<
  React.ComponentProps<"input"> & React.ComponentProps<"textarea">,
  "defaultValue" | "onChange" | "onSubmit" | "value"
> & {
  value: string
  onValueChange: (value: string) => void
  onConfirm?: () => void
  onCancel?: () => void
  isDirty?: boolean
  isChanged?: boolean
  suffix?: React.ReactNode
  error?: React.ReactNode
  multiline?: boolean
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
  className,
  multiline = false,
  endAddon,
  renderInput,
  ...props
}: ConfirmableInputProps) {
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
        {suffix || endAddon || isDirty ? (
          <InputGroupAddon align="inline-end">
            {suffix ? <InputGroupText>{suffix}</InputGroupText> : null}
            {endAddon}
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
    </div>
  )

}

export { ConfirmableInput }
