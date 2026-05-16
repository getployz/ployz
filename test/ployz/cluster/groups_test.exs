defmodule Ployz.Cluster.GroupsTest do
  use ExUnit.Case, async: false

  alias Ployz.Cluster.Groups

  test "joins and leaves live runtime role groups" do
    group = {:ployz_test, System.unique_integer([:positive])}

    assert :ok = Groups.join(group, self())
    assert self() in Groups.members(group)
    assert Groups.has_live_member?(group)

    assert :ok = Groups.leave(group, self())
    refute self() in Groups.members(group)
  end
end
