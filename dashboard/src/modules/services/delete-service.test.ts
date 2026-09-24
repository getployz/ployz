import { expect, it, vi } from "vitest";
import { deleteService } from "./delete-service";
import type { ServiceWriter } from "./services.collection";

it("stages the deletion at once without waiting for it to persist", () => {
  const transaction = { isPersisted: { promise: new Promise<never>(() => {}) } };
  const update = vi.fn().mockReturnValue(transaction);
  const collection: ServiceWriter = { update };
  expect(deleteService(collection, "service-1")).toBe(transaction);
  expect(update).toHaveBeenCalledWith("service-1", expect.any(Function));
  const draft = { deletedAt: null as Date | null };
  update.mock.calls[0]?.[1](draft);
  expect(draft.deletedAt).toBeInstanceOf(Date);
});
