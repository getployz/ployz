defmodule Ployz.Metadata.SchemaTest do
  use ExUnit.Case, async: false

  setup :reset_mnesia

  test "boot creates tables and is idempotent" do
    assert :ok = Ployz.Metadata.Schema.boot!()

    tables = :mnesia.system_info(:tables)
    for table <- Ployz.Metadata.Tables.tables(), do: assert(table in tables)
  end

  defp reset_mnesia(context), do: Ployz.TestSupport.Mnesia.reset!(context)
end
