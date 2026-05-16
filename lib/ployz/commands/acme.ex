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
      safe_work(fn ->
        with {:ok, issued} <-
               call(deps.issuer, :issue, [hostname, Keyword.get(opts, :timeout_ms, 30_000)]),
             :ok <- cert_ref_only(issued),
             {:ok, committed} <- commit(deps, hostname, receipt, lease, issued) do
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

  defp commit(deps, hostname, receipt, _lease, issued) do
    call(deps.transaction, :transaction, [
      fn ->
        :ok = assert_lease(deps.leases, receipt.lease_key, receipt.lease_token)
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
            lease_token: nil,
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

  defp next_cert_revision(module, hostname) do
    call(module, :next_revision, [hostname])
  end

  defp put_cert(module, hostname, row) do
    call!(module, :put, [hostname, row])
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
