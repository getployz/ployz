// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ServiceSettingInput } from "./ServiceSettingInput";

describe("ServiceSettingInput comparison copy", () => {
  afterEach(cleanup);

  it("labels a Saved comparison as Saved rather than Deployed", () => {
    render(
      <ServiceSettingInput
        ariaLabel="Replicas"
        value="3"
        isChanged
        baselineLabel="Saved"
        baselineValue="2"
        onCommit={vi.fn()}
      />,
    );

    expect(screen.getByLabelText("Replicas").getAttribute("title")).toBe(
      "Saved: 2",
    );
  });
});
