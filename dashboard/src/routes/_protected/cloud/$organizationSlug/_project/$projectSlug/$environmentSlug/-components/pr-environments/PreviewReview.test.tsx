// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { ReviewLines } from "./PreviewReview";

afterEach(cleanup);

it("keeps each value's spacing, so two settings that sign differently read apart", () => {
  render(<ReviewLines lines={["Review web at fedcba987654", '  variables X="a b"', '  variables Y="a  b"']} />);
  const items = screen.getAllByRole("listitem");
  expect(items.map((item) => item.textContent)).toEqual(["Review web at fedcba987654", 'variables X="a b"', 'variables Y="a  b"']);
  for (const item of items) expect(item.className).toContain("whitespace-pre-wrap");
});
