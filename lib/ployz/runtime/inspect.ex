defmodule Ployz.Runtime.Inspect do
  @moduledoc """
  Runtime inspection helpers built on the runtime server and stale classifier.
  """

  alias Ployz.Runtime.Server
  alias Ployz.Runtime.Stale

  @spec list_and_classify(GenServer.server(), Stale.committed(), timeout()) ::
          {:ok, [map()]} | {:error, Ployz.Substrate.Protocol.protocol_error()}
  def list_and_classify(runtime_server, committed, timeout \\ 30_000) when is_map(committed) do
    with {:ok, payload} <- Server.list_ployz(runtime_server, timeout),
         {:ok, resources} <- resources_from_payload(payload) do
      {:ok, Stale.classify(resources, committed)}
    end
  end

  @spec resources_from_payload(Ployz.Substrate.Protocol.json_value()) ::
          {:ok, [map()]} | {:error, map()}
  def resources_from_payload(%{"resources" => resources}) when is_list(resources),
    do: {:ok, resources}

  def resources_from_payload(resources) when is_list(resources), do: {:ok, resources}

  def resources_from_payload(_payload) do
    {:error,
     Ployz.Substrate.Protocol.error(
       "invalid_observation",
       "helper observation did not contain resources",
       %{}
     )}
  end
end
