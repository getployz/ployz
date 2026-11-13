// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { ApprovalPrompt, reviewApproval } from "./approval-prompt";

afterEach(cleanup);

it("shows every line it will sign with its spacing kept, and answers only the person's press", async () => {
  render(<ApprovalPrompt />);
  let answer: Promise<boolean> | undefined;
  act(() => {
    answer = reviewApproval({ title: "Redeploy production?", lines: ["Approved web: nginx", '  variables X="a b"', '  variables Y="a  b"'], note: "Check it.", action: "Approve" });
  });
  const box = screen.getByLabelText("What you approve");
  expect([...box.children].map((line) => line.textContent)).toEqual(["Approved web: nginx", '  variables X="a b"', '  variables Y="a  b"']);
  expect(box.className).toContain("whitespace-pre-wrap");
  fireEvent.click(screen.getByRole("button", { name: "Approve" }));
  expect(await answer).toBe(true);
  act(() => {
    answer = reviewApproval({ title: "Redeploy production?", lines: ["Approved web: nginx"], note: "Check it.", action: "Approve" });
  });
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  expect(await answer).toBe(false);
});
