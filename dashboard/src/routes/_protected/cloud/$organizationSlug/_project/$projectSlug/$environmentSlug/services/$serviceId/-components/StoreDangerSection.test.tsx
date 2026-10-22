// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { ReviewLifecycleKind } from "@ployz/sdk";
import { afterEach, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { StoreDangerSection, type StoreService } from "./StoreServiceDrawer";
import * as deletes from "./useDeleteService";

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

function danger(change: ReviewLifecycleKind | null) {
  const remove = vi.fn();
  vi.spyOn(deletes, "useRemoveStoreService").mockReturnValue(remove);
  render(<StoreDangerSection state={asTestDouble<StoreService>()({
    environment: { project: "shop", environment: "production" }, service: { name: "web", change },
  })} />);
  return remove;
}

it("asks before deleting a new service, since nothing is left to discard", () => {
  const remove = danger("create");
  expect(screen.queryByText("The service is removed on your next deploy.")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Delete service" }));
  expect(remove).not.toHaveBeenCalled();
  screen.getByText("web is new and will be deleted, with its unsaved settings.");
  fireEvent.click(screen.getByRole("button", { name: "Delete" }));
  expect(remove).toHaveBeenCalledOnce();
});

it("stages a saved service's deletion for the next deploy without asking", () => {
  const remove = danger(null);
  screen.getByText("The service is removed on your next deploy.");
  fireEvent.click(screen.getByRole("button", { name: "Delete service" }));
  expect(remove).toHaveBeenCalledOnce();
});
