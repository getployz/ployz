defmodule Mix.Tasks.PloyzTest do
  use ExUnit.Case, async: true

  alias Mix.Tasks.Ployz

  test "returns structured usage for unknown commands" do
    assert {:error, {:usage, usage}, nil} = Ployz.dispatch(["wat"])
    assert usage =~ "mix ployz deploy"
  end
end
