// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { ManagedDomainDialog } from "./ManagedDomain";

afterEach(() => {
  cleanup();
  document.body.replaceChildren();
});

it("a refused subdomain shows core's reason, not the schema's", async () => {
  render(
    <ManagedDomainDialog managed={{ prefix: "web", targetPort: null }} clusterDomain="example.ployz.app" takenPrefixes={[]}
      defaultTargetPort={null} onClose={vi.fn()} onSubmit={vi.fn()} />,
  );
  const subdomain = screen.getByLabelText("Subdomain");
  fireEvent.change(subdomain, { target: { value: "-bad-" } });
  fireEvent.blur(subdomain);
  await waitFor(() => expect(screen.getByRole("button", { name: "Save domain" }).hasAttribute("disabled")).toBe(true));
  const errors = [...document.querySelectorAll('[data-slot="field-error"]')].map((node) => node.textContent);
  expect(errors.length).toBe(1);
  expect(errors[0]).toMatch(/DNS label/u);
});

it("says a blank port uses the Service's PORT and what it is", async () => {
  render(
    <ManagedDomainDialog managed={{ prefix: "web", targetPort: null }} clusterDomain="example.ployz.app" takenPrefixes={[]}
      defaultTargetPort={8080} onClose={vi.fn()} onSubmit={vi.fn()} />,
  );
  expect(screen.getByText("Leave blank to use this service's PORT variable, 8080.")).toBeTruthy();
  expect(screen.queryByText(/listens on/u)).toBeNull();
  const port = screen.getByLabelText(/port/iu);
  fireEvent.change(port, { target: { value: "70000" } });
  fireEvent.blur(port);
  await waitFor(() => expect(screen.getByText("Enter a port between 1 and 65535, or leave it blank.")).toBeTruthy());
});
