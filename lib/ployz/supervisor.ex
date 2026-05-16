defmodule Ployz.Supervisor do
  @moduledoc false

  use Supervisor

  def start_link(opts) do
    Supervisor.start_link(__MODULE__, opts, name: Keyword.get(opts, :name, __MODULE__))
  end

  @impl true
  def init(opts) do
    Ployz.Metadata.Schema.boot!()
    ensure_pg_started!()

    command_supervisor_name =
      Keyword.get(opts, :command_supervisor_name, Ployz.Commands.Supervisor)

    gateway_name = Keyword.get(opts, :gateway_name, Ployz.Gateway.Projection)

    children = [
      {DynamicSupervisor, name: command_supervisor_name, strategy: :one_for_one},
      {Ployz.Gateway.Projection, name: gateway_name}
    ]

    Supervisor.init(children, strategy: :one_for_one)
  end

  defp ensure_pg_started! do
    case Process.whereis(:pg) do
      nil ->
        case :pg.start_link() do
          {:ok, _pid} -> :ok
          {:error, {:already_started, _pid}} -> :ok
        end

      _pid ->
        :ok
    end
  end
end
