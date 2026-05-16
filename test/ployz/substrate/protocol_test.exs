defmodule Ployz.Substrate.ProtocolTest do
  use ExUnit.Case, async: true

  alias Ployz.Substrate.Protocol

  test "encodes a versioned request frame" do
    assert {:ok, {"req-1", frame}} =
             Protocol.encode_request("docker.inspect", %{"name" => "web"}, request_id: "req-1")

    assert {:ok, decoded} = Protocol.decode_json(String.trim(frame))
    assert decoded["version"] == 1
    assert decoded["request_id"] == "req-1"
    assert decoded["op"] == "docker.inspect"
    assert decoded["params"] == %{"name" => "web"}
  end

  test "rejects unsupported response versions" do
    frame = ~s({"version":2,"request_id":"req-1","ok":{}})

    assert {:error, nil, error} = Protocol.decode_response(frame)
    assert error["code"] == "unsupported_version"
  end

  test "rejects oversized response frames" do
    frame = String.duplicate("x", Protocol.max_frame_size() + 1)

    assert {:error, nil, error} = Protocol.decode_response(frame)
    assert error["code"] == "frame_too_large"
  end

  test "redacts helper error details" do
    frame =
      ~s({"version":1,"request_id":"req-1","error":{"code":"failed","message":"failed","details":{"token":"abc","nested":{"private_key":"secret","safe":"value"}}}})

    assert {:error, "req-1", error} = Protocol.decode_response(frame)
    assert error["details"]["token"] == "[REDACTED]"
    assert error["details"]["nested"]["private_key"] == "[REDACTED]"
    assert error["details"]["nested"]["safe"] == "value"
  end
end
