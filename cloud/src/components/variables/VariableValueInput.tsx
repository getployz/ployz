import { Input } from "#/components/ui/input";
import { ReferenceSuggestionList } from "#/components/variables/reference-suggestions";
import { useVariableAutocomplete } from "#/components/variables/use-variable-autocomplete";
import type { ReferenceTarget } from "#/modules/environment-design/variable-autocomplete";

type InputProps = Omit<React.ComponentProps<typeof Input>, "value" | "onChange">;

/**
 * A single-line value input with inline `${{ }}` reference autocomplete. Behaves
 * like a normal `Input`; type `${{` to get a slug-namespaced suggestion list of
 * referenceable variables, managed exports, and variable groups.
 */
export function VariableValueInput({
  value,
  onValueChange,
  targets,
  onKeyDown,
  ...inputProps
}: InputProps & {
  value: string;
  onValueChange: (value: string) => void;
  targets: ReferenceTarget[];
}) {
  const autocomplete = useVariableAutocomplete<HTMLInputElement>({
    value,
    onValueChange,
    targets,
  });

  return (
    <div className="relative">
      <Input
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
        autoComplete="off"
        spellCheck={false}
        {...inputProps}
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
