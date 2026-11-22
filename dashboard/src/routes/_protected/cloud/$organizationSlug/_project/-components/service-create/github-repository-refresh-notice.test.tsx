// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { GithubRepositoryRefreshNotice } from "./github-repository-refresh-notice";

afterEach(cleanup);

it("says when a failed first load is read again", () => {
  render(<GithubRepositoryRefreshNotice initial />);
  expect(screen.getByRole("alert").textContent).toBe("Could not load repositories. Trying again every 15 seconds.");
});

it("says the shown repositories may be out of date, and when they are read again", () => {
  render(<GithubRepositoryRefreshNotice />);
  expect(screen.getByRole("alert").textContent).toBe("Could not refresh repositories. Shown results may be out of date. Trying again every 15 seconds.");
});

it("says to reload the page when the repository list crashed, since the poll's reads never reach it", () => {
  render(<GithubRepositoryRefreshNotice crashed />);
  expect(screen.getByRole("alert").textContent).toBe("Could not load repositories. Reload the page to try again.");
});
