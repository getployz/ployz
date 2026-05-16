defmodule Ployz.Metadata.Tables do
  @moduledoc false

  @compile {:no_warn_undefined, {:mnesia, :delete, 1}}
  @compile {:no_warn_undefined, {:mnesia, :is_transaction, 0}}
  @compile {:no_warn_undefined, {:mnesia, :match_object, 1}}
  @compile {:no_warn_undefined, {:mnesia, :read, 2}}
  @compile {:no_warn_undefined, {:mnesia, :transaction, 1}}
  @compile {:no_warn_undefined, {:mnesia, :write, 1}}

  @key_value_tables [
    :machines,
    :commands,
    :services,
    :service_heads,
    :revisions,
    :routes,
    :certs,
    :volumes
  ]

  @tables @key_value_tables ++ [:leases]

  def key_value_tables, do: @key_value_tables
  def tables, do: @tables

  def transaction(fun) when is_function(fun, 0) do
    case :mnesia.transaction(fun) do
      {:atomic, value} -> {:ok, value}
      {:aborted, reason} -> {:error, reason}
    end
  end

  def write(table, key, value) when table in @key_value_tables do
    activity(fn -> :mnesia.write({table, key, value}) end)
  end

  def read(table, key) when table in @key_value_tables do
    activity(fn ->
      case :mnesia.read(table, key) do
        [{^table, ^key, value}] -> {:ok, value}
        [] -> {:error, :not_found}
      end
    end)
  end

  def delete(table, key) when table in @key_value_tables do
    activity(fn -> :mnesia.delete({table, key}) end)
  end

  def all(table) when table in @key_value_tables do
    activity(fn ->
      table
      |> match_all()
      |> Enum.map(fn {^table, key, value} -> {key, value} end)
    end)
  end

  defp match_all(table), do: :mnesia.match_object({table, :_, :_})

  defp activity(fun) do
    if :mnesia.is_transaction() do
      fun.()
    else
      case :mnesia.transaction(fun) do
        {:atomic, value} -> value
        {:aborted, reason} -> {:error, reason}
      end
    end
  end
end
