export type PersistableTransaction = {
  isPersisted: {
    promise: Promise<unknown>;
  };
};
