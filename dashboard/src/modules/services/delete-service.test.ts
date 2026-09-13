import { expect, it, vi } from "vitest";
import { deleteService } from "./delete-service";
import type { ServiceWriter } from "./services.collection";

it("waits for the deletion to persist", async () => {
  let persist!: () => void;
  const promise = new Promise<void>((resolve) => { persist = resolve; });
  const update = vi.fn().mockReturnValue({ isPersisted: { promise } });
  const collection: ServiceWriter = { update };
  const done = vi.fn();
  const deleting = deleteService(collection, "service-1").then(done);
  expect(update).toHaveBeenCalledWith("service-1", expect.any(Function));
  await Promise.resolve();
  expect(done).not.toHaveBeenCalled();
  persist();
  await deleting;
  expect(done).toHaveBeenCalledOnce();
});

it("propagates persistence failure to the caller", async () => {
  const collection: ServiceWriter = {
    update: () => ({ isPersisted: { promise: Promise.reject(new Error("Save failed")) } }),
  };
  await expect(deleteService(collection, "service-1")).rejects.toThrow("Save failed");
});
