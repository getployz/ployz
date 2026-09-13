import {
  type ServiceWithContextRecord,
} from "#/modules/environment-design/services";
import {
  setDraftValueAtPath,
  type DeepPath,
  type DeepPathValue,
} from "#/utils/schema-path";

export type PersistableTransaction = {
  isPersisted: {
    promise: Promise<unknown>;
  };
};

export type CollectionLike<TEntity, TKey> = {
  update(
    entityId: TKey,
    updater: (draft: TEntity) => void,
  ): PersistableTransaction;
};

export type CollectionResourceConfig<TEntity, TKey> = {
  getKey: (entity: TEntity) => TKey;
  commit: <TPath extends DeepPath<TEntity>>(args: {
    collection: CollectionLike<TEntity, TKey>;
    entityId: TKey;
    path: TPath;
    value: DeepPathValue<TEntity, TPath>;
  }) => PersistableTransaction;
};

export const collectionFieldResources = {
  service: {
    getKey: (entity: ServiceWithContextRecord) => entity.id,
    commit: ({
      collection,
      entityId,
      path,
      value,
    }: {
      collection: CollectionLike<ServiceWithContextRecord, string>;
      entityId: string;
      path: DeepPath<ServiceWithContextRecord>;
      value: DeepPathValue<
        ServiceWithContextRecord,
        DeepPath<ServiceWithContextRecord>
      >;
    }) => {
      return collection.update(entityId, (draft) => {
        setDraftValueAtPath(draft, path, value);
      });
    },
  } satisfies CollectionResourceConfig<
    ServiceWithContextRecord,
    string
  >,
};

export type CollectionFieldResourceName = keyof typeof collectionFieldResources;

export type CollectionFieldResourceEntity<
  TResource extends CollectionFieldResourceName,
> = Parameters<(typeof collectionFieldResources)[TResource]["getKey"]>[0];

export type CollectionFieldResourceKey<
  TResource extends CollectionFieldResourceName,
> = ReturnType<(typeof collectionFieldResources)[TResource]["getKey"]>;

export type CollectionFieldResourceCollection<
  TResource extends CollectionFieldResourceName,
> = CollectionLike<
  CollectionFieldResourceEntity<TResource>,
  CollectionFieldResourceKey<TResource>
>;
