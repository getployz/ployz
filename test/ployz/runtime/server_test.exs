defmodule Ployz.Runtime.ServerTest do
  use ExUnit.Case, async: true

  alias Ployz.Runtime.Server

  defmodule FakePort do
    use GenServer

    def start_link(responses), do: GenServer.start_link(__MODULE__, responses)
    def init(responses), do: {:ok, responses}

    def handle_call({op, params}, _from, responses) do
      {:reply, {:ok, %{"op" => op, "params" => params}}, responses}
    end
  end

  setup do
    case Process.whereis(:pg) do
      nil -> {:ok, _pid} = :pg.start_link()
      _pid -> :ok
    end

    :ok
  end

  test "joins the runtime pg group when started" do
    {:ok, fake_port} = FakePort.start_link(%{})
    {:ok, runtime} = Server.start_link(port: fake_port, group: :ployz_runtime_test)

    assert runtime in :pg.get_members(:ployz_runtime_test)
  end
end
