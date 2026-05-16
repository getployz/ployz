defmodule Ployz.Cli.OutputTest do
  use ExUnit.Case, async: true

  test "json output is parseable JSON with redacted secret-like keys" do
    output =
      Ployz.Cli.Output.format(
        {:error,
         %{
           reason: %{token: "secret", safe: "value"},
           receipt: %{id: "cmd-1", status: :failed}
         }},
        format: :json
      )

    assert {:ok, decoded} = Ployz.Substrate.Protocol.decode_json(String.trim(output))
    assert decoded["error"]["reason"]["safe"] == "value"
    refute Map.has_key?(decoded["error"]["reason"], "token")
    assert decoded["error"]["receipt"]["status"] == "failed"
  end
end
