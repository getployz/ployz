defmodule Ployz.Metadata.Leases do
  @moduledoc false

  @compile {:no_warn_undefined, {:mnesia, :delete, 1}}
  @compile {:no_warn_undefined, {:mnesia, :is_transaction, 0}}
  @compile {:no_warn_undefined, {:mnesia, :read, 2}}
  @compile {:no_warn_undefined, {:mnesia, :transaction, 1}}
  @compile {:no_warn_undefined, {:mnesia, :write, 1}}

  def acquire(key, owner, ttl_ms, now_ms \\ System.system_time(:millisecond)) do
    activity(fn ->
      token = :crypto.strong_rand_bytes(16) |> Base.url_encode64(padding: false)
      expires_at = now_ms + ttl_ms

      case :mnesia.read(:leases, key) do
        [] ->
          :mnesia.write({:leases, key, token, owner, expires_at})
          {:ok, lease(key, token, owner, expires_at)}

        [{:leases, ^key, _existing_token, existing_owner, existing_expires_at}]
        when existing_expires_at > now_ms ->
          {:error, {:lease_held, existing_owner, existing_expires_at}}

        [{:leases, ^key, _existing_token, _existing_owner, existing_expires_at}]
        when existing_expires_at <= now_ms ->
          :mnesia.write({:leases, key, token, owner, expires_at})
          {:ok, lease(key, token, owner, expires_at)}
      end
    end)
  end

  def release(key, token) do
    activity(fn ->
      case :mnesia.read(:leases, key) do
        [{:leases, ^key, ^token, _owner, _expires_at}] ->
          :mnesia.delete({:leases, key})
          :ok

        [] ->
          :ok

        [{:leases, ^key, _other_token, _owner, _expires_at}] ->
          {:error, :token_mismatch}
      end
    end)
  end

  def assert_current(key, token, now_ms \\ System.system_time(:millisecond)) do
    activity(fn ->
      case :mnesia.read(:leases, key) do
        [{:leases, ^key, ^token, _owner, expires_at}] when expires_at > now_ms ->
          :ok

        [{:leases, ^key, ^token, _owner, expires_at}] ->
          {:error, {:lease_expired, expires_at}}

        [{:leases, ^key, _other_token, _owner, _expires_at}] ->
          {:error, :token_mismatch}

        [] ->
          {:error, :lease_missing}
      end
    end)
  end

  def get(key) do
    activity(fn ->
      case :mnesia.read(:leases, key) do
        [{:leases, ^key, token, owner, expires_at}] ->
          {:ok, lease(key, token, owner, expires_at)}

        [] ->
          {:error, :not_found}
      end
    end)
  end

  defp lease(key, token, owner, expires_at) do
    %{key: key, token: token, owner: owner, expires_at: expires_at}
  end

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
