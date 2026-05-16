defmodule Ployz.Application do
  @moduledoc false

  use Application

  @impl true
  def start(_type, _args) do
    Ployz.Supervisor.start_link(name: Ployz.Supervisor)
  end
end
