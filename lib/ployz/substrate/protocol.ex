defmodule Ployz.Substrate.Protocol do
  @moduledoc """
  JSON-lines protocol for the Rust substrate helper.

  The BEAM side owns request framing, request IDs, response validation, and
  redaction before errors are shown to operators or persisted in receipts.
  """

  @version 1
  @max_frame_size 1_048_576

  @redacted "[REDACTED]"
  @secret_key_fragments ~w[
    account auth authorization bearer cert cookie credential key pass password pem private secret token
  ]

  @type request_id :: binary()
  @type operation :: binary() | atom()
  @type json_value ::
          nil
          | boolean()
          | number()
          | binary()
          | [json_value()]
          | %{optional(binary()) => json_value()}

  @type protocol_error :: %{
          required(binary()) => binary() | map()
        }

  @doc "Returns the current helper protocol version."
  @spec version() :: pos_integer()
  def version, do: @version

  @doc "Returns the largest accepted JSON-lines frame, in bytes."
  @spec max_frame_size() :: pos_integer()
  def max_frame_size, do: @max_frame_size

  @doc """
  Encodes a helper request as a single JSON line.

  A request always includes `version`, `request_id`, `op`, and `params`.
  """
  @spec encode_request(operation(), map(), keyword()) ::
          {:ok, {request_id(), binary()}} | {:error, protocol_error()}
  def encode_request(op, params, opts \\ [])

  def encode_request(op, params, opts) when is_map(params) do
    request_id = Keyword.get_lazy(opts, :request_id, &new_request_id/0)
    version = Keyword.get(opts, :version, @version)

    with :ok <- validate_version(version),
         {:ok, op} <- normalize_operation(op),
         :ok <- validate_request_id(request_id) do
      request = %{
        "version" => version,
        "request_id" => request_id,
        "op" => op,
        "params" => params
      }

      encoded = encode_json(request) <> "\n"

      if byte_size(encoded) <= @max_frame_size do
        {:ok, {request_id, encoded}}
      else
        {:error,
         error("frame_too_large", "helper request frame exceeds limit", %{
           max_bytes: @max_frame_size
         })}
      end
    end
  end

  def encode_request(_op, _params, _opts),
    do: {:error, error("invalid_request", "helper request params must be a map", %{})}

  @doc """
  Decodes and validates one helper response line.

  Responses must include the current `version`, a valid `request_id`, and exactly
  one of `ok` or `error`.
  """
  @spec decode_response(binary(), request_id() | nil) ::
          {:ok, request_id(), json_value()} | {:error, request_id() | nil, protocol_error()}
  def decode_response(line, expected_request_id \\ nil)

  def decode_response(line, expected_request_id) when is_binary(line) do
    with :ok <- validate_frame_size(line),
         {:ok, decoded} <- decode_json(String.trim_trailing(line, "\n")),
         {:ok, response} <- validate_response_map(decoded),
         {:ok, version} <- fetch_version(response),
         :ok <- validate_version(version),
         {:ok, request_id} <- fetch_request_id(response),
         :ok <- validate_expected_request_id(request_id, expected_request_id) do
      cond do
        Map.has_key?(response, "ok") and Map.has_key?(response, "error") ->
          {:error, request_id,
           error("invalid_response", "helper response cannot contain both ok and error", %{})}

        Map.has_key?(response, "ok") ->
          {:ok, request_id, redact(response["ok"])}

        Map.has_key?(response, "error") ->
          {:error, request_id, normalize_helper_error(response["error"])}

        true ->
          {:error, request_id,
           error("invalid_response", "helper response must contain ok or error", %{})}
      end
    else
      {:error, %{} = err} -> {:error, nil, err}
      {:error, request_id, %{} = err} -> {:error, request_id, err}
    end
  end

  def decode_response(_line, _expected_request_id),
    do: {:error, nil, error("invalid_frame", "helper response frame must be binary", %{})}

  @doc "Redacts secret-like keys recursively from maps and lists."
  @spec redact(json_value() | map()) :: json_value() | map()
  def redact(value) when is_map(value) do
    Map.new(value, fn {key, nested} ->
      key_string = to_string(key)

      if secret_key?(key_string) do
        {key_string, @redacted}
      else
        {key_string, redact(nested)}
      end
    end)
  end

  def redact(value) when is_list(value), do: Enum.map(value, &redact/1)
  def redact(value), do: value

  @doc "Creates an operator-safe structured protocol error."
  @spec error(binary(), binary(), map()) :: protocol_error()
  def error(code, message, details)
      when is_binary(code) and is_binary(message) and is_map(details) do
    %{"code" => code, "message" => message, "details" => redact(details)}
  end

  @doc false
  @spec encode_json(json_value() | map()) :: binary()
  def encode_json(value), do: JSON.encode!(value)

  @doc false
  @spec decode_json(binary()) :: {:ok, json_value()} | {:error, protocol_error()}
  def decode_json(json) when is_binary(json) do
    case JSON.decode(json) do
      {:ok, value} -> {:ok, value}
      {:error, reason} -> {:error, error("invalid_json", inspect(reason), %{})}
    end
  end

  defp validate_response_map(response) when is_map(response), do: {:ok, response}

  defp validate_response_map(_response),
    do: {:error, error("invalid_response", "helper response must be a JSON object", %{})}

  defp validate_frame_size(line) do
    if byte_size(line) <= @max_frame_size do
      :ok
    else
      {:error,
       error("frame_too_large", "helper response frame exceeds limit", %{
         max_bytes: @max_frame_size
       })}
    end
  end

  defp validate_version(@version), do: :ok

  defp validate_version(version) when is_integer(version) do
    {:error,
     error("unsupported_version", "unsupported helper protocol version", %{version: version})}
  end

  defp validate_version(_version),
    do: {:error, error("invalid_version", "helper protocol version must be an integer", %{})}

  defp fetch_version(%{"version" => version}), do: {:ok, version}

  defp fetch_version(_response),
    do: {:error, error("invalid_version", "helper response is missing version", %{})}

  defp normalize_operation(op) when is_atom(op), do: normalize_operation(Atom.to_string(op))

  defp normalize_operation(op) when is_binary(op) do
    if Regex.match?(~r/\A[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+\z/, op) do
      {:ok, op}
    else
      {:error, error("invalid_operation", "helper operation name is invalid", %{op: op})}
    end
  end

  defp normalize_operation(_op),
    do: {:error, error("invalid_operation", "helper operation must be an atom or string", %{})}

  defp fetch_request_id(%{"request_id" => request_id}) do
    with :ok <- validate_request_id(request_id), do: {:ok, request_id}
  end

  defp fetch_request_id(_response),
    do: {:error, error("invalid_request_id", "helper response is missing request_id", %{})}

  defp validate_request_id(request_id) when is_binary(request_id) do
    if Regex.match?(~r/\A[A-Za-z0-9._:-]{1,128}\z/, request_id) do
      :ok
    else
      {:error, error("invalid_request_id", "helper request_id contains invalid characters", %{})}
    end
  end

  defp validate_request_id(_request_id),
    do: {:error, error("invalid_request_id", "helper request_id must be a string", %{})}

  defp validate_expected_request_id(_request_id, nil), do: :ok
  defp validate_expected_request_id(request_id, request_id), do: :ok

  defp validate_expected_request_id(request_id, expected_request_id) do
    {:error, request_id,
     error("unexpected_response", "helper response request_id does not match pending request", %{
       expected_request_id: expected_request_id,
       request_id: request_id
     })}
  end

  defp normalize_helper_error(error_payload) when is_map(error_payload) do
    code = Map.get(error_payload, "code", "helper_error")
    message = Map.get(error_payload, "message", "helper operation failed")
    details = Map.get(error_payload, "details", %{})

    error(
      to_string(code),
      to_string(message),
      if(is_map(details), do: details, else: %{"value" => details})
    )
  end

  defp normalize_helper_error(error_payload) do
    error("helper_error", "helper operation failed", %{"error" => error_payload})
  end

  defp new_request_id do
    unique = System.unique_integer([:positive, :monotonic])
    "req-" <> Integer.to_string(unique, 36)
  end

  defp secret_key?(key) do
    normalized = String.downcase(key)
    Enum.any?(@secret_key_fragments, &String.contains?(normalized, &1))
  end
end
