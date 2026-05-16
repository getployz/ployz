defmodule Ployz.Commands.Acme do
  @moduledoc """
  Explicit ACME certificate issuance.

  ACME state is serialized by a per-host Mnesia lease. Certificate rows advance
  only after the issuer returns a certificate reference; PEM material is rejected
  at this boundary.
  """

  @pem_markers ["-----BEGIN", "PRIVATE KEY", "CERTIFICATE-----"]

  @spec issue(String.t(), keyword(), map()) :: {:ok, map()} | {:error, term(), map() | nil}
  def issue(hostname, opts \\ [], deps \\ default_deps()) when is_binary(hostname) do
    command_id = Keyword.get_lazy(opts, :command_id, &new_command_id/0)
    owner = Keyword.get(opts, :owner, node())
    lease_key = {:acme, hostname}
    receipt = running_receipt(command_id, owner, lease_key, hostname)

    with :ok <- create_command(deps.commands, receipt),
         {:ok, lease} <-
           acquire_lease(deps.leases, lease_key, owner, Keyword.get(opts, :lease_ttl_ms, 60_000)) do
      run_with_lease(hostname, opts, deps, Map.put(receipt, :lease_token, lease.token), lease)
    else
      {:error, reason} ->
        fail_without_lease(deps.commands, receipt, reason)
    end
  end

  def default_deps do
    %{
      commands: Ployz.Metadata.Commands,
      leases: Ployz.Metadata.Leases,
      certs: Ployz.Metadata.Certs,
      transaction: Ployz.Metadata.Tables,
      issuer: Ployz.Acme.Issuer
    }
  end

  defp run_with_lease(hostname, opts, deps, receipt, lease) do
    result =
      with {:ok, issued} <-
             call(deps.issuer, :issue, [hostname, Keyword.get(opts, :timeout_ms, 30_000)]),
           :ok <- cert_ref_only(issued),
           {:ok, committed} <- commit(deps, hostname, receipt, lease, issued) do
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

  defp commit(deps, hostname, receipt, lease, issued) do
    call(deps.transaction, :transaction, [
      fn ->
        {:ok, revision} = next_cert_revision(deps.certs, hostname)

        row = %{
          hostname: hostname,
          revision: revision,
          cert_ref: Map.fetch!(issued, :cert_ref),
          chain_ref: Map.get(issued, :chain_ref),
          issued_at: Map.get(issued, :issued_at, System.system_time(:second)),
          expires_at: Map.fetch!(issued, :expires_at)
        }

        :ok = put_cert(deps.certs, hostname, row)

        committed =
          Map.merge(receipt, %{
            status: :committed,
            phase: :committed,
            lease_token: lease.token,
            cert_ref: row.cert_ref,
            revision: revision,
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
        :cert_issue,
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

  defp next_cert_revision(module, hostname) do
    cond do
      exports?(module, :next_revision, 1) ->
        call(module, :next_revision, [hostname])

      exports?(module, :get, 1) ->
        case call(module, :get, [hostname]) do
          {:ok, %{revision: revision}} -> {:ok, revision + 1}
          {:error, :not_found} -> {:ok, 1}
          {:error, reason} -> {:error, reason}
        end

      true ->
        {:error, {:missing_metadata_contract, module, :next_revision}}
    end
  end

  defp put_cert(module, hostname, row) do
    cond do
      exports?(module, :put, 1) -> call!(module, :put, [row])
      exports?(module, :put, 2) -> call!(module, :put, [hostname, row])
      true -> throw({:metadata_commit_failed, :put_cert, :missing_contract})
    end
  end

  defp cert_ref_only(%{cert_ref: "ployz-cert://" <> _} = issued) do
    inspected = inspect(issued)

    if Enum.any?(@pem_markers, &String.contains?(inspected, &1)) do
      {:error, :acme_issuer_returned_secret_material}
    else
      :ok
    end
  end

  defp cert_ref_only(_issued), do: {:error, :acme_issuer_missing_cert_ref}

  defp running_receipt(command_id, owner, lease_key, hostname) do
    %{
      id: command_id,
      kind: "cert.issue",
      owner: owner,
      hostname: hostname,
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
