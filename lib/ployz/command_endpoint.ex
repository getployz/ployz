defmodule Ployz.CommandEndpoint do
  @moduledoc """
  Local command ingress boundary.
  """

  alias Ployz.Auth
  alias Ployz.Commands.Acme
  alias Ployz.Commands.Deploy
  alias Ployz.Commands.MachineAdd
  alias Ployz.Commands.MachineRemove
  alias Ployz.Commands.MigrateVolume
  alias Ployz.Commands.Supervisor, as: CommandSupervisor

  def authorize_and_dispatch(actor, %{command: command, args: args} = request)
      when is_atom(command) and is_map(args) do
    with :ok <- Auth.authorize(actor, command) do
      command
      |> dispatch(actor, args, Map.get(request, :opts, []))
      |> normalize_result()
    else
      {:error, reason} ->
        {:error, %{audience: :operator, phase: :authorize, reason: reason, receipt: nil}}
    end
  end

  def authorize_and_dispatch(_actor, _request) do
    {:error, %{audience: :operator, phase: :decode, reason: :invalid_request, receipt: nil}}
  end

  defp dispatch(:machine_add, actor, args, opts),
    do: run_command(fn -> MachineAdd.run(actor, args, opts) end)

  defp dispatch(:machine_remove, actor, args, opts),
    do: run_command(fn -> MachineRemove.run(actor, args, opts) end)

  defp dispatch(:deploy, _actor, %{manifest_path: path}, opts),
    do: run_command(fn -> Deploy.run(path, opts) end)

  defp dispatch(:cert_issue, _actor, %{hostname: hostname}, opts),
    do: run_command(fn -> Acme.issue(hostname, opts) end)

  defp dispatch(:migrate_volume, _actor, %{volume: volume, destination: destination}, opts),
    do: run_command(fn -> MigrateVolume.run(volume, destination, opts) end)

  defp dispatch(:gateway_routes, _actor, _args, _opts) do
    with {:ok, snapshot} <- Ployz.Gateway.Projection.snapshot() do
      {:ok, snapshot.routes}
    end
  end

  defp dispatch(:status, _actor, _args, _opts), do: Ployz.Status.snapshot()

  defp dispatch(command, _actor, _args, _opts) do
    {:error,
     %{
       audience: :operator,
       phase: :dispatch,
       reason: {:unsupported_command, command},
       receipt: nil
     }}
  end

  defp run_command(fun), do: CommandSupervisor.run(fun)

  defp normalize_result({:ok, %{id: _id} = receipt}), do: {:ok, %{receipt: receipt}}
  defp normalize_result({:ok, result}), do: {:ok, %{result: result}}

  defp normalize_result({:error, reason, receipt}) do
    {:error,
     %{
       audience: :operator,
       phase: Map.get(receipt || %{}, :phase, :execute),
       reason: reason,
       receipt: receipt
     }}
  end

  defp normalize_result({:error, %{audience: :operator} = error}) do
    {:error, Map.put_new(error, :receipt, nil)}
  end

  defp normalize_result({:error, %{id: _id} = receipt}) do
    {:error,
     %{
       audience: :operator,
       phase: Map.get(receipt, :phase, :execute),
       reason: Map.get(receipt, :last_error, :command_failed),
       receipt: receipt
     }}
  end

  defp normalize_result({:error, reason}) do
    {:error, %{audience: :operator, phase: :execute, reason: reason, receipt: nil}}
  end
end
