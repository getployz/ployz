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
           acquire_lease(deps.leases, lease_key, owner, Keyword.get(opts, :lease_ttl_ms, 60_000)) do
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
      with {:ok, snapshot} <-
             call(deps.substrate, :snapshot, [volume, Keyword.get(opts, :timeout_ms, 30_000)]),
           {:ok, transfer} <-
             call(deps.substrate, :send_receive, [
               snapshot,
               destination,
               Keyword.get(opts, :timeout_ms, 300_000)
             ]),
           :ok <-
             call(deps.substrate, :verify_destination, [
               transfer,
               Keyword.get(opts, :timeout_ms, 30_000)
             ]),
           {:ok, committed} <- commit(deps, volume, destination, receipt, lease, transfer) do
        {:ok, committed}
      end

    _ = call(deps.leases, :release, [receipt.lease_key, lease.token])

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

  defp commit(deps, volume, destination, receipt, lease, transfer) do
    call(deps.transaction, :transaction, [
      fn ->
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
            lease_token: lease.token,
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
    if exports?(module, :create, 1) do
      call(module, :create, [receipt])
    else
      call(module, :create_running, [
        receipt.id,
        :migrate_volume,
        %{role: :local_operator},
        owner: receipt.owner,
        phase: receipt.phase
      ])
    end
  end

  defp finish_command(module, %{status: :committed} = receipt) do
    if exports?(module, :finish, 1) do
      call(module, :finish, [receipt])
    else
      call(module, :succeed, [receipt.id, Map.drop(receipt, [:id, :status, :phase])])
    end
  end

  defp finish_command(module, %{status: :failed} = receipt) do
    if exports?(module, :finish, 1) do
      call(module, :finish, [receipt])
    else
      call(module, :fail, [receipt.id, receipt.last_error])
    end
  end

  defp acquire_lease(module, key, owner, ttl_ms) do
    case call(module, :acquire, [key, owner, ttl_ms]) do
      {:ok, %{token: _token} = lease} -> {:ok, lease}
      {:ok, token} when is_binary(token) -> {:ok, %{key: key, token: token, owner: owner}}
      {:error, reason} -> {:error, reason}
    end
  end

  defp next_generation(module, volume) do
    cond do
      exports?(module, :next_generation, 1) ->
        call(module, :next_generation, [volume])

      exports?(module, :get, 1) ->
        case call(module, :get, [volume]) do
          {:ok, %{generation: generation}} -> {:ok, generation + 1}
          {:error, :not_found} -> {:ok, 1}
          {:error, reason} -> {:error, reason}
        end

      true ->
        {:error, {:missing_metadata_contract, module, :next_generation}}
    end
  end

  defp promote_generation(module, volume, row) do
    cond do
      exports?(module, :promote_generation, 1) ->
        call!(module, :promote_generation, [row])

      exports?(module, :put, 2) ->
        call!(module, :put, [volume, row])

      true ->
        throw({:metadata_commit_failed, :promote_generation, :missing_contract})
    end
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

  defp exports?(module, function, arity) when is_atom(module) do
    Code.ensure_loaded?(module) and function_exported?(module, function, arity)
  end

  defp exports?(_module, _function, _arity), do: false

  defp new_command_id, do: "cmd-" <> Base.encode16(:crypto.strong_rand_bytes(8), case: :lower)
  defp now_ms, do: System.system_time(:millisecond)
end
