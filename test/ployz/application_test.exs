defmodule Ployz.ApplicationTest do
  use ExUnit.Case, async: false

  setup :reset_mnesia

  test "application supervisor starts with metadata and command supervision" do
    suffix = System.unique_integer([:positive])

    {:ok, pid} =
      Ployz.Supervisor.start_link(
        name: :"ployz-test-supervisor-#{suffix}",
        command_supervisor_name: :"ployz-test-commands-#{suffix}",
        gateway_name: :"ployz-test-gateway-#{suffix}"
      )

    assert Process.alive?(pid)

    assert {:ok, _} =
             Ployz.Metadata.Tables.transaction(fn -> :mnesia.table_info(:machines, :size) end)
  end

  defp reset_mnesia(context), do: Ployz.TestSupport.Mnesia.reset!(context)
end
