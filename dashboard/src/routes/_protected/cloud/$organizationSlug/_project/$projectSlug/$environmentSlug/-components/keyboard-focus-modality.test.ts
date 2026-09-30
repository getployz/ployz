// @vitest-environment jsdom

import { cleanup, renderHook } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { useKeyboardFocusModality } from "./keyboard-focus-modality";

afterEach(cleanup);

const mode = () => document.documentElement.dataset["navigation"];
const press = (target: EventTarget, key: string) => target.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true }));

it("turns keyboard mode on for Tab anywhere, and for / and the arrows only outside a field; a pointer press ends it", () => {
  renderHook(() => useKeyboardFocusModality());
  const field = document.body.appendChild(document.createElement("input"));
  try {
    press(field, "/");
    press(field, "ArrowLeft");
    expect(mode()).toBeUndefined();
    press(field, "Tab");
    expect(mode()).toBe("keyboard");
    window.dispatchEvent(new Event("pointerdown"));
    expect(mode()).toBeUndefined();
    press(document.body, "/");
    expect(mode()).toBe("keyboard");
  } finally {
    field.remove();
  }
});
