defmodule Ployz.Commands.MachineRemove do
  @moduledoc false

  alias Ployz.Metadata.Commands
  alias Ployz.Metadata.Machines
  alias Ployz.Metadata.Tables

  def run(actor, args, opts \\ [])

  def run(actor, %{id: id} = args, _opts) when is_binary(id) do
    command_id = Map.get(args, :command_id, new_command_id())

    with {:ok, receipt} <-
           Tables.transaction(fn ->
             :ok = Commands.create_running(command_id, :machine_remove, actor, phase: :metadata)

             receipt =
               case Machines.mark_removed(id) do
                 :ok ->
                   :ok = Commands.succeed(command_id, %{machine_id: id, status: :removed})
                   {:ok, receipt} = Commands.get(command_id)
                   receipt

                 {:error, reason} ->
                   :ok = Commands.fail(command_id, reason)
                   {:ok, receipt} = Commands.get(command_id)
                   receipt
               end

             receipt
           end) do
      if receipt.status == :succeeded, do: {:ok, receipt}, else: {:error, receipt}
    else
      {:error, reason} -> {:error, %{command_id: command_id, status: :failed, reason: reason}}
    end
  end

  def run(_actor, _args, _opts), do: {:error, %{phase: :decode, reason: :invalid_machine_remove}}

  defp new_command_id,
    do: "cmd-" <> Base.url_encode64(:crypto.strong_rand_bytes(12), padding: false)
end
