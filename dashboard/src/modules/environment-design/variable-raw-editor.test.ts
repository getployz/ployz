import { Result } from "effect";
import { describe, expect, it } from "vitest";
import type {
  ParsedEntry,
  RawEditorParseError,
} from "#/modules/environment-design/variable-raw-editor";
import type { VariableRecord } from "#/modules/environment-design/variables";
import {
  diffVariables,
  findSealedVariableNameCollisions,
  findDuplicateEnvKeys,
  findDuplicateJsonKeys,
  getSealedVariableCollisionMessage,
  parseEnv,
  parseJson,
  serializeVariablesToEnv,
  serializeVariablesToJson,
} from "#/modules/environment-design/variable-raw-editor";

function expectOk<T>(
  result: Result.Result<T, RawEditorParseError>,
): T {
  if (Result.isFailure(result)) {
    throw new Error(`expected Ok, got ${result.failure.message}`);
  }
  return result.success;
}

function expectErr<T>(
  result: Result.Result<T, RawEditorParseError>,
): RawEditorParseError {
  if (!Result.isFailure(result)) {
    throw new Error("expected Err, got Ok");
  }
  return result.failure;
}

function parseEnvOk(text: string): ParsedEntry[] {
  return expectOk(parseEnv(text));
}

function plain(key: string, value: string, id = `id-${key}`): VariableRecord {
  return {
    id,
    serviceId: "service-1",
    variableGroupId: null,

    key,
    description: null,
    exported: false,
    value: { type: "plain", value },
    createdAt: new Date(0),
    updatedAt: new Date(0),
  };
}

function sealed(key: string, id = `id-${key}`): VariableRecord {
  return {
    id,
    serviceId: "service-1",
    variableGroupId: null,

    key,
    description: null,
    exported: false,
    value: { type: "sealed", hasValue: true, fingerprint: `fp-${key}` },
    createdAt: new Date(0),
    updatedAt: new Date(0),
  };
}

describe("serializeVariablesToEnv", () => {
  it("quotes values with whitespace and special chars, leaves simple values bare, and omits sealed values", () => {
    const text = serializeVariablesToEnv([
      plain("DB_USER", "pscale_api_x"),
      plain("APP_URL", "https://example.com/path"),
      plain("MESSAGE", "hello world"),
      plain("MULTI", "line1\nline2"),
      plain("EMPTY", ""),
      sealed("DB_PASSWORD"),
    ]);
    expect(text).toBe(
      [
        "DB_USER=pscale_api_x",
        "APP_URL=https://example.com/path",
        'MESSAGE="hello world"',
        'MULTI="line1\\nline2"',
        'EMPTY=""',
      ].join("\n"),
    );
  });
});

describe("parseEnv", () => {
  it("parses bare, double-quoted, single-quoted values and skips comments/blank lines", () => {
    const entries = parseEnvOk(
      [
        "# header",
        "",
        "FOO=bar",
        'GREETING="hello world"',
        "RAW_LITERAL='no \\n escapes'",
        "EMPTY=",
      ].join("\n"),
    );
    expect(entries).toEqual([
      { key: "FOO", value: "bar" },
      { key: "GREETING", value: "hello world" },
      { key: "RAW_LITERAL", value: "no \\n escapes" },
      { key: "EMPTY", value: "" },
    ]);
  });

  it("normalizes keys to uppercase", () => {
    expect(parseEnvOk("port=80\nApi_Key=secret")).toEqual([
      { key: "PORT", value: "80" },
      { key: "API_KEY", value: "secret" },
    ]);
  });

  it("interprets \\n inside double quotes", () => {
    const [entry] = parseEnvOk('MULTI="line1\\nline2"');
    expect(entry).toEqual({ key: "MULTI", value: "line1\nline2" });
  });

  it("rejects invalid lines", () => {
    expect(expectErr(parseEnv("not a key=value pair"))._tag).toBe(
      "RawEditorParseError",
    );
    expect(expectErr(parseEnv('UNTERMINATED="oops')).message).toMatch(
      /unterminated/i,
    );
  });

  it("merges duplicate keys keeping the last value and original position", () => {
    expect(parseEnvOk("FOO=1\nBAR=x\nFOO=2")).toEqual([
      { key: "FOO", value: "2" },
      { key: "BAR", value: "x" },
    ]);
    // Collisions only visible after upper-casing still merge.
    expect(parseEnvOk("foo=1\nFOO=2")).toEqual([{ key: "FOO", value: "2" }]);
  });
});

describe("parseJson", () => {
  it("parses an object of strings", () => {
    expect(expectOk(parseJson('{"FOO":"bar","BAZ":"qux"}'))).toEqual([
      { key: "FOO", value: "bar" },
      { key: "BAZ", value: "qux" },
    ]);
  });

  it("normalizes JSON keys to uppercase", () => {
    expect(expectOk(parseJson('{"port":"80","Api_Key":"secret"}'))).toEqual([
      { key: "PORT", value: "80" },
      { key: "API_KEY", value: "secret" },
    ]);
  });

  it("rejects bad shapes", () => {
    expect(expectErr(parseJson("[]")).message).toMatch(/object/i);
    expect(expectErr(parseJson('{"FOO": 1}')).message).toMatch(/string/i);
    expect(expectErr(parseJson('{"1BAD":"x"}')).message).toMatch(/variable key/i);
  });

  it("treats blank input as no entries", () => {
    expect(expectOk(parseJson(""))).toEqual([]);
    expect(expectOk(parseJson("   \n  "))).toEqual([]);
  });

  it("merges keys that collide after upper-casing, keeping the last value", () => {
    expect(expectOk(parseJson('{"foo":"1","FOO":"2"}'))).toEqual([
      { key: "FOO", value: "2" },
    ]);
  });
});

describe("findDuplicateEnvKeys", () => {
  it("reports upper-cased keys appearing more than once, ignoring noise", () => {
    expect(
      findDuplicateEnvKeys("FOO=1\nfoo=2\nBAR=x\n# comment\nnot a line"),
    ).toEqual(["FOO"]);
  });

  it("returns an empty array when there are no duplicates", () => {
    expect(findDuplicateEnvKeys("FOO=1\nBAR=2")).toEqual([]);
  });
});

describe("findDuplicateJsonKeys", () => {
  it("reports case collisions and ignores invalid/non-object JSON", () => {
    expect(findDuplicateJsonKeys('{"foo":"1","FOO":"2"}')).toEqual(["FOO"]);
    expect(findDuplicateJsonKeys('{"FOO":"1","BAR":"2"}')).toEqual([]);
    expect(findDuplicateJsonKeys("not json")).toEqual([]);
    expect(findDuplicateJsonKeys("[]")).toEqual([]);
    expect(findDuplicateJsonKeys("")).toEqual([]);
  });
});

describe("diffVariables", () => {
  it("classifies creates, updates, deletes, and ignores sealed variables", () => {
    const current: VariableRecord[] = [
      plain("KEEP", "same"),
      plain("CHANGE", "old"),
      plain("REMOVE", "bye"),
      sealed("SECRET"),
    ];
    const parsed = parseEnvOk(
      [
        "KEEP=same",
        "CHANGE=new",
        "ADDED=fresh",
      ].join("\n"),
    );
    const diff = diffVariables(parsed, current);
    expect(diff.creates).toMatchObject([{ key: "ADDED", value: "fresh" }]);
    expect(diff.creates[0]?.id).toMatch(/^[0-9a-f-]{36}$/);
    expect(diff.updates).toEqual([
      { variableId: "id-CHANGE", key: "CHANGE", value: "new" },
    ]);
    expect(diff.deletes).toEqual(["id-REMOVE"]);
  });

  it("treats one-to-one key replacements as create plus delete", () => {
    const diff = diffVariables(parseEnvOk("BAR=new"), [
      plain("FOO", "old", "variable-1"),
    ]);

    expect(diff.creates).toMatchObject([{ key: "BAR", value: "new" }]);
    expect(diff.updates).toEqual([]);
    expect(diff.deletes).toEqual(["variable-1"]);
  });
});

describe("findSealedVariableNameCollisions", () => {
  it("returns submitted keys that match omitted sealed variables", () => {
    const parsed = parseEnvOk("api_key=new-value\nPORT=3000");

    expect(
      findSealedVariableNameCollisions(parsed, [
        sealed("API_KEY"),
        plain("PORT", "80"),
      ]),
    ).toEqual(["API_KEY"]);
  });

  it("ignores duplicate collisions after the first report", () => {
    expect(
      findSealedVariableNameCollisions(
        [
          { key: "API_KEY", value: "one" },
          { key: "API_KEY", value: "two" },
        ],
        [sealed("API_KEY")],
      ),
    ).toEqual(["API_KEY"]);
  });
});

describe("getSealedVariableCollisionMessage", () => {
  it("names the sealed variable and points to the supported remedies", () => {
    expect(getSealedVariableCollisionMessage("API_KEY")).toBe(
      "You already have a sealed variable named API_KEY. Please rename the variable or delete the sealed variable.",
    );
  });
});

describe("serializeVariablesToJson", () => {
  it("omits sealed values", () => {
    const json = serializeVariablesToJson([
      plain("FOO", "bar"),
      sealed("API_KEY"),
    ]);
    expect(JSON.parse(json)).toEqual({
      FOO: "bar",
    });
  });
});
