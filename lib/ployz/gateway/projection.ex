defmodule Ployz.Gateway.Projection do
  @moduledoc """
  Gateway/DNS projection reader with last-good preservation.

  Refresh failures are visible in the returned snapshot metadata, but they do
  not replace the last successfully read committed routes and certificate refs.
  """

  use GenServer

  defstruct routes: [],
            certs: %{},
            revision: nil,
            refreshed_at: nil,
            freshness: :never_loaded,
            last_error: nil

  @type snapshot :: %__MODULE__{}

  def start_link(opts \\ []) do
    name = Keyword.get(opts, :name, __MODULE__)
    readers = Keyword.get(opts, :readers, default_readers())
    GenServer.start_link(__MODULE__, readers, name: name)
  end

  def refresh(server \\ __MODULE__), do: GenServer.call(server, :refresh)
  def snapshot(server \\ __MODULE__), do: GenServer.call(server, :snapshot)

  def init(readers), do: {:ok, %{readers: readers, snapshot: %__MODULE__{}}}

  def handle_call(:snapshot, _from, state), do: {:reply, {:ok, state.snapshot}, state}

  def handle_call(:refresh, _from, state) do
    case read_committed(state.readers) do
      {:ok, snapshot} ->
        {:reply, {:ok, freshness(snapshot)}, %{state | snapshot: snapshot}}

      {:error, reason} ->
        preserved = %{state.snapshot | freshness: :last_good, last_error: reason}
        {:reply, {:error, %{reason: reason, snapshot: preserved}}, %{state | snapshot: preserved}}
    end
  end

  def read_once(readers \\ default_readers()) do
    read_committed(readers)
  end

  defp read_committed(readers) do
    with {:ok, routes} <- apply(readers.routes, :committed, []),
         {:ok, certs} <- apply(readers.certs, :active_refs, []) do
      {:ok,
       %__MODULE__{
         routes: routes,
         certs: certs,
         revision: projection_revision(routes, certs),
         refreshed_at: System.system_time(:millisecond),
         freshness: :fresh,
         last_error: nil
       }}
    end
  end

  defp projection_revision(routes, certs) do
    :erlang.phash2({routes, certs})
  end

  defp freshness(snapshot) do
    %{
      status: snapshot.freshness,
      revision: snapshot.revision,
      refreshed_at: snapshot.refreshed_at
    }
  end

  defp default_readers do
    %{routes: Ployz.Metadata.Routes, certs: Ployz.Metadata.Certs}
  end
end
