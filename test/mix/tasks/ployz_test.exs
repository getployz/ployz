defmodule Mix.Tasks.PloyzTest do
  use ExUnit.Case, async: false

  alias Mix.Tasks.Ployz

  test "returns structured usage for unknown commands" do
    assert {:error, {:usage, usage}, nil} = Ployz.dispatch(["wat"])
    assert usage =~ "mix ployz deploy"
  end

  test "privileged commands fail closed when the endpoint contract is missing" do
    previous = Application.get_env(:ployz, :command_endpoint)
    Application.put_env(:ployz, :command_endpoint, __MODULE__.BadEndpoint)

    on_exit(fn ->
      if previous do
        Application.put_env(:ployz, :command_endpoint, previous)
      else
        Application.delete_env(:ployz, :command_endpoint)
      end
    end)

    assert {:error,
            %{phase: :dispatch, reason: {:invalid_command_endpoint, __MODULE__.BadEndpoint}}} =
             Ployz.dispatch(["machine", "add", "node-a"])
  end

  test "run raises on dispatch errors" do
    assert_raise Mix.Error, fn -> Ployz.run(["wat"]) end
  end

  defmodule BadEndpoint do
  end
end
