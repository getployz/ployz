defmodule Ployz.Runtime.Server do
  @moduledoc """
  Runtime role process that delegates substrate work to the helper seam.
  """

  use GenServer

  alias Ployz.Substrate.Port, as: SubstratePort

  @group Ployz.Cluster.Groups.runtime_group()
  @default_timeout 30_000

  @type option ::
          {:name, GenServer.name()}
          | {:port, GenServer.server()}
          | {:port_opts, keyword()}
          | {:group, atom()}

  @spec start_link([option()]) :: GenServer.on_start()
  def start_link(opts \\ []) do
    {name_opts, opts} = Keyword.split(opts, [:name])
    GenServer.start_link(__MODULE__, opts, name_opts)
  end

  @spec group() :: atom()
  def group, do: @group

  @spec members(atom()) :: [pid()]
  def members(group \\ @group) do
    ensure_pg_started()
    :pg.get_members(group)
  end

  @spec reachable_members([term()], timeout()) :: {:ok, [pid()]} | {:error, term()}
  def reachable_members(active_members, _timeout) when is_list(active_members) do
    live =
      @group
      |> members()
      |> Enum.filter(&Process.alive?/1)

    cond do
      active_members == [] -> {:error, :no_active_runtime_members}
      live == [] -> {:error, :no_live_runtime_members}
      true -> {:ok, Enum.take(live, length(active_members))}
    end
  end

  @spec start_and_probe([GenServer.server()], Ployz.Manifest.t(), timeout()) ::
          {:ok, [map()]} | {:error, term()}
  def start_and_probe(members, manifest, timeout) when is_list(members) do
    members
    |> Enum.reduce_while({:ok, []}, fn member, {:ok, acc} ->
      params = docker_params(member, manifest)

      with {:ok, started} <- start(member, params, timeout),
           {:ok, inspected} <- inspect(member, %{"name" => params["name"]}, timeout) do
        evidence = %{
          member: inspect_member(member),
          container: params["name"],
          started: started,
          inspected: inspected
        }

        {:cont, {:ok, [evidence | acc]}}
      else
        {:error, reason} -> {:halt, {:error, reason}}
      end
    end)
    |> case do
      {:ok, evidence} -> {:ok, Enum.reverse(evidence)}
      error -> error
    end
  end

  @spec bid(GenServer.server(), map(), timeout()) :: {:ok, map()}
  def bid(server, params, timeout \\ @default_timeout),
    do: GenServer.call(server, {:bid, params}, timeout)

  @spec start(GenServer.server(), map(), timeout()) ::
          {:ok, Ployz.Substrate.Protocol.json_value()}
          | {:error, Ployz.Substrate.Protocol.protocol_error()}
  def start(server, params, timeout \\ @default_timeout),
    do: call_substrate(server, :start, params, timeout)

  @spec stop(GenServer.server(), map(), timeout()) ::
          {:ok, Ployz.Substrate.Protocol.json_value()}
          | {:error, Ployz.Substrate.Protocol.protocol_error()}
  def stop(server, params, timeout \\ @default_timeout),
    do: call_substrate(server, :stop, params, timeout)

  @spec inspect(GenServer.server(), map(), timeout()) ::
          {:ok, Ployz.Substrate.Protocol.json_value()}
          | {:error, Ployz.Substrate.Protocol.protocol_error()}
  def inspect(server, params, timeout \\ @default_timeout),
    do: call_substrate(server, :inspect, params, timeout)

  @spec list_ployz(GenServer.server(), timeout()) ::
          {:ok, Ployz.Substrate.Protocol.json_value()}
          | {:error, Ployz.Substrate.Protocol.protocol_error()}
  def list_ployz(server, timeout \\ @default_timeout),
    do: call_substrate(server, :list_ployz, %{}, timeout)

  @spec probe(GenServer.server(), map(), timeout()) ::
          {:ok, Ployz.Substrate.Protocol.json_value()}
          | {:error, Ployz.Substrate.Protocol.protocol_error()}
  def probe(server, params, timeout \\ @default_timeout), do: inspect(server, params, timeout)

  @impl GenServer
  def init(opts) do
    group = Keyword.get(opts, :group, @group)

    with :ok <- ensure_pg_started(),
         :ok <- join_group(group),
         {:ok, port} <- resolve_port(opts) do
      {:ok, %{group: group, port: port}}
    else
      {:error, reason} -> {:stop, reason}
    end
  end

  @impl GenServer
  def handle_call({:bid, params}, _from, state) when is_map(params) do
    {:reply,
     {:ok, %{"node" => Atom.to_string(node()), "runtime" => "docker", "accepted" => true}}, state}
  end

  def handle_call({:substrate, :start, params, timeout}, _from, state) do
    {:reply, SubstratePort.docker_start(state.port, params, timeout), state}
  end

  def handle_call({:substrate, :stop, params, timeout}, _from, state) do
    {:reply, SubstratePort.docker_stop(state.port, params, timeout), state}
  end

  def handle_call({:substrate, :inspect, params, timeout}, _from, state) do
    {:reply, SubstratePort.docker_inspect(state.port, params, timeout), state}
  end

  def handle_call({:substrate, :list_ployz, params, timeout}, _from, state) do
    {:reply, SubstratePort.docker_list_ployz(state.port, params, timeout), state}
  end

  defp call_substrate(server, op, params, timeout) when is_map(params) do
    GenServer.call(server, {:substrate, op, params, timeout}, timeout + 1_000)
  end

  defp docker_params(member, manifest) do
    service = manifest.service
    suffix = :erlang.phash2({member, System.unique_integer([:positive])})

    %{
      "name" => "ployz-#{service}-#{suffix}",
      "image" => manifest.image,
      "args" => command_args(manifest.command),
      "env" => manifest.env,
      "labels" =>
        Map.merge(
          %{
            "ployz.service" => service,
            "ployz.managed_by" => "ployz-v2",
            "ployz.test" => Map.get(manifest.metadata, "e2e", "false")
          },
          metadata_labels(manifest.metadata)
        )
    }
  end

  defp command_args(nil), do: []

  defp command_args(command) when is_binary(command) do
    command
    |> String.split(" ", trim: true)
    |> Enum.reject(&(&1 == ""))
  end

  defp metadata_labels(metadata) do
    metadata
    |> Enum.reject(fn {key, _value} -> key in ["e2e"] end)
    |> Map.new(fn {key, value} -> {"ployz.metadata.#{key}", to_string(value)} end)
  end

  defp inspect_member(pid) when is_pid(pid), do: inspect(pid)
  defp inspect_member(member), do: inspect(member)

  defp ensure_pg_started do
    case Process.whereis(:pg) do
      nil ->
        case :pg.start_link() do
          {:ok, _pid} -> :ok
          {:error, {:already_started, _pid}} -> :ok
          {:error, reason} -> {:error, {:runtime_group_start_failed, reason}}
        end

      _pid ->
        :ok
    end
  end

  defp join_group(group) do
    case :pg.join(group, self()) do
      :ok -> :ok
      {:error, {:already_joined, _pid}} -> :ok
      {:error, reason} -> {:error, {:runtime_group_join_failed, reason}}
    end
  rescue
    error in ArgumentError -> {:error, {:runtime_group_join_failed, Exception.message(error)}}
  end

  defp resolve_port(opts) do
    case Keyword.fetch(opts, :port) do
      {:ok, port} ->
        {:ok, port}

      :error ->
        SubstratePort.start_link(Keyword.get(opts, :port_opts, []))
    end
  end
end
