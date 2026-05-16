defmodule Ployz.Metadata.Commands do
  @moduledoc """
  Command receipts are durable evidence before external work starts.
  """

  alias Ployz.Metadata.Tables

  def create(%{id: command_id} = receipt) do
    Tables.write(:commands, command_id, redact(receipt))
  end

  def finish(%{id: command_id} = receipt) do
    Tables.write(:commands, command_id, redact(receipt))
  end

  def create_running(command_id, kind, actor, opts \\ []) do
    receipt = %{
      id: command_id,
      kind: kind,
      actor: actor,
      owner: Keyword.get(opts, :owner, node()),
      lease_token: Keyword.get(opts, :lease_token),
      phase: Keyword.get(opts, :phase, :accepted),
      status: :running,
      started_at: now(),
      completed_at: nil,
      last_error: nil,
      result: nil
    }

    Tables.write(:commands, command_id, receipt)
  end

  def mark_phase(command_id, phase) do
    update(command_id, fn receipt -> {:ok, %{receipt | phase: phase}} end)
  end

  def succeed(command_id, result \\ %{}) do
    update(command_id, fn receipt ->
      {:ok, %{receipt | status: :succeeded, completed_at: now(), result: result, last_error: nil}}
    end)
  end

  def fail(command_id, reason) do
    update(command_id, fn receipt ->
      {:ok,
       %{
         receipt
         | status: :failed,
           completed_at: now(),
           last_error: redact(reason)
       }}
    end)
  end

  def get(command_id), do: Tables.read(:commands, command_id)

  defp update(command_id, fun) do
    with {:ok, receipt} <- Tables.read(:commands, command_id),
         {:ok, next} <- fun.(receipt) do
      Tables.write(:commands, command_id, next)
    end
  end

  defp redact(reason) when is_atom(reason), do: %{code: reason}

  defp redact(%{} = value) do
    value
    |> Map.drop([:secret, :private_key, :pem, "secret", "private_key", "pem"])
    |> Map.new(fn {key, nested} -> {key, redact_nested(nested)} end)
  end

  defp redact(reason), do: %{code: :command_failed, detail: inspect(reason)}

  defp redact_nested(%{} = value), do: redact(value)
  defp redact_nested(values) when is_list(values), do: Enum.map(values, &redact_nested/1)
  defp redact_nested(value), do: value

  defp now, do: System.system_time(:millisecond)
end
