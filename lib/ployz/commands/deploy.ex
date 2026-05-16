defmodule Ployz.Commands.Deploy do
  @moduledoc """
  Explicit deploy command for the BEAM/Mnesia MVP.

  This module owns command ordering, candidate filtering, runtime probing, and
  the atomic commit boundary. Durable writes are delegated to narrow metadata
  modules so storage ownership remains outside command orchestration.
  """

  alias Ployz.Manifest

  @type receipt :: map()
  @type result :: {:ok, receipt()} | {:error, term(), receipt() | nil}

  @spec run(Path.t(), keyword(), map()) :: result()
  def run(path, opts \\ [], deps \\ default_deps()) do
    with {:ok, manifest} <- Manifest.parse_file(path) do
      run_manifest(manifest, opts, deps)
    else
      {:error, reason} -> {:error, reason, nil}
    end
  end

  @spec run_manifest(Manifest.t(), keyword(), map()) :: result()
  def run_manifest(%Manifest{} = manifest, opts \\ [], deps \\ default_deps()) do
    command_id = Keyword.get_lazy(opts, :command_id, &new_command_id/0)
    owner = Keyword.get(opts, :owner, node())
    lease_key = {:deploy, manifest.service}

    receipt = running_receipt(command_id, owner, "deploy", lease_key)

    with :ok <- create_command(deps.commands, receipt),
         {:ok, lease} <-
           acquire_lease(deps.leases, lease_key, owner, Keyword.get(opts, :lease_ttl_ms, 60_000)) do
      run_with_lease(manifest, opts, deps, Map.put(receipt, :lease_token, lease.token), lease)
    else
      {:error, reason} ->
        fail_without_lease(deps.commands, receipt, reason)
    end
  end

  def default_deps do
    %{
      commands: Ployz.Metadata.Commands,
      leases: Ployz.Metadata.Leases,
      machines: Ployz.Metadata.Machines,
      runtime: Ployz.Runtime.Server,
      transaction: Ployz.Metadata.Tables,
      revisions: Ployz.Metadata.Revisions,
      services: Ployz.Metadata.Services,
      routes: Ployz.Metadata.Routes,
      gateway: Ployz.Gateway.Projection
    }
  end

  defp run_with_lease(manifest, opts, deps, receipt, lease) do
    lease_key = receipt.lease_key

    result =
      with {:ok, candidates} <-
             active_reachable_candidates(deps, Keyword.get(opts, :timeout_ms, 5_000)),
           {:ok, selected} <- select_members(candidates, manifest.instances),
           {:ok, runtime_evidence} <-
             start_and_probe(
               deps.runtime,
               selected,
               manifest,
               Keyword.get(opts, :timeout_ms, 5_000)
             ),
           {:ok, committed} <- commit(deps, manifest, receipt, lease, selected, runtime_evidence),
           {:ok, final} <- refresh_gateway(deps, committed) do
        {:ok, final}
      end

    _ = call(deps.leases, :release, [lease_key, lease.token])

    case result do
      {:ok, final} ->
        {:ok, final}

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

  defp active_reachable_candidates(deps, timeout_ms) do
    with {:ok, active} <- active_members(deps.machines),
         {:ok, reachable} <- reachable_members(deps.runtime, active, timeout_ms) do
      {:ok, reachable}
    end
  end

  defp active_members(module) do
    cond do
      exports?(module, :active_runtime_members, 0) ->
        call(module, :active_runtime_members, [])

      exports?(module, :selectable, 0) ->
        {:ok, call(module, :selectable, [])}

      true ->
        {:error, {:missing_metadata_contract, module, :active_runtime_members}}
    end
  end

  defp reachable_members(module, active, timeout_ms) do
    cond do
      exports?(module, :reachable_members, 2) ->
        call(module, :reachable_members, [active, timeout_ms])

      exports?(module, :members, 0) ->
        members = call(module, :members, [])

        if active == [] do
          {:error, :no_active_runtime_members}
        else
          {:ok, Enum.take(members, length(active))}
        end

      true ->
        {:error, {:missing_runtime_contract, module, :reachable_members}}
    end
  end

  defp select_members(candidates, needed) do
    selected = Enum.take(candidates, needed)

    if length(selected) == needed do
      {:ok, selected}
    else
      {:error, {:no_reachable_runtime_members, required: needed, available: length(candidates)}}
    end
  end

  defp commit(deps, manifest, receipt, lease, selected, runtime_evidence) do
    call(deps.transaction, :transaction, [
      fn ->
        {:ok, revision} = next_revision(deps.revisions, manifest.service)

        deploy_revision = %{
          service: manifest.service,
          revision: revision,
          manifest: manifest,
          members: selected,
          runtime_evidence: runtime_evidence,
          committed_at: now_ms()
        }

        :ok = put_revision(deps.revisions, deploy_revision)
        :ok = put_head(deps, manifest.service, revision)
        :ok = replace_routes(deps.routes, manifest.service, revision, manifest.routes)

        committed =
          receipt
          |> Map.merge(%{
            status: :committed,
            phase: :committed,
            lease_token: lease.token,
            service: manifest.service,
            revision: revision,
            members: selected,
            route_count: length(manifest.routes),
            finished_at: now_ms()
          })

        :ok = finish_command(deps.commands, committed)
        committed
      end
    ])
  end

  defp start_and_probe(module, selected, manifest, timeout_ms) do
    cond do
      exports?(module, :start_and_probe, 3) ->
        call(module, :start_and_probe, [selected, manifest, timeout_ms])

      exports?(module, :start, 3) and exports?(module, :probe, 3) ->
        params = %{service: manifest.service, image: manifest.image, routes: manifest.routes}

        selected
        |> Enum.reduce_while({:ok, []}, fn member, {:ok, acc} ->
          with {:ok, started} <- call(module, :start, [member, params, timeout_ms]),
               {:ok, probe} <- call(module, :probe, [member, params, timeout_ms]) do
            {:cont, {:ok, [%{member: member, started: started, probe: probe} | acc]}}
          else
            {:error, reason} -> {:halt, {:error, reason}}
          end
        end)
        |> case do
          {:ok, evidence} -> {:ok, Enum.reverse(evidence)}
          error -> error
        end

      true ->
        {:error, {:missing_runtime_contract, module, :start_and_probe}}
    end
  end

  defp create_command(module, receipt) do
    if exports?(module, :create, 1) do
      call(module, :create, [receipt])
    else
      call(module, :create_running, [
        receipt.id,
        :deploy,
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

  defp next_revision(module, service) do
    cond do
      exports?(module, :next_for_service, 1) ->
        call(module, :next_for_service, [service])

      exports?(module, :get_head, 1) ->
        case call(module, :get_head, [service]) do
          {:ok, %{revision: revision}} -> {:ok, revision + 1}
          {:error, :not_found} -> {:ok, 1}
          {:error, reason} -> {:error, reason}
        end

      true ->
        {:error, {:missing_metadata_contract, module, :next_for_service}}
    end
  end

  defp put_revision(module, row) do
    cond do
      exports?(module, :put, 1) ->
        call!(module, :put, [row])

      exports?(module, :put_revision, 3) ->
        call!(module, :put_revision, [row.service, row.revision, row])

      true ->
        throw({:metadata_commit_failed, :put_revision, :missing_contract})
    end
  end

  defp put_head(deps, service, revision) do
    cond do
      exports?(deps.services, :put_head, 2) ->
        call!(deps.services, :put_head, [service, revision])

      exports?(deps.revisions, :put_head, 2) ->
        call!(deps.revisions, :put_head, [service, revision])

      true ->
        throw({:metadata_commit_failed, :put_head, :missing_contract})
    end
  end

  defp replace_routes(module, service, revision, routes) do
    cond do
      exports?(module, :replace_for_service, 3) ->
        call!(module, :replace_for_service, [service, revision, routes])

      exports?(module, :put, 2) ->
        Enum.each(routes, fn route ->
          host = Map.get(route, :host) || Map.fetch!(route, "host")
          call!(module, :put, [host, Map.merge(route, %{service: service, revision: revision})])
        end)

        :ok

      true ->
        throw({:metadata_commit_failed, :replace_routes, :missing_contract})
    end
  end

  defp refresh_gateway(deps, receipt) do
    case call(deps.gateway, :refresh, []) do
      {:ok, freshness} ->
        {:ok, Map.put(receipt, :gateway, freshness)}

      {:error, reason} ->
        {:ok, Map.put(receipt, :gateway, %{status: :last_good_preserved, error: reason})}
    end
  end

  defp running_receipt(command_id, owner, kind, lease_key) do
    %{
      id: command_id,
      kind: kind,
      owner: owner,
      phase: :started,
      status: :running,
      lease_key: lease_key,
      lease_token: nil,
      started_at: now_ms(),
      last_error: nil
    }
  end

  defp fail_receipt(receipt, reason) do
    receipt
    |> Map.merge(%{
      status: :failed,
      phase: :failed,
      last_error: reason,
      finished_at: now_ms()
    })
  end

  defp call(module, function, args) when is_atom(module), do: apply(module, function, args)
  defp call(fun, _function, args) when is_function(fun), do: apply(fun, args)

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
