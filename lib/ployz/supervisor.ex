defmodule Ployz.Supervisor do
  @moduledoc false

  use Supervisor

  def start_link(opts) do
    Supervisor.start_link(__MODULE__, opts, name: Keyword.get(opts, :name, __MODULE__))
  end

  @impl true
  def init(opts) do
    :ok = validate_distribution!(opts)
    Ployz.Metadata.Schema.boot!()
    ensure_pg_started!()

    command_supervisor_name =
      Keyword.get(opts, :command_supervisor_name, Ployz.Commands.Supervisor)

    gateway_name = Keyword.get(opts, :gateway_name, Ployz.Gateway.Projection)
    runtime_name = Keyword.get(opts, :runtime_name, Ployz.Runtime.Server)

    children = [
      {Ployz.Commands.Supervisor, name: command_supervisor_name},
      {Ployz.Runtime.Server, name: runtime_name},
      {Ployz.Gateway.Projection, name: gateway_name}
    ]

    Supervisor.init(children, strategy: :one_for_one)
  end

  defp validate_distribution!(opts) do
    mode =
      Keyword.get(opts, :distribution_mode, Application.get_env(:ployz, :distribution_mode, :dev))

    distribution_opts = [
      cookie_path:
        Keyword.get(opts, :cookie_path, Application.get_env(:ployz, :distribution_cookie_path)),
      bind_address:
        Keyword.get(opts, :bind_address, Application.get_env(:ployz, :distribution_bind_address))
    ]

    case Ployz.Cluster.Distribution.validate_config(mode, distribution_opts) do
      :ok -> :ok
      {:error, reason} -> raise "invalid Ployz distribution config: #{inspect(reason)}"
    end
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
