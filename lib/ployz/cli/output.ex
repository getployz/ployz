defmodule Ployz.Cli.Output do
  @moduledoc false

  @spec format(term(), keyword()) :: String.t()
  def format(value, opts \\ []) do
    case Keyword.get(opts, :format, :text) do
      :json -> jsonish(value) <> "\n"
      :text -> text(value) <> "\n"
    end
  end

  defp text({:ok, receipt}), do: text(receipt)
  defp text({:error, receipt}) when is_map(receipt), do: "error:\n#{text(receipt)}"
  defp text({:error, reason, nil}), do: "error: #{inspect(reason)}"
  defp text({:error, reason, receipt}), do: "error: #{inspect(reason)}\n#{text(receipt)}"

  defp text(%{status: status, id: id} = receipt) do
    lines = [
      "command #{id}: #{status}",
      maybe_line("service", Map.get(receipt, :service)),
      maybe_line("revision", Map.get(receipt, :revision)),
      maybe_line("hostname", Map.get(receipt, :hostname)),
      maybe_line("volume", Map.get(receipt, :volume)),
      maybe_line("generation", Map.get(receipt, :generation)),
      maybe_line("error", Map.get(receipt, :last_error))
    ]

    lines
    |> Enum.reject(&is_nil/1)
    |> Enum.join("\n")
  end

  defp text(value), do: inspect(value, pretty: true)

  defp maybe_line(_label, nil), do: nil
  defp maybe_line(label, value), do: "#{label}: #{inspect(value)}"

  defp jsonish(value) do
    value
    |> redact()
    |> inspect(limit: :infinity, printable_limit: :infinity)
  end

  defp redact(value) when is_map(value) do
    Map.new(value, fn
      {key, _value} when key in [:private_key, "private_key", :pem, "pem"] -> {key, "[redacted]"}
      {key, value} -> {key, redact(value)}
    end)
  end

  defp redact(values) when is_list(values), do: Enum.map(values, &redact/1)
  defp redact(value), do: value
end
