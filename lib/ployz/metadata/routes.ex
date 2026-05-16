defmodule Ployz.Metadata.Routes do
  @moduledoc false

  alias Ployz.Metadata.Tables

  def put(hostname, route) when is_binary(hostname) and is_map(route) do
    Tables.write(:routes, hostname, Map.put(route, :hostname, hostname))
  end

  def get(hostname), do: Tables.read(:routes, hostname)

  def replace_for_service(service, revision, routes) do
    Enum.each(routes, fn route ->
      host = Map.fetch!(route, :host)
      :ok = put(host, Map.merge(route, %{service: service, revision: revision}))
    end)

    :ok
  end

  def committed do
    case Tables.all(:routes) do
      {:error, reason} ->
        {:error, reason}

      rows ->
        routes =
          rows
          |> Enum.map(fn {_host, route} -> route end)
          |> Enum.sort_by(fn route -> Map.get(route, :host) || Map.get(route, :hostname) end)

        {:ok, routes}
    end
  end
end
