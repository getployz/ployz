import type { ServiceWriter } from "./services.collection";

/** Stages the deletion optimistically; the writer rolls back and toasts if saving fails. */
export function deleteService(collection: ServiceWriter, serviceId: string) {
  return collection.update(serviceId, (draft) => {
    draft.deletedAt = new Date();
  });
}
