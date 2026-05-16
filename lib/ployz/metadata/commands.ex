defmodule Ployz.Metadata.Commands do
  @moduledoc """
  Command receipts are durable evidence before external work starts.
  """

  alias Ployz.Metadata.Tables
  alias Ployz.Redactor

  def create(%{id: command_id} = receipt) do
    Tables.write(:commands, command_id, Redactor.redact(receipt))
  end

  def finish(%{id: command_id} = receipt) do
    Tables.write(:commands, command_id, Redactor.redact(receipt))
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
      {:ok, %{receipt | status: :committed, completed_at: now(), result: result, last_error: nil}}
    end)
  end

  def fail(command_id, reason) do
    update(command_id, fn receipt ->
      {:ok,
       %{
         receipt
         | status: :failed,
           completed_at: now(),
           last_error: Redactor.redact(error_reason(reason))
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

  defp error_reason(reason) when is_atom(reason), do: %{code: reason}
  defp error_reason(%{} = reason), do: reason
  defp error_reason(reason), do: %{code: :command_failed, detail: inspect(reason)}

  defp now, do: System.system_time(:millisecond)
end
