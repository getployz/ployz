defmodule Ployz.CommandEndpointTest do
  use ExUnit.Case, async: false

  setup :reset_mnesia

  test "authorized operator dispatches machine add" do
    request = %{command: :machine_add, args: %{id: "node-1", command_id: "cmd-1"}}

    assert {:ok, %{status: :succeeded}} =
             Ployz.CommandEndpoint.authorize_and_dispatch(%{role: :local_operator}, request)

    assert {:ok, %{status: :active}} = Ployz.Metadata.Machines.get("node-1")
  end

  test "unauthorized command fails before receipt creation" do
    request = %{command: :machine_add, args: %{id: "node-1", command_id: "cmd-1"}}

    assert {:error, %{phase: :authorize, reason: :forbidden}} =
             Ployz.CommandEndpoint.authorize_and_dispatch(%{role: :status_reader}, request)

    assert {:error, :not_found} = Ployz.Metadata.Commands.get("cmd-1")
  end

  defp reset_mnesia(context), do: Ployz.TestSupport.Mnesia.reset!(context)
end
