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

  def authorize_and_dispatch(actor, %{command: command, args: args} = request)
      when is_atom(command) and is_map(args) do
    with :ok <- Auth.authorize(actor, command) do
      dispatch(command, actor, args, Map.get(request, :opts, []))
    else
      {:error, reason} -> {:error, %{audience: :operator, phase: :authorize, reason: reason}}
    end
  end

  def authorize_and_dispatch(_actor, _request) do
    {:error, %{audience: :operator, phase: :decode, reason: :invalid_request}}
  end

  defp dispatch(:machine_add, actor, args, opts), do: MachineAdd.run(actor, args, opts)
  defp dispatch(:machine_remove, actor, args, opts), do: MachineRemove.run(actor, args, opts)
  defp dispatch(:deploy, _actor, %{manifest_path: path}, opts), do: Deploy.run(path, opts)
  defp dispatch(:cert_issue, _actor, %{hostname: hostname}, opts), do: Acme.issue(hostname, opts)

  defp dispatch(:migrate_volume, _actor, %{volume: volume, destination: destination}, opts),
    do: MigrateVolume.run(volume, destination, opts)

  defp dispatch(:gateway_routes, _actor, _args, _opts) do
    with {:ok, snapshot} <- Ployz.Gateway.Projection.snapshot() do
      {:ok, snapshot.routes}
    end
  end

  defp dispatch(:status, _actor, _args, _opts), do: Ployz.Status.snapshot()

  defp dispatch(command, _actor, _args, _opts) do
    {:error, %{audience: :operator, phase: :dispatch, reason: {:unsupported_command, command}}}
  end
end
