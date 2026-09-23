// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import type { ComponentProps } from "react";
import type { RuntimeContainerRecord, RuntimeServiceRecord } from "#/modules/runtime/runtime.collection";
import { ProjectCard } from "./project-card";

afterEach(cleanup);

const environment: NonNullable<ComponentProps<typeof ProjectCard>["environment"]> = {
  name: "Production", namespace: "store-production",
  services: ["api", "db", "worker"].map(slug => ({
    id: slug, slug, config: { source: { type: "empty", version: 1, rootDir: "/" } },
  })),
};

function container(id: string, state: string, health = "healthy"): RuntimeContainerRecord {
  return { id, displayName: id, machineId: "machine", projectName: "store-production", kind: "service_container", runtime: { state, health } };
}

function runtimeService(identity: string, containers: RuntimeContainerRecord[], hookContainers: RuntimeContainerRecord[] = []): RuntimeServiceRecord {
  return { id: identity, identity, serviceId: identity, containers, hookContainers, observedAt: "2026-09-23T00:00:00Z" };
}

it("counts working services once, excluding stopped containers, hooks, and other environments", () => {
  const runtimeServices = [
    runtimeService("store-production/api", [container("api-1", "running"), container("api-2", "running")]),
    runtimeService("store-production/db", [container("db", "running", "not_configured")]),
    runtimeService("store-production/worker", [container("worker", "exited")], [container("hook", "running")]),
    runtimeService("staging/worker", [container("other", "running")]),
    runtimeService("store-production/removed", [container("removed", "running")]),
  ];
  const view = render(<ProjectCard name="Store" environment={environment} runtimeServices={runtimeServices} runtimeStatus="observed" />);
  expect(screen.getByText("2/3 services online")).toBeTruthy();
  expect(screen.getByText("production")).toBeTruthy();
  expect(screen.getByLabelText("Services").children).toHaveLength(3);

  view.rerender(<ProjectCard name="Store" environment={environment} runtimeServices={runtimeServices} runtimeStatus="unavailable" />);
  expect(screen.getByText("3 services")).toBeTruthy();
  expect(screen.queryByText(/online/)).toBeNull();
});

it("does not count unhealthy or starting containers as online", () => {
  render(<ProjectCard name="Store" environment={environment} runtimeStatus="observed" runtimeServices={[
    runtimeService("store-production/api", [container("api", "running", "unhealthy")]),
    runtimeService("store-production/db", [container("db", "running", "starting")]),
  ]} />);
  expect(screen.getByText("0/3 services online")).toBeTruthy();
});

it("keeps empty and single-service footers compact", () => {
  const view = render(<ProjectCard name="Store" environment={{ ...environment, services: [] }} runtimeServices={[]} runtimeStatus="observed" />);
  expect(screen.getByText("No services")).toBeTruthy();
  view.rerender(<ProjectCard name="Store" environment={{ ...environment, services: environment.services.slice(0, 1) }} runtimeServices={[]} runtimeStatus="observed" />);
  expect(screen.getByText("0/1 service online")).toBeTruthy();
});
