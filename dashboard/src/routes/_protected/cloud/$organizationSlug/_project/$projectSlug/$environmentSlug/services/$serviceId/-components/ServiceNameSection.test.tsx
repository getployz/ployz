// @vitest-environment jsdom

import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { ServiceNameSection } from "./ServiceNameSection";
import type { ServiceDrawerState } from "./useServiceDrawerState";

function createState(overrides?: Partial<ServiceDrawerState>) {
  const service = {
    id: "22222222-2222-4222-8222-222222222222",
    environmentId: "11111111-1111-4111-8111-111111111111",
    lineageId: "44444444-4444-4444-8444-444444444444",
    name: "api",
    slug: "api",
    source: {
      version: 1,
      type: "empty",
      rootDir: "/",
    },
    registryCredentialUsername: null,
    hasStoredRegistryCredential: false,
    preDeployCommand: null,
    startCommand: null,
    healthcheck: {
      type: "none",
    },
    restartPolicy: "unless-stopped",
    firstDeployedAt: null,
    createdAt: new Date("2026-01-01T00:00:00.000Z"),
    updatedAt: new Date("2026-01-01T00:00:00.000Z"),
    projectSlug: "project",
    environmentSlug: "prod",
    env: {},
  } as ServiceDrawerState["service"];

  const updateMock = vi.fn(() => ({
    isPersisted: {
      promise: Promise.resolve(),
    },
  }));

  const state: ServiceDrawerState = {
    organizationSlug: "acme",
    service,
    environmentNodes: [
      {
        type: "service",
        id: service.id,
        name: service.name,
      },
      {
        type: "variable_group",
        id: "33333333-3333-4333-8333-333333333333",
        name: "WEB",
      },
    ],
    diff: asTestDouble<ServiceDrawerState["diff"]>()({
      field: () => ({
        changed: false,
        baselineValue: undefined,
      }),
    }),
    collection: {
      update: updateMock,
    },
    managedPrefixesInUse: [],
    defaultTargetPort: 8080,
    ...overrides,
  };

  return { state, updateMock };
}

describe("ServiceNameSection", () => {
  it("prevents renaming a service to another environment node name", async () => {
    const { state, updateMock } = createState();

    render(<ServiceNameSection state={state} />);

    fireEvent.change(screen.getByLabelText("Name"), {
      target: {
        value: " web ",
      },
    });
    fireEvent.click(screen.getByLabelText("Confirm"));

    await screen.findByText(
      'A node named "web" already exists in this environment.',
    );
    await waitFor(() => {
      expect(updateMock.mock.calls).toHaveLength(0);
    });
  });
});
