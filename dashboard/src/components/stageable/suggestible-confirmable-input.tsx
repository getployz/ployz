import * as React from "react"
import { Combobox as ComboboxPrimitive } from "@base-ui/react/combobox"
import {
  Combobox,
  ComboboxContent,
  ComboboxEmpty,
  ComboboxItem,
  ComboboxList,
  ComboboxTrigger,
} from "#/components/ui/combobox"
import { InputGroupButton, InputGroupInput } from "#/components/ui/input-group"
import { Skeleton } from "#/components/ui/skeleton"
import {
  ConfirmableInput,
  type ConfirmableInputProps,
} from "#/components/stageable/confirmable-input"

type SuggestibleConfirmableInputProps = ConfirmableInputProps & {
  suggestions: string[]
  suggestionsLoading?: boolean
  suggestionsMessage?: string
  suggestionsNotice?: string
  onSuggestionSelect?: (value: string) => void
}

export function SuggestibleConfirmableInput({
  suggestions,
  suggestionsLoading = false,
  suggestionsMessage,
  suggestionsNotice,
  onSuggestionSelect,
  ...props
}: SuggestibleConfirmableInputProps) {
  const [open, setOpen] = React.useState(false)
  const [highlighted, setHighlighted] = React.useState<string>()

  return (
    <Combobox
      items={suggestions}
      value={props.value || null}
      inputValue={props.value}
      onInputValueChange={(next, details) => {
        if (details.reason !== "item-press" || !onSuggestionSelect) props.onValueChange(next)
      }}
      onValueChange={(next) => {
        if (next !== null) (onSuggestionSelect ?? props.onValueChange)(next)
      }}
      onItemHighlighted={(item) => setHighlighted(item)}
      open={open}
      onOpenChange={(next) => {
        setOpen(next)
        if (!next) setHighlighted(undefined)
      }}
      openOnInputClick
      disabled={props.disabled}
    >
      <ConfirmableInput
        {...props}
        endAddon={
          <InputGroupButton
            size="icon-xs"
            variant="ghost"
            render={<ComboboxTrigger />}
            aria-label="Show suggestions"
          />
        }
        renderInput={(inputProps) => (
          <ComboboxPrimitive.Input
            {...inputProps}
            render={<InputGroupInput />}
            onFocus={(event) => {
              setOpen(true)
              inputProps.onFocus?.(event)
            }}
            onKeyDown={(event) => {
              if (open && event.key === "Escape") return
              if (open && event.key === "Enter" && highlighted !== undefined) return
              if (event.key === "Enter") setOpen(false)
              inputProps.onKeyDown?.(event)
            }}
          />
        )}
      />
      <ComboboxContent>
        {suggestionsNotice ? (
          <p className="px-2 py-1.5 text-muted-foreground text-xs">{suggestionsNotice}</p>
        ) : null}
        <ComboboxEmpty>
          {suggestionsLoading ? (
            <div role="status" aria-label="Loading suggestions" className="flex flex-col gap-2">
              <Skeleton className="h-4 w-2/3" /><Skeleton className="h-4 w-1/2" />
            </div>
          ) : (
            suggestionsMessage ?? "No matches. Enter a custom path."
          )}
        </ComboboxEmpty>
        <ComboboxList>
          {(item: string) => <ComboboxItem key={item} value={item}>{item}</ComboboxItem>}
        </ComboboxList>
      </ComboboxContent>
    </Combobox>
  )
}
