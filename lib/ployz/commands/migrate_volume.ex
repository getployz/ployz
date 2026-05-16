defmodule Ployz.Commands.MigrateVolume do
  @moduledoc """
  ZFS volume migration command.

  The command promotes a new volume generation only after destination
  verification succeeds. Failed send/receive/verify steps leave durable volume
  truth unchanged and finish the command receipt as failed.
  """

  @spec run(String.t(), String.t(), keyword(), map()) ::
          {:ok, map()} | {:error, term(), map() | nil}
  def run(volume, destination, opts \\ [], deps \\ default_deps())
      when is_binary(volume) and is_binary(destination) do
    command_id = Keyword.get_lazy(opts, :command_id, &new_command_id/0)
    owner = Keyword.get(opts, :owner, node())
    lease_key = {:volume, volume}
    receipt = running_receipt(command_id, owner, lease_key, volume, destination)

    with :ok <- create_command(deps.commands, receipt),
         {:ok, lease} <-
           acquire_lease(deps.leases, lease_key, owner, Keyword.get(opts, :lease_ttl_ms, 720_000)) do
      run_with_lease(
        volume,
        destination,
        opts,
        deps,
        Map.put(receipt, :lease_token, lease.token),
        lease
      )
    else
      {:error, reason} ->
        fail_without_lease(deps.commands, receipt, reason)
    end
  end

  def default_deps do
    %{
      commands: Ployz.Metadata.Commands,
      leases: Ployz.Metadata.Leases,
      volumes: Ployz.Metadata.Volumes,
      transaction: Ployz.Metadata.Tables,
      substrate: Ployz.Substrate.Zfs
    }
  end

  defp run_with_lease(volume, destination, opts, deps, receipt, lease) do
    result =
      safe_work(fn ->
        with {:ok, snapshot} <-
               call(deps.substrate, :snapshot, [volume, Keyword.get(opts, :timeout_ms, 30_000)]),
             {:ok, stream} <-
               call(deps.substrate, :send, [snapshot, Keyword.get(opts, :timeout_ms, 300_000)]),
             {:ok, transfer} <-
               call(deps.substrate, :recv, [
                 stream,
                 destination,
                 Keyword.get(opts, :timeout_ms, 300_000)
               ]),
             :ok <-
               call(deps.substrate, :verify, [
                 transfer,
                 Keyword.get(opts, :timeout_ms, 30_000)
               ]),
             {:ok, committed} <- commit(deps, volume, destination, receipt, lease, transfer) do
          {:ok, committed}
        end
      end)

    release_lease(deps.leases, receipt.lease_key, lease.token)

    case result do
      {:ok, committed} ->
        {:ok, committed}

      {:error, reason} ->
        failed = fail_receipt(receipt, reason)
        _ = finish_command(deps.commands, failed)
        {:error, reason, failed}
    end
  end

  defp fail_without_lease(commands, receipt, reason) do
    failed = fail_receipt(receipt, reason)
    _ = finish_command(commands, failed)
    {:error, reason, failed}
  end

  defp commit(deps, volume, destination, receipt, _lease, transfer) do
    call(deps.transaction, :transaction, [
      fn ->
        :ok = assert_lease(deps.leases, receipt.lease_key, receipt.lease_token)
        {:ok, generation} = next_generation(deps.volumes, volume)

        row = %{
          volume: volume,
          generation: generation,
          machine: destination,
          dataset_ref: Map.fetch!(transfer, :dataset_ref),
          verified_at: now_ms()
        }

        :ok = promote_generation(deps.volumes, volume, row)

        committed =
          Map.merge(receipt, %{
            status: :committed,
            phase: :committed,
            lease_token: nil,
            generation: generation,
            dataset_ref: row.dataset_ref,
            finished_at: now_ms()
          })

        :ok = finish_command(deps.commands, committed)
        committed
      end
    ])
  end

  defp create_command(module, receipt) do
    call(module, :create, [receipt])
  end

  defp finish_command(module, %{status: :committed} = receipt) do
    call(module, :finish, [receipt])
  end

  defp finish_command(module, %{status: :failed} = receipt) do
    call(module, :finish, [receipt])
  end

  defp acquire_lease(module, key, owner, ttl_ms) do
    call(module, :acquire, [key, owner, ttl_ms])
  end

  defp assert_lease(module, key, token) do
    call!(module, :assert_current, [key, token])
  end

  defp next_generation(module, volume) do
    call(module, :next_generation, [volume])
  end

  defp promote_generation(module, _volume, row) do
    call!(module, :promote_generation, [row])
  end

  defp running_receipt(command_id, owner, lease_key, volume, destination) do
    %{
      id: command_id,
      kind: "volume.migrate",
      owner: owner,
      volume: volume,
      destination: destination,
      phase: :started,
      status: :running,
      lease_key: lease_key,
      lease_token: nil,
      started_at: now_ms(),
      last_error: nil
    }
  end

  defp fail_receipt(receipt, reason) do
    Map.merge(receipt, %{
      status: :failed,
      phase: :failed,
      lease_token: nil,
      last_error: reason,
      finished_at: now_ms()
    })
  end

  defp call(module, function, args) when is_atom(module), do: apply(module, function, args)

  defp call!(module, function, args) do
    case call(module, function, args) do
      :ok -> :ok
      {:ok, value} -> value
      {:error, reason} -> throw({:metadata_commit_failed, function, reason})
    end
  end

  defp safe_work(fun) do
    fun.()
  rescue
    exception ->
      {:error,
       %{
         code: :command_crashed,
         exception: inspect(exception.__struct__),
         message: Exception.message(exception)
       }}
  catch
    :exit, reason ->
      {:error, %{code: :command_exited, detail: inspect(reason)}}

    kind, reason ->
      {:error, %{code: :command_threw, kind: kind, detail: inspect(reason)}}
  end

  defp release_lease(module, key, token) do
    _ = call(module, :release, [key, token])
    :ok
  rescue
    _error -> :ok
  catch
    _kind, _reason -> :ok
  end

  defp new_command_id, do: "cmd-" <> Base.encode16(:crypto.strong_rand_bytes(8), case: :lower)
  defp now_ms, do: System.system_time(:millisecond)
end
