// @vitest-environment jsdom

import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import type { PlainVariableRecord, VariableRecord, VariableWriter } from "#/modules/variables/variables";
import { VariableRow } from "./variable-row";
import { toast } from "sonner";

const collection = asTestDouble<VariableWriter>()({
  delete: vi.fn(),
  update: vi.fn(),
});

function plainVariable(
  overrides: Partial<PlainVariableRecord> = {},
): PlainVariableRecord {
  return {
    id: "33333333-3333-4333-8333-333333333333",
    serviceId: "22222222-2222-4222-8222-222222222222",

    key: "API_KEY",
    description: null,
    exported: false,
    value: {
      type: "plain",
      value: "super-secret",
    },
    ...overrides,
  };
}

function sealedVariable(): VariableRecord {
  return {
    ...plainVariable(),
    value: {
      type: "sealed",
    },
  };
}

describe("VariableRow", () => {
  beforeEach(() => {
    vi.spyOn(toast, "success");
  });

  afterEach(() => {
    cleanup();
    document.body.replaceChildren();
    document.body.style.removeProperty("overflow");
    vi.restoreAllMocks();
  });

  it("shows a subtle reference icon whose message opens on tap", async () => {
    render(<VariableRow variable={plainVariable({ unresolvedReferences: ["Postgres"] })} collection={collection} onSealVariable={vi.fn()} />);
    expect(screen.queryByText("Unknown reference: Postgres")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Unresolved variable reference" }));
    expect(await screen.findByText("Unknown reference: Postgres")).toBeTruthy();
    expect(toast.success).not.toHaveBeenCalled();
  });

  it("shows a seal action for plain variables and confirms without plaintext", async () => {
    const onSealVariable = vi.fn().mockResolvedValue(undefined);

    render(
      <VariableRow
        variable={plainVariable()}
        collection={collection}
        onSealVariable={onSealVariable}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Variable actions" }));
    fireEvent.click(await screen.findByText("Seal"));

    expect(await screen.findByText("Seal Variable")).toBeTruthy();
    expect(screen.queryByText("super-secret")).toBeNull();
    expect(screen.queryByText(/learn more/i)).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Seal variable" }));

    await waitFor(() => {
      expect(onSealVariable).toHaveBeenCalledWith(
        expect.objectContaining({
          key: "API_KEY",
          value: {
            type: "plain",
            value: "super-secret",
          },
        }),
      );
    });
    await waitFor(() => {
      expect(screen.queryByText("Seal Variable")).toBeNull();
    });
    expect(toast.success).not.toHaveBeenCalled();
  });

  it("closes the seal dialog at once because sealing is optimistic", async () => {
    const onSealVariable = vi.fn();

    render(
      <VariableRow
        variable={plainVariable()}
        collection={collection}
        onSealVariable={onSealVariable}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Variable actions" }));
    fireEvent.click(await screen.findByText("Seal"));
    fireEvent.click(screen.getByRole("button", { name: "Seal variable" }));

    expect(onSealVariable).toHaveBeenCalledOnce();
    await waitFor(() => {
      expect(screen.queryByText("Seal Variable")).toBeNull();
    });
    expect(screen.queryByText("Sealing…")).toBeNull();
  });

  it("does not call the seal action when confirmation is cancelled", async () => {
    const onSealVariable = vi.fn().mockResolvedValue(undefined);

    render(
      <VariableRow
        variable={plainVariable()}
        collection={collection}
        onSealVariable={onSealVariable}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Variable actions" }));
    fireEvent.click(await screen.findByText("Seal"));
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(onSealVariable).not.toHaveBeenCalled();
  });

  it("does not expose plain-only controls for sealed variables", async () => {
    render(
      <VariableRow
        variable={sealedVariable()}
        collection={collection}
        onSealVariable={vi.fn()}
      />,
    );

    expect(screen.queryByText("Show value")).toBeNull();
    expect(screen.queryByText("Copy value")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Variable actions" }));
    expect(await screen.findByText("Delete")).toBeTruthy();
    expect(screen.queryByText("Seal")).toBeNull();
    expect(screen.queryByText("Edit")).toBeNull();
  });

  it("says a secret is Not set, and Set value seals the one typed", async () => {
    const onSealVariable = vi.fn();
    render(<VariableRow variable={{ ...sealedVariable(), value: { type: "sealed", notSet: true } }} collection={collection}
      onSealVariable={onSealVariable} />);
    expect(screen.getByText("Not set")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Variable actions" }));
    fireEvent.click(await screen.findByText("Set value"));
    const input = screen.getByLabelText("Secret value");
    expect(input.getAttribute("type")).toBe("password");
    fireEvent.change(input, { target: { value: "prod-key" } });
    fireEvent.keyDown(input, { key: "Enter" });

    expect(onSealVariable).toHaveBeenCalledWith(expect.objectContaining({ key: "API_KEY", value: { type: "plain", value: "prod-key" } }));
    expect(collection.update).not.toHaveBeenCalled();
  });

  it("pins a variable marked Never sync, and its menu syncs it again", async () => {
    const onNeverSync = vi.fn();
    const variable = plainVariable({ neverSynced: true });
    render(<VariableRow variable={variable} collection={collection} onSealVariable={vi.fn()} onNeverSync={onNeverSync} />);
    expect(screen.getByText("Never synced")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Variable actions" }));
    fireEvent.click(await screen.findByText("Sync again"));
    expect(onNeverSync).toHaveBeenCalledWith(variable, false);
  });

  it("offers Never sync for a variable that syncs, without a pin", async () => {
    const onNeverSync = vi.fn();
    const variable = plainVariable();
    render(<VariableRow variable={variable} collection={collection} onSealVariable={vi.fn()} onNeverSync={onNeverSync} />);
    expect(screen.queryByText("Never synced")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Variable actions" }));
    fireEvent.click(await screen.findByText("Never sync"));
    expect(onNeverSync).toHaveBeenCalledWith(variable, true);
  });

  it("renames from the menu: prefilled key, typed name saved uppercased", async () => {
    const onRename = vi.fn();
    const variable = plainVariable();
    render(<VariableRow variable={variable} collection={collection} onSealVariable={vi.fn()} onRename={onRename} />);

    fireEvent.click(screen.getByRole("button", { name: "Variable actions" }));
    fireEvent.click(await screen.findByText("Rename"));
    const input = await screen.findByLabelText("Name");
    expect((input as HTMLInputElement).value).toBe("API_KEY");
    const save = screen.getByRole("button", { name: "Rename" });
    expect(save.hasAttribute("disabled")).toBe(true);

    fireEvent.change(input, { target: { value: "1BAD" } });
    expect(screen.getByText("Use letters, digits and underscores, not starting with a digit (at most 128).")).toBeTruthy();
    expect(save.hasAttribute("disabled")).toBe(true);

    fireEvent.change(input, { target: { value: "stripe_key" } });
    fireEvent.click(save);
    expect(onRename).toHaveBeenCalledExactlyOnceWith(variable, "STRIPE_KEY", false);
    await waitFor(() => expect(screen.queryByLabelText("Name")).toBeNull());
  });

  it("renames a sealed variable without showing a value", async () => {
    const onRename = vi.fn();
    const variable = sealedVariable();
    render(<VariableRow variable={variable} collection={collection} onSealVariable={vi.fn()} onRename={onRename} />);

    fireEvent.click(screen.getByRole("button", { name: "Variable actions" }));
    fireEvent.click(await screen.findByText("Rename"));
    fireEvent.change(await screen.findByLabelText("Name"), { target: { value: "STRIPE_KEY" } });
    fireEvent.click(screen.getByRole("button", { name: "Rename" }));
    expect(onRename).toHaveBeenCalledExactlyOnceWith(variable, "STRIPE_KEY", false);
    expect(screen.queryByText("super-secret")).toBeNull();
  });

  it("lists the variables that use it before a rename, and switches them with it", async () => {
    const onRename = vi.fn();
    const variable = plainVariable();
    render(<VariableRow variable={variable} collection={collection} onSealVariable={vi.fn()} onRename={onRename}
      referrers={["web.UPSTREAM", "worker.BACKEND"]} />);

    fireEvent.click(screen.getByRole("button", { name: "Variable actions" }));
    fireEvent.click(await screen.findByText("Rename"));
    expect(await screen.findByText("Also updates 2 references")).toBeTruthy();
    const listed = screen.getByRole("list", { name: "References" });
    expect([...listed.querySelectorAll("li")].map((item) => item.textContent)).toEqual(["web.UPSTREAM", "worker.BACKEND"]);
    expect(screen.queryByRole("checkbox")).toBeNull();

    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "STRIPE_KEY" } });
    fireEvent.click(screen.getByRole("button", { name: "Rename" }));
    expect(onRename).toHaveBeenCalledExactlyOnceWith(variable, "STRIPE_KEY", true);
  });

  it("cancels a rename without saving", async () => {
    const onRename = vi.fn();
    render(<VariableRow variable={plainVariable()} collection={collection} onSealVariable={vi.fn()} onRename={onRename} />);
    fireEvent.click(screen.getByRole("button", { name: "Variable actions" }));
    fireEvent.click(await screen.findByText("Rename"));
    fireEvent.change(await screen.findByLabelText("Name"), { target: { value: "STRIPE_KEY" } });
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByLabelText("Name")).toBeNull());
    expect(onRename).not.toHaveBeenCalled();
  });

  it("offers no Rename where the panel can't rename", async () => {
    render(<VariableRow variable={plainVariable()} collection={collection} onSealVariable={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Variable actions" }));
    await screen.findByText("Delete");
    expect(screen.queryByText("Rename")).toBeNull();
  });
});
