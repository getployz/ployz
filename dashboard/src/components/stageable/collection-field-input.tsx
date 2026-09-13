import { SchemaFieldInput } from "#/components/stageable/schema-field-input";
import {
  collectionFieldResources,
  type CollectionFieldResourceCollection,
  type CollectionFieldResourceEntity,
  type CollectionFieldResourceKey,
  type CollectionFieldResourceName,
} from "#/components/stageable/collection-field-resources";
import {
  getValueAtPath,
  type TextInputPath,
} from "#/utils/schema-path";
import type { StringSchema } from "#/modules/environment-design/schema";
import {
  asBigInt,
  asBoolean,
  asFiniteNumber,
  asString,
} from "#/lib/json";

function normalizeInputValue<T>(value: T) {
  const text = asString(value);
  if (text !== null) {
    return text;
  }

  if (value == null) {
    return "";
  }

  const number = asFiniteNumber(value);
  if (number !== null) {
    return String(number);
  }

  const bool = asBoolean(value);
  if (bool !== null) {
    return String(bool);
  }

  const bigint = asBigInt(value);
  if (bigint !== null) {
    return String(bigint);
  }

  throw new Error("CollectionFieldInput only supports scalar leaf values.");
}

type CollectionFieldInputProps<
  TResource extends CollectionFieldResourceName,
  TPath extends TextInputPath<CollectionFieldResourceEntity<TResource>>,
> = {
  resource: TResource;
  collection: CollectionFieldResourceCollection<TResource>;
  entity: CollectionFieldResourceEntity<TResource>;
  entityId: CollectionFieldResourceKey<TResource>;
  path: TPath;
  label?: string;
  baselineValue?: string;
  isChanged?: boolean;
  schema: StringSchema;
  disabled?: boolean;
  placeholder?: string;
};

export function CollectionFieldInput<
  TResource extends CollectionFieldResourceName,
  TPath extends TextInputPath<CollectionFieldResourceEntity<TResource>>,
>({
  resource,
  collection,
  entity,
  entityId,
  path,
  label,
  baselineValue,
  isChanged = false,
  schema,
  disabled,
  placeholder,
}: CollectionFieldInputProps<TResource, TPath>) {
  const resourceConfig = collectionFieldResources[resource];
  const externalValue = normalizeInputValue(getValueAtPath(entity, path));

  return (
    <SchemaFieldInput
      schema={schema}
      value={externalValue}
      label={label}
      baselineValue={baselineValue}
      isChanged={isChanged}
      disabled={disabled}
      placeholder={placeholder}
      onCommit={(value) => {
        // SAFETY: collectionFieldResources.commit is generic over each resource's entity/path; TResource/TPath pin this call.
        const commit = resourceConfig.commit as (args: {
          collection: typeof collection;
          entityId: typeof entityId;
          path: typeof path;
          value: string;
        }) => ReturnType<typeof resourceConfig.commit>;
        return commit({ collection, entityId, path, value });
      }}
    />
  );
}
