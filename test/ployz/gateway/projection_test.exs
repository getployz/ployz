defmodule Ployz.Gateway.ProjectionTest do
  use ExUnit.Case, async: false

  alias Ployz.Gateway.Projection

  defmodule Routes do
    def committed, do: Agent.get(:gateway_projection_test, &Map.fetch!(&1, :routes))
  end

  defmodule Certs do
    def active_refs, do: Agent.get(:gateway_projection_test, &Map.fetch!(&1, :certs))
  end

  setup do
    {:ok, agent} =
      Agent.start_link(
        fn ->
          %{
            routes: {:ok, [%{host: "example.com", service: "web", revision: 1}]},
            certs: {:ok, %{}}
          }
        end,
        name: :gateway_projection_test
      )

    on_exit(fn ->
      if Process.alive?(agent), do: Agent.stop(agent)
    end)

    {:ok, pid} =
      Projection.start_link(
        name: :gateway_projection_under_test,
        readers: %{routes: Routes, certs: Certs}
      )

    %{pid: pid}
  end

  test "preserves last good snapshot when refresh fails", %{pid: pid} do
    assert {:ok, %{status: :fresh}} = Projection.refresh(pid)
    assert {:ok, fresh} = Projection.snapshot(pid)
    assert fresh.routes == [%{host: "example.com", service: "web", revision: 1}]

    Agent.update(:gateway_projection_test, fn state ->
      %{state | routes: {:error, :mnesia_unavailable}}
    end)

    assert {:error, %{reason: :mnesia_unavailable, snapshot: preserved}} = Projection.refresh(pid)
    assert preserved.freshness == :last_good
    assert preserved.routes == fresh.routes
  end
end
