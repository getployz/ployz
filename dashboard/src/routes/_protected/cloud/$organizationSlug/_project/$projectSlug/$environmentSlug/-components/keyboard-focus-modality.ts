import { useEffect } from "react";

/**
 * Canvas nodes draw their focus only while the user navigates by keyboard: Tab, the arrows or the finder's `/` start
 * that, and a pointer press ends it. Enter and Escape change nothing, so a panel opened by a click and closed with
 * Escape hands focus back to its node without drawing it.
 */
export function useKeyboardFocusModality() {
  useEffect(() => {
    const root = document.documentElement;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Tab" || event.key === "/" || event.key.startsWith("Arrow")) root.dataset["navigation"] = "keyboard";
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
