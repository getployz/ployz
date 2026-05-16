defmodule Ployz.Cli.Output do
  @moduledoc false

  @spec format(term(), keyword()) :: String.t()
  def format(value, opts \\ []) do
    case Keyword.get(opts, :format, :text) do
      :json ->
        Ployz.Substrate.Protocol.encode_json(value |> jsonable() |> Ployz.Redactor.redact()) <>
          "\n"

      :text ->
        text(value) <> "\n"
    end
  end

  defp text({:ok, %{receipt: receipt}}), do: text(receipt)
  defp text({:ok, %{result: result}}), do: text(result)
  defp text({:ok, receipt}), do: text(receipt)
  defp text({:error, %{reason: reason, receipt: nil}}), do: "error: #{inspect(reason)}"

  defp text({:error, %{reason: reason, receipt: receipt}}),
    do: "error: #{inspect(reason)}\n#{text(receipt)}"

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

  defp jsonable({:ok, %{receipt: receipt}}), do: %{"ok" => %{"receipt" => jsonable(receipt)}}
  defp jsonable({:ok, %{result: result}}), do: %{"ok" => %{"result" => jsonable(result)}}
  defp jsonable({:error, error}), do: %{"error" => jsonable(error)}

  defp jsonable({:error, reason, receipt}),
    do: %{"error" => jsonable(%{reason: reason, receipt: receipt})}

  defp jsonable(%{} = map) do
    Map.new(map, fn {key, value} -> {to_string(key), jsonable(value)} end)
  end

  defp jsonable(values) when is_list(values), do: Enum.map(values, &jsonable/1)
  defp jsonable(value) when is_atom(value), do: Atom.to_string(value)
  defp jsonable(value) when is_tuple(value), do: value |> Tuple.to_list() |> jsonable()
  defp jsonable(value), do: value
end
