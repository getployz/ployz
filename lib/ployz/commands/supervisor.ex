defmodule Ployz.Commands.Supervisor do
  @moduledoc false

  use DynamicSupervisor

  @default_timeout 120_000

  def start_link(opts \\ []) do
    DynamicSupervisor.start_link(__MODULE__, opts, name: Keyword.get(opts, :name, __MODULE__))
  end

  @impl DynamicSupervisor
  def init(_opts), do: DynamicSupervisor.init(strategy: :one_for_one)

  def run(fun, timeout \\ @default_timeout) when is_function(fun, 0) do
    parent = self()
    ref = make_ref()

    child_spec = %{
      id: {__MODULE__, ref},
      start: {Task, :start_link, [fn -> send(parent, {ref, self(), safe_call(fun)}) end]},
      restart: :temporary,
      type: :worker
    }

    with {:ok, pid} <- DynamicSupervisor.start_child(__MODULE__, child_spec) do
      monitor = Process.monitor(pid)

      receive do
        {^ref, ^pid, result} ->
          Process.demonitor(monitor, [:flush])
          result

        {:DOWN, ^monitor, :process, ^pid, reason} ->
          {:error,
           %{
             audience: :operator,
             phase: :execute,
             reason: %{code: :command_worker_exited, detail: inspect(reason)},
             receipt: nil
           }}
      after
        timeout ->
          _ = DynamicSupervisor.terminate_child(__MODULE__, pid)

          {:error,
           %{
             audience: :operator,
             phase: :execute,
             reason: %{code: :command_worker_timeout, timeout_ms: timeout},
             receipt: nil
           }}
      end
    else
      {:error, reason} ->
        {:error,
         %{
           audience: :operator,
           phase: :start,
           reason: %{code: :command_worker_start_failed, detail: inspect(reason)},
           receipt: nil
         }}
    end
  end

  defp safe_call(fun) do
    fun.()
  rescue
    exception ->
      {:error,
       %{
         audience: :operator,
         phase: :execute,
         reason: %{
           code: :command_worker_crashed,
           exception: inspect(exception.__struct__),
           message: Exception.message(exception)
         },
         receipt: nil
       }}
  catch
    kind, reason ->
      {:error,
       %{
         audience: :operator,
         phase: :execute,
         reason: %{code: :command_worker_threw, kind: kind, detail: inspect(reason)},
         receipt: nil
       }}
  end
end
