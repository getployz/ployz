import { useRef, useState } from "react";
import {
  filterReferenceTargets,
  type ReferenceTarget,
} from "#/modules/environment-design/variable-autocomplete";
import { buildRefToken, caretToken } from "#/modules/environment-design/variable-template";

type EditableElement = HTMLInputElement | HTMLTextAreaElement;

/**
 * Inline `${{ }}` autocomplete for a controlled text field. Detects the token
 * under the caret, filters reference targets, and handles keyboard navigation +
 * insertion while focus stays in the field (so it works inside both `<input>`
 * and `<textarea>`). `onKeyDown` returns true when it consumed the event so the
 * caller can skip its own handling (e.g. Enter-to-save).
 */
export function useVariableAutocomplete<T extends EditableElement>(input: {
  value: string;
  onValueChange: (value: string) => void;
  targets: ReferenceTarget[];
}) {
  const ref = useRef<T>(null);
  const [caret, setCaret] = useState<number | null>(null);
  const [activeIndex, setActiveIndex] = useState(0);
  const [dismissed, setDismissed] = useState(false);

  const token = caret != null && !dismissed ? caretToken(input.value, caret) : null;
  const suggestions = token ? filterReferenceTargets(input.targets, token) : [];
  const open = suggestions.length > 0;
  const clampedIndex = Math.min(activeIndex, Math.max(0, suggestions.length - 1));

  function syncCaret() {
    const element = ref.current;
    if (element) setCaret(element.selectionStart);
  }

  function handleChange(event: React.ChangeEvent<T>) {
    setDismissed(false);
    setActiveIndex(0);
    setCaret(event.target.selectionStart);
    input.onValueChange(event.target.value);
  }

  function applySelection(target: ReferenceTarget) {
    if (!token) return;
    const refToken = buildRefToken({ ownerSlug: target.ownerSlug, key: target.key });
    const before = input.value.slice(0, token.start) + refToken;
    input.onValueChange(before + input.value.slice(token.end));
    setDismissed(true);
    requestAnimationFrame(() => {
      const element = ref.current;
      if (!element) return;
      element.focus();
      element.setSelectionRange(before.length, before.length);
      setCaret(before.length);
    });
  }

  function handleKeyDown(event: React.KeyboardEvent<T>): boolean {
    if (!open) return false;
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        setActiveIndex((clampedIndex + 1) % suggestions.length);
        return true;
      case "ArrowUp":
        event.preventDefault();
        setActiveIndex((clampedIndex - 1 + suggestions.length) % suggestions.length);
        return true;
      case "Enter":
      case "Tab": {
        const target = suggestions[clampedIndex];
        if (!target) return false;
        event.preventDefault();
        applySelection(target);
        return true;
      }
      case "Escape":
        event.preventDefault();
        setDismissed(true);
        return true;
      default:
        return false;
    }
  }

  return {
    ref,
    open,
    suggestions,
    activeIndex: clampedIndex,
    setActiveIndex,
    onChange: handleChange,
    onKeyDown: handleKeyDown,
    onSelect: applySelection,
    syncCaret,
    dismiss: () => setDismissed(true),
  };
}
