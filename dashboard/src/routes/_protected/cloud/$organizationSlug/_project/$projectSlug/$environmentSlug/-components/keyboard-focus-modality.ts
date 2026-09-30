import { useEffect } from "react";

/** A key pressed while typing into a field moves its caret or types; it doesn't navigate. */
const typing = (target: EventTarget | null) =>
  target instanceof HTMLElement && (target.isContentEditable || target.matches("input, textarea, select"));

/**
 * Canvas nodes draw their focus only while the user navigates by keyboard: Tab, and the arrows or the finder's `/`
 * outside a field, start that, and a pointer press ends it. Enter and Escape change nothing, so a panel opened by a click
 * and closed with Escape hands focus back to its node without drawing it.
 */
export function useKeyboardFocusModality() {
  useEffect(() => {
    const root = document.documentElement;
    const onKey = (event: KeyboardEvent) => {
      const navigating = event.key === "Tab" || (!typing(event.target) && (event.key === "/" || event.key.startsWith("Arrow")));
      if (navigating) root.dataset["navigation"] = "keyboard";
    };
    const onPointer = () => { delete root.dataset["navigation"]; };
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("pointerdown", onPointer, true);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("pointerdown", onPointer, true);
      delete root.dataset["navigation"];
    };
  }, []);
}
