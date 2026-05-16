defmodule Ployz.Runtime.StaleTest do
  use ExUnit.Case, async: true

  alias Ployz.Runtime.Stale

  test "classifies current service resources" do
    committed = %{service_heads: %{"web" => "rev-2"}, machines: %{"node-a" => "active"}}
    resource = %{kind: "container", service: "web", revision: "rev-2", machine: "node-a"}

    assert [%{classification: :current}] = Stale.classify(resource, committed)
  end

  test "classifies old service revisions as stale" do
    committed = %{service_heads: %{"web" => "rev-2"}, machines: %{"node-a" => "active"}}
    resource = %{kind: "container", service: "web", revision: "rev-1", machine: "node-a"}

    assert [%{classification: :stale}] = Stale.classify(resource, committed)
  end

  test "classifies removed machine resources separately" do
    committed = %{service_heads: %{"web" => "rev-2"}, machines: %{"node-a" => "removed"}}
    resource = %{kind: "container", service: "web", revision: "rev-2", machine: "node-a"}

    assert [%{classification: :removed_machine}] = Stale.classify(resource, committed)
  end

  test "classifies malformed observations as unknown" do
    committed = %{service_heads: %{"web" => "rev-2"}}

    assert [%{classification: :unknown}] = Stale.classify(%{"service" => "web"}, committed)
  end
end
