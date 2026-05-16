defmodule Ployz.Metadata.Schema do
  @moduledoc """
  Owns Mnesia schema and table boot for the v2 metadata core.
  """

  @compile {:no_warn_undefined, {:mnesia, :create_schema, 1}}
  @compile {:no_warn_undefined, {:mnesia, :create_table, 2}}
  @compile {:no_warn_undefined, {:mnesia, :start, 0}}
  @compile {:no_warn_undefined, {:mnesia, :stop, 0}}
  @compile {:no_warn_undefined, {:mnesia, :wait_for_tables, 2}}

  alias Ployz.Metadata.Tables

  def boot!(opts \\ []) do
    configure_dir!(opts)
    ensure_schema!()
    ensure_started!()
    ensure_tables!(Keyword.get(opts, :storage, storage_mode()))
    :ok
  end

  def reset_for_test!(dir) do
    :mnesia.stop()
    File.rm_rf!(dir)
    File.mkdir_p!(dir)
    Application.put_env(:ployz, :metadata_dir, dir)
    Application.put_env(:ployz, :metadata_storage, :disc)
    boot!()
  end

  defp configure_dir!(opts) do
    dir = Keyword.get(opts, :dir, Application.get_env(:ployz, :metadata_dir, "tmp/mnesia/dev"))
    File.mkdir_p!(dir)
    Application.put_env(:mnesia, :dir, String.to_charlist(dir))
  end

  defp ensure_schema! do
    case :mnesia.create_schema([node()]) do
      :ok -> :ok
      {:error, {_, {:already_exists, _}}} -> :ok
      {:error, {_, {:already_exists, _, _}}} -> :ok
      {:error, {:already_exists, _}} -> :ok
      {:error, reason} -> raise "failed to create Mnesia schema: #{inspect(reason)}"
    end
  end

  defp ensure_started! do
    case :mnesia.start() do
      :ok -> :ok
      {:error, {:already_started, :mnesia}} -> :ok
    end
  end

  defp ensure_tables!(storage) do
    for table <- Tables.key_value_tables() do
      ensure_table!(table, [storage_option(storage), attributes: [:key, :value], type: :set])
    end

    ensure_table!(:leases, [
      storage_option(storage),
      attributes: [:key, :token, :owner, :expires_at],
      type: :set
    ])

    case :mnesia.wait_for_tables(Tables.tables(), 5_000) do
      :ok -> :ok
      {:timeout, tables} -> raise "timed out waiting for Mnesia tables: #{inspect(tables)}"
      {:error, reason} -> raise "failed waiting for Mnesia tables: #{inspect(reason)}"
    end
  end

  defp ensure_table!(table, options) do
    case :mnesia.create_table(table, options) do
      {:atomic, :ok} -> :ok
      {:aborted, {:already_exists, ^table}} -> :ok
      {:aborted, reason} -> raise "failed to create Mnesia table #{table}: #{inspect(reason)}"
    end
  end

  defp storage_option(:ram), do: {:ram_copies, [node()]}
  defp storage_option(:disc) when node() == :nonode@nohost, do: {:ram_copies, [node()]}
  defp storage_option(:disc), do: {:disc_copies, [node()]}

  defp storage_mode do
    Application.get_env(:ployz, :metadata_storage, :disc)
  end
end
