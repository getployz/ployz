defmodule Ployz.Runtime.ServerTest do
  use ExUnit.Case, async: true

  alias Ployz.Runtime.Server

  defmodule FakePort do
    use GenServer

    def start_link(responses), do: GenServer.start_link(__MODULE__, responses)
    def init(responses), do: {:ok, responses}

    def handle_call({:request, op, params}, _from, responses) do
      {:reply, {:ok, %{"op" => op, "params" => params}}, responses}
    end
  end

  defmodule ScriptedPort do
    use GenServer

    def start_link(script), do: GenServer.start_link(__MODULE__, script)
    def init(script), do: {:ok, script}

    def handle_call({:request, op, params}, _from, %{owner: owner, replies: replies}) do
      send(owner, {:port_call, self(), op, params})

      case replies do
        [{^op, reply} | rest] ->
          {:reply, reply, %{owner: owner, replies: rest}}

        [{expected, reply} | rest] ->
          send(owner, {:unexpected_port_call, expected, op})
          {:reply, reply, %{owner: owner, replies: rest}}

        [] ->
          {:reply, {:error, %{"code" => "unexpected_call", "op" => op}},
           %{owner: owner, replies: []}}
      end
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

  test "cleans up already started containers when a later runtime fails" do
    owner = self()

    {:ok, first_port} =
      ScriptedPort.start_link(%{
        owner: owner,
        replies: [
          {"docker.start", {:ok, %{"started" => true}}},
          {"docker.inspect", {:ok, %{"running" => true}}},
          {"docker.stop", {:ok, %{"stopped" => true}}}
        ]
      })

    {:ok, second_port} =
      ScriptedPort.start_link(%{
        owner: owner,
        replies: [
          {"docker.start", {:error, %{"code" => "start_failed"}}}
        ]
      })

    {:ok, first} = Server.start_link(port: first_port, group: :ployz_runtime_cleanup_test)
    {:ok, second} = Server.start_link(port: second_port, group: :ployz_runtime_cleanup_test)

    manifest = %Ployz.Manifest{service: "web", image: "busybox", instances: 2}

    assert {:error, %{"code" => "start_failed"}} =
             Server.start_and_probe([first, second], manifest, 1_000)

    assert_received {:port_call, ^first_port, "docker.start", _params}
    assert_received {:port_call, ^first_port, "docker.inspect", _params}
    assert_received {:port_call, ^second_port, "docker.start", _params}
    assert_received {:port_call, ^first_port, "docker.stop", %{"name" => _container}}
    refute_received {:unexpected_port_call, _expected, _actual}
  end
end
