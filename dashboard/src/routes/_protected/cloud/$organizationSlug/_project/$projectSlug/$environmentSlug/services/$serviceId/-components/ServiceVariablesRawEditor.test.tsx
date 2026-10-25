// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import { afterEach, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { storeVariableWriter } from "#/modules/config-store/store-variables";
import type { StoreEdit, useStoreWriter } from "#/modules/config-store/store-write";
import type { SealItem, Sealing } from "#/modules/variables/sealing";
import type { VariableRecord } from "#/modules/variables/variables";
import { ServiceVariablesRawEditor } from "./ServiceVariablesRawEditor";

const originalClipboard = Object.getOwnPropertyDescriptor(navigator, "clipboard");

afterEach(() => {
  if (originalClipboard) Object.defineProperty(navigator, "clipboard", originalClipboard);
  else Reflect.deleteProperty(navigator, "clipboard");
  cleanup();
  document.body.replaceChildren();
});

it("copies the active format's current draft after editing JSON", async () => {
  const writeText = vi.fn(async () => undefined);
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
  render(<Harness onApply={() => ({ isPersisted: { promise: Promise.resolve() } })} />);
  fireEvent.change(screen.getByLabelText("Service variables in ENV format"), { target: { value: "A=old" } });
  fireEvent.click(screen.getByRole("button", { name: "Copy ENV" }));
  await waitFor(() => expect(writeText).toHaveBeenLastCalledWith("A=old"));
  fireEvent.click(screen.getByRole("tab", { name: "JSON" }));
  const current = '{"A":"current"}';
  fireEvent.change(screen.getByLabelText("Service variables in JSON format"), { target: { value: current } });
  fireEvent.click(screen.getByRole("button", { name: "Copy JSON" }));
  await waitFor(() => expect(writeText).toHaveBeenLastCalledWith(current));
});

function Harness({ onApply }: { onApply: () => { isPersisted: { promise: Promise<unknown> } } }) {
  const [open, setOpen] = useState(true);
  return <ServiceVariablesRawEditor open={open} onOpenChange={setOpen} variables={[]} onApply={onApply} />;
}

it("a refused save reopens the editor on what was typed, with the Store's reason", async () => {
  const refused = Promise.reject(new Error("web has no variable named MISSING"));
  refused.catch(() => undefined);
  render(<Harness onApply={() => ({ isPersisted: { promise: refused } })} />);
  fireEvent.change(screen.getByLabelText("Service variables in ENV format"), { target: { value: "A=${{ web.MISSING }}" } });
  fireEvent.click(screen.getByRole("button", { name: "Update variables" }));
  await waitFor(() => expect(screen.getByText("web has no variable named MISSING")).toBeTruthy());
  expect(screen.getByLabelText<HTMLTextAreaElement>("Service variables in ENV format").value).toBe("A=${{ web.MISSING }}");
});

const SECRET: VariableRecord = { id: "SECRET", serviceId: "web", key: "SECRET", description: null, exported: false, value: { type: "sealed" } };
const LOG_LEVEL: VariableRecord = { id: "LOG_LEVEL", serviceId: "web", key: "LOG_LEVEL", description: null, exported: false, value: { type: "plain", value: "info" } };

/** The Service tab's wiring: the raw editor's Apply through the real variables writer, over a Store writer that records each edit. */
function StoreHarness({ sealing, edits, prepare, variables = [SECRET, LOG_LEVEL] }: {
  sealing: "no_server" | "ready";
  edits: StoreEdit[];
  prepare: Sealing["prepare"];
  variables?: VariableRecord[];
}) {
  const [open, setOpen] = useState(true);
  const store = asTestDouble<ReturnType<typeof useStoreWriter>>()({
    edit: (edit: StoreEdit) => { edits.push(edit); return { isPersisted: { promise: Promise.resolve() } }; },
  });
  const writer = storeVariableWriter(store, { project: "shop", environment: "production" }, "web", variables, new Map(),
    { prepare, target: { environment: "0e7c2f3a-5b1d-4c8e-9f60-1a2b3c4d5e6f", service: "7d4e9a12-3c5b-4f6e-8a90-b1c2d3e4f5a6" } });
  return <ServiceVariablesRawEditor open={open} onOpenChange={setOpen} variables={variables} sealing={sealing}
    onApply={({ creates, updates, deletes }) => writer.replace([...creates, ...updates], deletes)} />;
}

const env = () => screen.getByLabelText<HTMLTextAreaElement>("Service variables in ENV format");

it.each(["no_server", "ready"] as const)("a save that leaves out a sealed variable never removes it, sealing %s", async (sealing) => {
  const edits: StoreEdit[] = [];
  render(<StoreHarness sealing={sealing} edits={edits} prepare={vi.fn()} />);
  expect(env().value).toBe("LOG_LEVEL=info");
  fireEvent.change(env(), { target: { value: "" } });
  fireEvent.click(screen.getByRole("button", { name: "Update variables" }));

  expect(edits).toEqual([{ environment: { project: "shop", environment: "production" }, changes: [{ op: "unset", path: "web.env.LOG_LEVEL" }] }]);
});

it("names each sealed variable, and seals the new keys ticked Sealed, across ENV and JSON, while sealing is ready", async () => {
  const edits: StoreEdit[] = [];
  const prepare = vi.fn(async (items: readonly SealItem[]) => items.map((item) => ({ sealedFor: item.target.environment, value: { sealed: "c2VhbGVk", scope: { service: item.target.service }, key: item.key } })));
  render(<StoreHarness sealing="ready" edits={edits} prepare={prepare} />);
  expect(screen.getByText("SECRET")).toBeTruthy();
  expect(env().value).not.toContain("SECRET");

  fireEvent.change(env(), { target: { value: "LOG_LEVEL=info\nTOKEN=tok_live_123456\nREGION=fsn1" } });
  fireEvent.click(screen.getByRole("checkbox", { name: "Seal TOKEN" }));
  fireEvent.click(screen.getByRole("tab", { name: "JSON" }));
  fireEvent.click(screen.getByRole("tab", { name: "ENV" }));
  expect(screen.getByRole("checkbox", { name: "Seal TOKEN" }).getAttribute("aria-checked")).toBe("true");
  expect(screen.getByRole("checkbox", { name: "Seal REGION" }).getAttribute("aria-checked")).toBe("false");
  fireEvent.click(screen.getByRole("button", { name: "Update variables" }));

  expect(edits).toHaveLength(1);
  const [edit] = edits;
  expect(JSON.stringify(edit?.changes)).not.toContain("tok_live_123456");
  expect(await edit?.prepare?.()).toEqual([
    { op: "set", path: "web.env.TOKEN", value: { secret: { sealedFor: "0e7c2f3a-5b1d-4c8e-9f60-1a2b3c4d5e6f", value: { sealed: "c2VhbGVk", scope: { service: "7d4e9a12-3c5b-4f6e-8a90-b1c2d3e4f5a6" }, key: "TOKEN" } } } },
    { op: "set", path: "web.env.REGION", value: "fsn1" },
  ]);
  expect(prepare).toHaveBeenCalledWith([{ key: "TOKEN", value: "tok_live_123456", target: expect.anything() }]);
});

it("a key ticked Sealed still goes Sealed once the cached state turns no_server", async () => {
  const edits: StoreEdit[] = [];
  const prepare = vi.fn(async (items: readonly SealItem[]) => items.map((item) => ({ sealedFor: item.target.environment, value: { sealed: "c2VhbGVk", scope: { service: item.target.service }, key: item.key } })));
  const { rerender } = render(<StoreHarness sealing="ready" edits={edits} prepare={prepare} />);
  fireEvent.change(env(), { target: { value: "LOG_LEVEL=info\nTOKEN=tok_live_123456" } });
  fireEvent.click(screen.getByRole("checkbox", { name: "Seal TOKEN" }));
  rerender(<StoreHarness sealing="no_server" edits={edits} prepare={prepare} />);
  expect(screen.getByRole("checkbox", { name: "Seal TOKEN" }).getAttribute("aria-checked")).toBe("true");
  fireEvent.click(screen.getByRole("button", { name: "Update variables" }));

  expect(edits).toHaveLength(1);
  expect(JSON.stringify(edits[0]?.changes)).not.toContain("tok_live_123456");
  expect(JSON.stringify(await edits[0]?.prepare?.())).not.toContain("tok_live_123456");
  expect(prepare).toHaveBeenCalledWith([{ key: "TOKEN", value: "tok_live_123456", target: expect.anything() }]);
});

it.each([
  ["another value", "other-tab-value"],
  ["the same value", "tok_live_123456"],
])("a key ticked Sealed goes Sealed when another tab makes it plain first, with %s", async (_, theirs) => {
  const edits: StoreEdit[] = [];
  const prepare = vi.fn(async (items: readonly SealItem[]) => items.map((item) => ({ sealedFor: item.target.environment, value: { sealed: "c2VhbGVk", scope: { service: item.target.service }, key: item.key } })));
  const { rerender } = render(<StoreHarness sealing="ready" edits={edits} prepare={prepare} />);
  fireEvent.change(env(), { target: { value: "LOG_LEVEL=info\nTOKEN=tok_live_123456" } });
  fireEvent.click(screen.getByRole("checkbox", { name: "Seal TOKEN" }));
  const token: VariableRecord = { ...LOG_LEVEL, id: "TOKEN", key: "TOKEN", value: { type: "plain", value: theirs } };
  rerender(<StoreHarness sealing="ready" edits={edits} prepare={prepare} variables={[SECRET, LOG_LEVEL, token]} />);
  expect(screen.getByRole("checkbox", { name: "Seal TOKEN" }).getAttribute("aria-checked")).toBe("true");
  fireEvent.click(screen.getByRole("button", { name: "Update variables" }));

  expect(edits).toHaveLength(1);
  expect(JSON.stringify(edits[0]?.changes)).not.toContain("tok_live_123456");
  expect(JSON.stringify(await edits[0]?.prepare?.())).not.toContain("tok_live_123456");
  expect(prepare).toHaveBeenCalledWith([{ key: "TOKEN", value: "tok_live_123456", target: expect.anything() }]);
});

it("holds a value it can't seal, saying why", () => {
  const edits: StoreEdit[] = [];
  render(<StoreHarness sealing="ready" edits={edits} prepare={vi.fn()} />);
  fireEvent.change(env(), { target: { value: "LOG_LEVEL=info\nPORT=8080" } });
  fireEvent.click(screen.getByRole("checkbox", { name: "Seal PORT" }));
  fireEvent.click(screen.getByRole("button", { name: "Update variables" }));

  expect(screen.getByText("PORT can't be sealed: Ployz reads it to route traffic.")).toBeTruthy();
  expect(edits).toEqual([]);
});

it("offers no Sealed choice without a Server", () => {
  render(<StoreHarness sealing="no_server" edits={[]} prepare={vi.fn()} />);
  fireEvent.change(env(), { target: { value: "LOG_LEVEL=info\nTOKEN=x" } });
  expect(screen.queryByRole("checkbox")).toBeNull();
  expect(screen.getByText(/1 sealed variable omitted from the editor/u)).toBeTruthy();
});

it("a refused save reopens with the keys it ticked Sealed", async () => {
  const refused = Promise.reject(new Error("Couldn't save this sealed value, so nothing was saved. Try again."));
  refused.catch(() => undefined);
  function Refusing() {
    const [open, setOpen] = useState(true);
    return <ServiceVariablesRawEditor open={open} onOpenChange={setOpen} variables={[]} sealing="ready" onApply={() => ({ isPersisted: { promise: refused } })} />;
  }
  render(<Refusing />);
  fireEvent.change(env(), { target: { value: "TOKEN=tok_live_123456" } });
  fireEvent.click(screen.getByRole("checkbox", { name: "Seal TOKEN" }));
  fireEvent.click(screen.getByRole("button", { name: "Update variables" }));
  await waitFor(() => expect(screen.getByText("Couldn't save this sealed value, so nothing was saved. Try again.")).toBeTruthy());
  expect(screen.getByRole("checkbox", { name: "Seal TOKEN" }).getAttribute("aria-checked")).toBe("true");
});
