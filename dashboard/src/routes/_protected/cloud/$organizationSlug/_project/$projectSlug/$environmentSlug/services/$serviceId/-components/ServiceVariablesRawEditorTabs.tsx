import { Tabs, TabsContent, TabsList, TabsTrigger } from "#/components/ui/tabs";
import { VariableValueTextarea } from "#/components/variables/VariableValueTextarea";
import type { ReferenceTarget } from "#/modules/environment-design/variable-autocomplete";
import type { RawEditorMode } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceVariablesRawEditorTypes";

export function ServiceVariablesRawEditorTabs({
  mode,
  envText,
  jsonText,
  valueTargets,
  onEnvTextChange,
  onJsonTextChange,
  onModeChange,
}: {
  mode: RawEditorMode;
  envText: string;
  jsonText: string;
  valueTargets: ReferenceTarget[];
  onEnvTextChange: (value: string) => void;
  onJsonTextChange: (value: string) => void;
  onModeChange: (mode: RawEditorMode) => void;
}) {
  return (
    <Tabs
      className="min-w-0"
      value={mode}
      onValueChange={(value) => {
        // SAFETY: the tab triggers are only "env" | "json".
        onModeChange(value as RawEditorMode);
      }}
    >
      <TabsList variant="line">
        <TabsTrigger value="env">ENV</TabsTrigger>
        <TabsTrigger value="json">JSON</TabsTrigger>
      </TabsList>

      <TabsContent value="env" className="mt-3 min-w-0">
        <VariableValueTextarea
          aria-label="Service variables in ENV format"
          value={envText}
          onValueChange={onEnvTextChange}
          targets={valueTargets}
          className="h-[40dvh] min-h-40 max-h-72 max-w-full field-sizing-fixed overflow-auto font-mono text-xs"
          placeholder='KEY="value"'
        />
      </TabsContent>

      <TabsContent value="json" className="mt-3 min-w-0">
        <VariableValueTextarea
          aria-label="Service variables in JSON format"
          value={jsonText}
          onValueChange={onJsonTextChange}
          targets={valueTargets}
          className="h-[40dvh] min-h-40 max-h-72 max-w-full field-sizing-fixed overflow-auto font-mono text-xs"
          placeholder="{}"
        />
      </TabsContent>
    </Tabs>
  );
}
