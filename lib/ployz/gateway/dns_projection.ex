defmodule Ployz.Gateway.DnsProjection do
  @moduledoc """
  DNS view over committed gateway routes.
  """

  alias Ployz.Gateway.Projection

  @spec records(Projection.snapshot()) :: [
          %{host: String.t(), service: String.t() | nil, revision: term()}
        ]
  def records(%Projection{routes: routes}) do
    Enum.map(routes, fn route ->
      %{
        host: Map.fetch!(route, :host),
        service: Map.get(route, :service),
        revision: Map.get(route, :revision)
      }
    end)
  end
end
