defmodule Ployz.Substrate.Port do
  @moduledoc """
  Supervised owner for a JSON-lines substrate helper port.
  """

  use GenServer

  alias Ployz.Substrate.Protocol

  @default_timeout 30_000

  @type option ::
          {:name, GenServer.name()}
          | {:helper_path, binary()}
          | {:args, [binary()]}
          | {:timeout, timeout()}

  @spec start_link([option()]) :: GenServer.on_start()
  def start_link(opts \\ []) do
    {name_opts, opts} = Keyword.split(opts, [:name])
    GenServer.start_link(__MODULE__, opts, name_opts)
  end

  @spec request(GenServer.server(), Protocol.operation(), map(), timeout()) ::
          {:ok, Protocol.json_value()} | {:error, Protocol.protocol_error()}
  def request(server, op, params, timeout \\ @default_timeout) when is_map(params) do
    GenServer.call(server, {:request, op, params}, timeout)
  end

  @spec docker_start(GenServer.server(), map(), timeout()) ::
          {:ok, Protocol.json_value()} | {:error, Protocol.protocol_error()}
  def docker_start(server, params, timeout \\ @default_timeout),
    do: request(server, "docker.start", params, timeout)

  @spec docker_stop(GenServer.server(), map(), timeout()) ::
          {:ok, Protocol.json_value()} | {:error, Protocol.protocol_error()}
  def docker_stop(server, params, timeout \\ @default_timeout),
    do: request(server, "docker.stop", params, timeout)

  @spec docker_inspect(GenServer.server(), map(), timeout()) ::
          {:ok, Protocol.json_value()} | {:error, Protocol.protocol_error()}
  def docker_inspect(server, params, timeout \\ @default_timeout),
    do: request(server, "docker.inspect", params, timeout)

  @spec docker_list_ployz(GenServer.server(), map(), timeout()) ::
          {:ok, Protocol.json_value()} | {:error, Protocol.protocol_error()}
  def docker_list_ployz(server, params \\ %{}, timeout \\ @default_timeout),
    do: request(server, "docker.list_ployz", params, timeout)

  @impl GenServer
  def init(opts) do
    helper_path =
      Keyword.get_lazy(opts, :helper_path, fn ->
        Application.get_env(:ployz, :substrate_helper_path, "ployz-substrate-helper")
      end)

    args = Keyword.get(opts, :args, [])

    case open_port(helper_path, args) do
      {:ok, port} ->
        {:ok, %{port: port, pending: %{}, helper_path: helper_path}}

      {:error, reason} ->
        {:stop, reason}
    end
  end

  @impl GenServer
  def handle_call({:request, op, params}, from, %{port: port, pending: pending} = state) do
    case Protocol.encode_request(op, params) do
      {:ok, {request_id, frame}} ->
        case send_frame(port, frame, state.helper_path) do
          :ok ->
            {:noreply, %{state | pending: Map.put(pending, request_id, from)}}

          {:error, error} ->
            {:reply, {:error, error}, state}
        end

      {:error, error} ->
        {:reply, {:error, error}, state}
    end
  end

  @impl GenServer
  def handle_info({_port, {:data, {:eol, line}}}, state), do: handle_line(line, state)
  def handle_info({_port, {:data, {:noeol, line}}}, state), do: handle_oversized_line(line, state)

  def handle_info({_port, {:data, line}}, state) when is_binary(line),
    do: handle_line(line, state)

  def handle_info({_port, {:exit_status, status}}, %{pending: pending} = state) do
    error =
      Protocol.error("helper_exited", "substrate helper exited", %{
        exit_status: status,
        helper_path: state.helper_path
      })

    Enum.each(pending, fn {_request_id, from} -> GenServer.reply(from, {:error, error}) end)
    {:stop, {:helper_exited, status}, %{state | pending: %{}}}
  end

  def handle_info({:EXIT, port, reason}, %{port: port, pending: pending} = state) do
    error =
      Protocol.error("helper_port_closed", "substrate helper port closed", %{
        reason: inspect(reason),
        helper_path: state.helper_path
      })

    Enum.each(pending, fn {_request_id, from} -> GenServer.reply(from, {:error, error}) end)
    {:stop, reason, %{state | pending: %{}}}
  end

  def handle_info(_message, state), do: {:noreply, state}

  defp handle_line(line, state) do
    case Protocol.decode_response(line) do
      {:ok, request_id, payload} ->
        reply(request_id, {:ok, payload}, state)

      {:error, request_id, error} when is_binary(request_id) ->
        reply(request_id, {:error, error}, state)

      {:error, nil, error} ->
        fail_all_pending(error, {:invalid_helper_response, error}, state)
    end
  end

  defp handle_oversized_line(_line, state) do
    error =
      Protocol.error("frame_too_large", "substrate helper response frame exceeds limit", %{
        max_bytes: Protocol.max_frame_size()
      })

    fail_all_pending(error, :helper_frame_too_large, state)
  end

  defp fail_all_pending(error, stop_reason, %{pending: pending} = state) do
    Enum.each(pending, fn {_request_id, from} -> GenServer.reply(from, {:error, error}) end)
    {:stop, stop_reason, %{state | pending: %{}}}
  end

  defp reply(request_id, response, %{pending: pending} = state) do
    case Map.pop(pending, request_id) do
      {nil, _pending} ->
        {:noreply, state}

      {from, pending} ->
        GenServer.reply(from, response)
        {:noreply, %{state | pending: pending}}
    end
  end

  defp open_port(helper_path, args) when is_binary(helper_path) and is_list(args) do
    port =
      Port.open({:spawn_executable, helper_path}, [
        :binary,
        :exit_status,
        {:args, args},
        {:line, Protocol.max_frame_size()}
      ])

    {:ok, port}
  rescue
    error in ArgumentError ->
      {:error, {:helper_open_failed, Exception.message(error)}}
  end

  defp send_frame(port, frame, helper_path) do
    case Port.command(port, frame) do
      true ->
        :ok

      false ->
        {:error,
         Protocol.error("helper_send_failed", "substrate helper rejected request frame", %{
           helper_path: helper_path
         })}
    end
  rescue
    error in ArgumentError ->
      {:error,
       Protocol.error("helper_port_closed", "substrate helper port is closed", %{
         helper_path: helper_path,
         error: Exception.message(error)
       })}
  end
end
