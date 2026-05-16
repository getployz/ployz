defmodule Ployz.Redactor do
  @moduledoc false

  @secret_key_fragments ~w[
    account auth authorization bearer cookie credential key pass password pem private secret token
  ]

  @safe_keys ~w[
    cert_ref chain_ref dataset_ref lease_key route_key
  ]

  def redact(value) when is_map(value) do
    value
    |> Enum.reject(fn {key, _value} -> secret_key?(key) end)
    |> Map.new(fn {key, nested} -> {key, redact(nested)} end)
  end

  def redact(values) when is_list(values), do: Enum.map(values, &redact/1)
  def redact(value), do: value

  defp secret_key?(key) do
    normalized =
      key
      |> to_string()
      |> String.downcase()

    normalized not in @safe_keys and
      Enum.any?(@secret_key_fragments, &String.contains?(normalized, &1))
  end
end
