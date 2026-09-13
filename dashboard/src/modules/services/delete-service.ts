import type { ServiceWriter } from "./services.collection";

export async function deleteService(collection: ServiceWriter, serviceId: string) {
  const transaction = collection.update(serviceId, (draft) => {
    draft.deletedAt = new Date();
  });
  await transaction.isPersisted.promise;
}
