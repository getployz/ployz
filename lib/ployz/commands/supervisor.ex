defmodule Ployz.Commands.Supervisor do
  @moduledoc false

  def child_spec(_opts) do
    DynamicSupervisor.child_spec(name: __MODULE__, strategy: :one_for_one)
  end

  def start_child(child_spec) do
    DynamicSupervisor.start_child(__MODULE__, child_spec)
  end
end
