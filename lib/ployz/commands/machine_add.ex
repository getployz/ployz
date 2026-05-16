defmodule Ployz.Commands.MachineAdd do
  @moduledoc false

  alias Ployz.Cluster.Groups
  alias Ployz.Metadata.Commands
  alias Ployz.Metadata.Machines
  alias Ployz.Metadata.Tables

  def run(actor, args, opts \\ [])

  def run(actor, %{id: id} = args, opts) when is_binary(id) do
    command_id = Map.get(args, :command_id, new_command_id())
    readiness = Keyword.get(opts, :readiness, &default_readiness/1)

    with {:ok, _} <-
           Tables.transaction(fn ->
             :ok = Commands.create_running(command_id, :machine_add, actor, phase: :metadata)
             :ok = Machines.put_joining(id, Map.drop(args, [:command_id]))
           end),
         :ok <- prove_readiness(readiness, args),
         {:ok, receipt} <-
           Tables.transaction(fn ->
             :ok = Commands.mark_phase(command_id, :commit)
             :ok = Machines.activate(id)
             :ok = Commands.succeed(command_id, %{machine_id: id, status: :active})
             {:ok, receipt} = Commands.get(command_id)
             receipt
           end) do
      {:ok, receipt}
    else
      {:error, reason} ->
        fail(command_id, reason)
    end
  end

  def run(_actor, _args, _opts), do: {:error, %{phase: :decode, reason: :invalid_machine_add}}

  defp prove_readiness(readiness, args) do
    case readiness.(args) do
      :ok -> :ok
      {:error, reason} -> {:error, reason}
      other -> {:error, {:invalid_readiness_result, other}}
    end
  end

  defp default_readiness(%{runtime_pid: pid}) when is_pid(pid) do
    if Groups.member?(Groups.runtime_group(), pid), do: :ok, else: {:error, :runtime_not_ready}
  end

  defp default_readiness(_args), do: :ok

  defp fail(command_id, reason) do
    {:ok, receipt} =
      Tables.transaction(fn ->
        _ = Commands.fail(command_id, reason)

        case Commands.get(command_id) do
          {:ok, receipt} -> receipt
          {:error, :not_found} -> %{id: command_id, status: :failed, last_error: reason}
        end
      end)

    {:error, receipt}
  end

  defp new_command_id,
    do: "cmd-" <> Base.url_encode64(:crypto.strong_rand_bytes(12), padding: false)
end
