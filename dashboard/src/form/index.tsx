"use client";

import type { ComponentProps, ReactNode } from "react";
import {
  createErrorVisibility,
  createFormHook,
  createValidator,
  getFormHookHelpers,
  useSelector,
  type AnyFieldApi,
  type FieldWithValue,
} from "@tanstack/react-form";

import { Button } from "#/components/ui/button";
import { asRecord, asString } from "#/lib/json";
import {
  Combobox as ComboboxRoot,
  ComboboxChip,
  ComboboxChips,
  ComboboxChipsInput,
  ComboboxContent,
  ComboboxEmpty,
  ComboboxInput,
  ComboboxItem,
  ComboboxList,
  ComboboxValue,
  useComboboxAnchor,
} from "#/components/ui/combobox";
import {
  Field,
  FieldDescription,
  FieldError,
  FieldLabel,
} from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import {
  Select as SelectRoot,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "#/components/ui/select";
import { Spinner } from "#/components/ui/spinner";

type FieldChromeProps = {
  field: AnyFieldApi;
  label?: ReactNode;
  description?: ReactNode;
  disabled?: boolean;
  htmlFor?: string;
  children: ReactNode;
};

function FieldChrome({
  field,
  label,
  description,
  disabled,
  htmlFor,
  children,
}: FieldChromeProps) {
  const invalid = field.meta.isInvalid;

  return (
    <Field
      data-disabled={disabled || undefined}
      data-invalid={invalid || undefined}
    >
      {label ? <FieldLabel htmlFor={htmlFor}>{label}</FieldLabel> : null}
      {description ? <FieldDescription>{description}</FieldDescription> : null}
      {children}
      <FieldError errors={field.errors} />
    </Field>
  );
}

type TextProps = {
  field: FieldWithValue<string>;
  label?: ReactNode;
  description?: ReactNode;
} & Omit<
  ComponentProps<typeof Input>,
  "aria-invalid" | "defaultValue" | "name" | "onBlur" | "onChange" | "value"
>;

function Text({
  field,
  label,
  description,
  id,
  disabled,
  ...props
}: TextProps) {
  const inputId = id ?? String(field.name);
  const isDisabled = useDisabled(field, disabled);

  return (
    <FieldChrome
      field={field}
      label={label}
      description={description}
      disabled={isDisabled}
      htmlFor={inputId}
    >
      <Input
        {...props}
        id={inputId}
        name={String(field.name)}
        value={field.value}
        aria-invalid={field.meta.isInvalid || undefined}
        disabled={isDisabled}
        onBlur={field.handleBlur}
        onChange={(event) => field.handleChange(event.target.value)}
      />
    </FieldChrome>
  );
}

type Choice = string | { value: string; label: string };

function choiceValue(choice: Choice) {
  const text = asString(choice);
  if (text !== null) return text;
  const record = asRecord(choice);
  return asString(record?.["value"]) ?? "";
}

function choiceLabel(choice: Choice) {
  const text = asString(choice);
  if (text !== null) return text;
  const record = asRecord(choice);
  return asString(record?.["label"]) ?? "";
}

function useDisabled(field: AnyFieldApi, disabled?: boolean) {
  const isSubmitting = useSelector(
    field.form.atom,
    (state) => state.isSubmitting,
  );
  return disabled || isSubmitting;
}

type ChoiceFieldProps = {
  field: FieldWithValue<string>;
  label?: ReactNode;
  description?: ReactNode;
  options: ReadonlyArray<Choice>;
  placeholder?: string;
  searchable?: boolean;
  disabled?: boolean;
  onValueChange?: (value: string) => void;
};

function Select({
  field,
  label,
  description,
  options,
  placeholder,
  searchable,
  disabled,
  onValueChange,
}: ChoiceFieldProps) {
  const id = String(field.name);
  const isDisabled = useDisabled(field, disabled);
  const labels = new Map(
    options.map((option) => [choiceValue(option), choiceLabel(option)]),
  );
  const values = [...labels.keys()];

  const handleValueChange = (value: string | null) => {
    const nextValue = value ?? "";
    field.handleChange(nextValue);
    onValueChange?.(nextValue);
  };

  return (
    <FieldChrome
      field={field}
      label={label}
      description={description}
      disabled={isDisabled}
      htmlFor={id}
    >
      {searchable ? (
        <ComboboxRoot
          items={values}
          value={field.value || null}
          itemToStringLabel={(value) => labels.get(value) ?? value}
          disabled={isDisabled}
          onValueChange={handleValueChange}
        >
          <ComboboxInput
            id={id}
            name={String(field.name)}
            placeholder={placeholder}
            aria-invalid={field.meta.isInvalid || undefined}
            disabled={isDisabled}
            showClear
            onBlur={field.handleBlur}
          />
          <ComboboxContent>
            <ComboboxEmpty>No matches.</ComboboxEmpty>
            <ComboboxList>
              {(value: string) => (
                <ComboboxItem key={value} value={value}>
                  {labels.get(value) ?? value}
                </ComboboxItem>
              )}
            </ComboboxList>
          </ComboboxContent>
        </ComboboxRoot>
      ) : (
        <SelectRoot
          name={String(field.name)}
          value={field.value}
          disabled={isDisabled}
          onValueChange={handleValueChange}
        >
          <SelectTrigger
            id={id}
            className="w-full"
            aria-invalid={field.meta.isInvalid || undefined}
            onBlur={field.handleBlur}
          >
            <SelectValue placeholder={placeholder} />
          </SelectTrigger>
          <SelectContent>
            <SelectGroup>
              {values.map((value) => (
                <SelectItem key={value} value={value}>
                  {labels.get(value) ?? value}
                </SelectItem>
              ))}
            </SelectGroup>
          </SelectContent>
        </SelectRoot>
      )}
    </FieldChrome>
  );
}

type MultiSelectProps = Omit<
  ChoiceFieldProps,
  "field" | "onValueChange" | "searchable"
> & {
  field: FieldWithValue<Array<string>>;
};

function MultiSelect({
  field,
  label,
  description,
  options,
  placeholder,
  disabled,
}: MultiSelectProps) {
  const isDisabled = useDisabled(field, disabled);
  const labels = new Map(
    options.map((option) => [choiceValue(option), choiceLabel(option)]),
  );
  const values = [...labels.keys()];
  const anchor = useComboboxAnchor();

  return (
    <FieldChrome
      field={field}
      label={label}
      description={description}
      disabled={isDisabled}
    >
      <ComboboxRoot
        multiple
        items={values}
        value={field.value}
        itemToStringLabel={(value) => labels.get(value) ?? value}
        disabled={isDisabled}
        onValueChange={(value) => field.handleChange(value)}
      >
        <ComboboxChips
          ref={anchor}
          aria-invalid={field.meta.isInvalid || undefined}
        >
          <ComboboxValue>
            {(selected: Array<string>) => (
              <>
                {selected.map((value) => (
                  <ComboboxChip key={value}>
                    {labels.get(value) ?? value}
                  </ComboboxChip>
                ))}
                <ComboboxChipsInput
                  name={String(field.name)}
                  placeholder={selected.length === 0 ? placeholder : undefined}
                  disabled={isDisabled}
                  onBlur={field.handleBlur}
                />
              </>
            )}
          </ComboboxValue>
        </ComboboxChips>
        <ComboboxContent anchor={anchor}>
          <ComboboxEmpty>No matches.</ComboboxEmpty>
          <ComboboxList>
            {(value: string) => (
              <ComboboxItem key={value} value={value}>
                {labels.get(value) ?? value}
              </ComboboxItem>
            )}
          </ComboboxList>
        </ComboboxContent>
      </ComboboxRoot>
    </FieldChrome>
  );
}

const { fieldComponent } = getFormHookHelpers();

const fieldComponents = {
  Text: fieldComponent.strict(Text, "field"),
  Select: fieldComponent.loose(Select, "field"),
  MultiSelect: fieldComponent.strict(MultiSelect, "field"),
};

function FormElement({ onSubmit, id, ...props }: ComponentProps<"form">) {
  const form = useFormContext();

  return (
    <form
      {...props}
      id={id ?? form.formId}
      onSubmit={
        onSubmit ??
        ((event) => {
          event.preventDefault();
          event.stopPropagation();
          void form.handleSubmit();
        })
      }
    />
  );
}

type SubmitButtonProps = ComponentProps<typeof Button> & {
  pendingChildren?: ReactNode;
};

function SubmitButton({
  children,
  pendingChildren,
  type = "submit",
  disabled,
  ...props
}: SubmitButtonProps) {
  const form = useFormContext();

  return (
    <form.Subscribe
      selector={(state) => [state.canSubmit, state.isSubmitting] as const}
    >
      {([canSubmit, isSubmitting]) => (
        <Button
          {...props}
          type={type}
          form={form.formId}
          disabled={disabled || !canSubmit || isSubmitting}
        >
          {isSubmitting ? <Spinner data-icon="inline-start" /> : null}
          {isSubmitting && pendingChildren ? pendingChildren : children}
        </Button>
      )}
    </form.Subscribe>
  );
}

const { appFormOptions, useAppForm, useFormContext } = createFormHook({
  fieldComponents,
  formComponents: {
    Form: FormElement,
    SubmitButton,
  },
});

const showErrorsAfterBlurOrSubmit = createErrorVisibility(
  ({ fieldState, state }) =>
    fieldState.meta.isBlurred || state.submissionAttempts > 0,
);

const validateAfterBlurThenWhileInvalid = createValidator({
  triggers: [
    "blur",
    {
      trigger: "change",
      when: ({ fieldApi }) => fieldApi !== undefined && fieldApi.meta.isInvalid,
    },
  ],
});

export {
  appFormOptions,
  showErrorsAfterBlurOrSubmit,
  useAppForm,
  validateAfterBlurThenWhileInvalid,
};
export type { ReactFormType } from "@tanstack/react-form";
