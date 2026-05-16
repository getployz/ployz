defmodule Ployz.CommandEndpointTest do
  use ExUnit.Case, async: false

  setup :reset_mnesia

  test "authorized operator dispatches machine add" do
    request = %{command: :machine_add, args: %{id: "node-1", command_id: "cmd-1"}}

    assert {:ok, %{receipt: %{status: :committed}}} =
             Ployz.CommandEndpoint.authorize_and_dispatch(%{role: :local_operator}, request)

    assert {:ok, %{status: :active}} = Ployz.Metadata.Machines.get("node-1")
  end

  test "unauthorized privileged commands fail before receipt creation" do
    requests = [
      %{command: :machine_add, args: %{id: "node-1", command_id: "cmd-add"}},
      %{command: :machine_remove, args: %{id: "node-1", command_id: "cmd-remove"}},
      %{command: :deploy, args: %{manifest_path: "/missing"}},
      %{command: :cert_issue, args: %{hostname: "example.com"}},
      %{command: :migrate_volume, args: %{volume: "pgdata", destination: "node-2"}}
    ]

    for request <- requests do
      assert {:error, %{phase: :authorize, reason: :forbidden, receipt: nil}} =
               Ployz.CommandEndpoint.authorize_and_dispatch(%{role: :status_reader}, request)
    end

    assert {:error, :not_found} = Ployz.Metadata.Commands.get("cmd-add")
    assert {:error, :not_found} = Ployz.Metadata.Commands.get("cmd-remove")
  end

  test "status reader can read gateway routes and status" do
    assert {:ok, %{result: []}} =
             Ployz.CommandEndpoint.authorize_and_dispatch(%{role: :status_reader}, %{
               command: :gateway_routes,
               args: %{}
             })

    assert {:ok, %{result: %{freshness: :local}}} =
             Ployz.CommandEndpoint.authorize_and_dispatch(%{role: :status_reader}, %{
               command: :status,
               args: %{}
             })
  end

  defp reset_mnesia(context), do: Ployz.TestSupport.Mnesia.reset!(context)
end
