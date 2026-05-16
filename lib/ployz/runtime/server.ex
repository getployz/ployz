defmodule Ployz.Runtime.Server do
  @moduledoc """
  Runtime role process that delegates substrate work to the helper seam.
  """

  use GenServer

  alias Ployz.Substrate.Port, as: SubstratePort
  alias Ployz.Substrate.Protocol

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
      |> MapSet.new()

    reachable =
      active_members
      |> Enum.map(&runtime_pid/1)
      |> Enum.filter(fn
        pid when is_pid(pid) -> MapSet.member?(live, pid) and Process.alive?(pid)
        _other -> false
      end)

    cond do
      active_members == [] -> {:error, :no_active_runtime_members}
      reachable == [] -> {:error, :no_live_runtime_members}
      true -> {:ok, reachable}
    end
  end

  @spec start_and_probe([GenServer.server()], Ployz.Manifest.t(), timeout()) ::
          {:ok, [map()]} | {:error, term()}
  def start_and_probe(members, manifest, timeout) when is_list(members) do
    start_and_probe(members, manifest, timeout, [], [])
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
         :ok <- join_group(group) do
      {:ok,
       %{
         group: group,
         port: Keyword.get(opts, :port),
         port_opts: Keyword.get(opts, :port_opts, [])
       }}
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
    reply_with_port(state, fn port -> SubstratePort.docker_start(port, params, timeout) end)
  end

  def handle_call({:substrate, :stop, params, timeout}, _from, state) do
    reply_with_port(state, fn port -> SubstratePort.docker_stop(port, params, timeout) end)
  end

  def handle_call({:substrate, :inspect, params, timeout}, _from, state) do
    reply_with_port(state, fn port -> SubstratePort.docker_inspect(port, params, timeout) end)
  end

  def handle_call({:substrate, :list_ployz, params, timeout}, _from, state) do
    reply_with_port(state, fn port -> SubstratePort.docker_list_ployz(port, params, timeout) end)
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

  defp start_and_probe([], _manifest, _timeout, evidence, _started) do
    {:ok, Enum.reverse(evidence)}
  end

  defp start_and_probe([member | rest], manifest, timeout, evidence, started) do
    params = docker_params(member, manifest)
    container = params["name"]

    case start(member, params, timeout) do
      {:ok, started_payload} ->
        started = [{member, container} | started]

        case inspect(member, %{"name" => container}, timeout) do
          {:ok, inspected} ->
            next_evidence = %{
              member: inspect_member(member),
              container: container,
              started: started_payload,
              inspected: inspected
            }

            start_and_probe(rest, manifest, timeout, [next_evidence | evidence], started)

          {:error, reason} ->
            cleanup_started(started, timeout)
            {:error, reason}
        end

      {:error, reason} ->
        cleanup_started(started, timeout)
        {:error, reason}
    end
  end

  defp cleanup_started(started, timeout) do
    Enum.each(started, fn {member, container} ->
      _ = stop(member, %{"name" => container}, timeout)
    end)
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

  defp runtime_pid(%{runtime_pid: pid}), do: pid
  defp runtime_pid(%{"runtime_pid" => pid}), do: pid
  defp runtime_pid(pid) when is_pid(pid), do: pid
  defp runtime_pid(_member), do: nil

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

  defp reply_with_port(state, fun) do
    case ensure_port(state) do
      {:ok, port, next_state} ->
        {:reply, fun.(port), next_state}

      {:error, error, next_state} ->
        {:reply, {:error, error}, next_state}
    end
  end

  defp ensure_port(%{port: port} = state) when is_pid(port), do: {:ok, port, state}

  defp ensure_port(%{port_opts: port_opts} = state) do
    case SubstratePort.start_link(port_opts) do
      {:ok, port} ->
        {:ok, port, %{state | port: port}}

      {:error, reason} ->
        error =
          Protocol.error("helper_unavailable", "substrate helper is unavailable", %{
            reason: inspect(reason)
          })

        {:error, error, state}
    end
  end
end
