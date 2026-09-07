import { Textarea } from "#/components/ui/textarea";
import { ReferenceSuggestionList } from "#/components/variables/reference-suggestions";
import { useVariableAutocomplete } from "#/components/variables/use-variable-autocomplete";
import type { ReferenceTarget } from "#/modules/environment-design/variable-autocomplete";

type TextareaProps = Omit<
  React.ComponentProps<typeof Textarea>,
  "value" | "onChange"
>;

/**
 * A multi-line value editor with inline `${{ }}` reference autocomplete, used by
 * the bulk ENV/JSON raw editor. The suggestion list is multi-line aware via the
 * shared caret-token logic.
 */
export function VariableValueTextarea({
  value,
  onValueChange,
  targets,
  onKeyDown,
  ...textareaProps
}: TextareaProps & {
  value: string;
  onValueChange: (value: string) => void;
  targets: ReferenceTarget[];
}) {
  const autocomplete = useVariableAutocomplete<HTMLTextAreaElement>({
    value,
    onValueChange,
    targets,
  });

  return (
    <div className="relative">
      <Textarea
        ref={autocomplete.ref}
        value={value}
        onChange={autocomplete.onChange}
        onKeyDown={(event) => {
          if (autocomplete.onKeyDown(event)) return;
          onKeyDown?.(event);
        }}
        onClick={autocomplete.syncCaret}
        onKeyUp={autocomplete.syncCaret}
        onBlur={autocomplete.dismiss}
        spellCheck={false}
        {...textareaProps}
      />
      {autocomplete.open ? (
        <ReferenceSuggestionList
          suggestions={autocomplete.suggestions}
          activeIndex={autocomplete.activeIndex}
          onSelect={autocomplete.onSelect}
          onActiveIndexChange={autocomplete.setActiveIndex}
        />
      ) : null}
    </div>
  );
}
