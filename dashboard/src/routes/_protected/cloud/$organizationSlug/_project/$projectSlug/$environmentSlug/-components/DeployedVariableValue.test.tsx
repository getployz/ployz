// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { DeployedVariableValue } from "./DeployedVariableValue";


afterEach(cleanup);

it("hides a plain value until revealed, and hides it again", () => {
  render(<DeployedVariableValue name="APP_URL" value="https://app.example.test" from={["api"]} />);
  expect(screen.queryByText("https://app.example.test")).toBeNull();
  expect(screen.getByText("resolves from api")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Show APP_URL" }));
  expect(screen.getByText("https://app.example.test")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Hide APP_URL" }));
  expect(screen.queryByText("https://app.example.test")).toBeNull();
});

it("shows a sealed value as Sealed with nothing to reveal", () => {
  render(<DeployedVariableValue name="DATABASE_URL" value={null} from={["api"]} />);
  expect(screen.getByText("Sealed")).toBeTruthy();
  expect(screen.queryByRole("button")).toBeNull();
});

it("shows a key the server did not return as not resolved, not Sealed", () => {
  render(<DeployedVariableValue name="GONE" value={undefined} from={[]} />);
  expect(screen.getByText("Not resolved")).toBeTruthy();
  expect(screen.queryByText("Sealed")).toBeNull();
  expect(screen.queryByRole("button")).toBeNull();
});
