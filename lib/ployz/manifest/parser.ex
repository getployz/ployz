defmodule Ployz.Manifest.Parser do
  @moduledoc false

  @scalar_sections ~w(env metadata)
  @list_sections ~w(routes volumes)

  @spec parse(String.t()) :: {:ok, map()} | {:error, term()}
  def parse(content) do
    content
    |> String.split("\n")
    |> Enum.with_index(1)
    |> Enum.reduce_while({:ok, %{data: %{}, section: nil, list_index: nil}}, &parse_line/2)
    |> case do
      {:ok, state} -> {:ok, state.data}
      {:error, _reason} = error -> error
    end
  end

  defp parse_line({line, number}, {:ok, state}) do
    case normalize(line) do
      :blank ->
        {:cont, {:ok, state}}

      {0, text} ->
        case split_pair(text) do
          {:ok, key, ""} when key in @scalar_sections ->
            {:cont,
             {:ok,
              %{state | data: Map.put_new(state.data, key, %{}), section: key, list_index: nil}}}

          {:ok, key, ""} when key in @list_sections ->
            {:cont,
             {:ok,
              %{state | data: Map.put_new(state.data, key, []), section: key, list_index: nil}}}

          {:ok, key, value} ->
            {:cont,
             {:ok,
              %{
                state
                | data: Map.put(state.data, key, cast_scalar(value)),
                  section: nil,
                  list_index: nil
              }}}

          :error ->
            {:halt, {:error, {:manifest_syntax, number, "expected key: value"}}}
        end

      {2, "- " <> text} ->
        with {:section, section} when section in @list_sections <- {:section, state.section},
             {:ok, key, value} <- split_pair(text) do
          item = %{key => cast_scalar(value)}
          items = Map.get(state.data, section, [])
          index = length(items)
          data = Map.put(state.data, section, items ++ [item])
          {:cont, {:ok, %{state | data: data, list_index: index}}}
        else
          {:section, _} ->
            {:halt, {:error, {:manifest_syntax, number, "list item outside list section"}}}

          :error ->
            {:halt, {:error, {:manifest_syntax, number, "expected list item key: value"}}}
        end

      {2, text} ->
        with {:section, section} when section in @scalar_sections <- {:section, state.section},
             {:ok, key, value} <- split_pair(text) do
          data = put_in(state.data, [section, key], cast_scalar(value))
          {:cont, {:ok, %{state | data: data}}}
        else
          {:section, section} when section in @list_sections ->
            {:halt,
             {:error,
              {:manifest_syntax, number, "list continuation must be indented four spaces"}}}

          {:section, _} ->
            {:halt, {:error, {:manifest_syntax, number, "nested value outside section"}}}

          :error ->
            {:halt, {:error, {:manifest_syntax, number, "expected key: value"}}}
        end

      {4, text} ->
        with {:section, section} when section in @list_sections <- {:section, state.section},
             index when is_integer(index) <- state.list_index,
             {:ok, key, value} <- split_pair(text) do
          data =
            update_in(
              state.data,
              [section],
              &List.update_at(&1, index, fn item -> Map.put(item, key, cast_scalar(value)) end)
            )

          {:cont, {:ok, %{state | data: data}}}
        else
          {:section, _} ->
            {:halt,
             {:error, {:manifest_syntax, number, "list continuation outside list section"}}}

          nil ->
            {:halt, {:error, {:manifest_syntax, number, "list continuation before list item"}}}

          :error ->
            {:halt, {:error, {:manifest_syntax, number, "expected key: value"}}}
        end

      {_indent, _text} ->
        {:halt, {:error, {:manifest_syntax, number, "unsupported indentation"}}}
    end
  end

  defp normalize(line) do
    line = strip_comment(line)

    if String.trim(line) == "" do
      :blank
    else
      indent = line |> String.length() |> Kernel.-(String.length(String.trim_leading(line)))
      {indent, String.trim(line)}
    end
  end

  defp strip_comment(line) do
    case String.split(line, "#", parts: 2) do
      [value | _] -> String.trim_trailing(value)
    end
  end

  defp split_pair(text) do
    case String.split(text, ":", parts: 2) do
      [key, value] ->
        key = String.trim(key)

        if key == "" do
          :error
        else
          {:ok, key, String.trim(value)}
        end

      [_] ->
        :error
    end
  end

  defp cast_scalar(value) do
    cond do
      value in ["true", "false"] ->
        value == "true"

      Regex.match?(~r/^\d+$/, value) ->
        String.to_integer(value)

      String.starts_with?(value, "\"") and String.ends_with?(value, "\"") ->
        String.trim(value, "\"")

      true ->
        value
    end
  end
end
