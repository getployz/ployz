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
import type { OrganizationVariablesCollection } from "#/modules/environment-design/variable-collections";
import type { PlainVariableRecord } from "#/modules/environment-design/variable-mutation-actions";
import type { VariableRecord } from "#/modules/environment-design/variables";
import { VariableRow } from "./variable-row";
import { toast } from "sonner";

const collection = asTestDouble<OrganizationVariablesCollection>()({
  delete: vi.fn(),
  update: vi.fn(),
});

function plainVariable(
  overrides: Partial<PlainVariableRecord> = {},
): PlainVariableRecord {
  return {
    id: "33333333-3333-4333-8333-333333333333",
    serviceId: "22222222-2222-4222-8222-222222222222",
    variableGroupId: null,
    configKeyId: "44444444-4444-4444-8444-444444444444",
    key: "API_KEY",
    description: null,
    exported: false,
    value: {
      type: "plain",
      value: "super-secret",
    },
    createdAt: new Date(0),
    updatedAt: new Date(0),
    ...overrides,
  };
}

function sealedVariable(): VariableRecord {
  return {
    ...plainVariable(),
    value: {
      type: "sealed",
      hasValue: true,
      fingerprint: "fingerprint",
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

  it("shows pending feedback while the seal action is running", async () => {
    let resolveSeal!: () => void;
    const onSealVariable = vi.fn(
      () =>
        new Promise<void>((resolve) => {
          resolveSeal = resolve;
        }),
    );

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

    await waitFor(() => {
      expect(onSealVariable).toHaveBeenCalled();
    });
    expect(await screen.findByText("Sealing…")).toBeTruthy();

    resolveSeal();
    await waitFor(() => {
      expect(screen.queryByText("Sealing…")).toBeNull();
    });
    expect(toast.success).not.toHaveBeenCalled();
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
});
