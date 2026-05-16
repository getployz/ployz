defmodule Ployz.ManifestTest do
  use ExUnit.Case, async: true

  alias Ployz.Manifest

  test "parses the native YAML-like MVP manifest" do
    manifest = """
    service: web
    image: ghcr.io/example/web:sha
    instances: 2
    env:
      DATABASE_URL: ployz-secret://prod/database-url
    routes:
      - host: example.com
        path: /
        port: 4000
    volumes:
      - name: data
        mount: /data
    """

    assert {:ok, parsed} = Manifest.parse_string(manifest)
    assert parsed.service == "web"
    assert parsed.image == "ghcr.io/example/web:sha"
    assert parsed.instances == 2
    assert parsed.env == %{"DATABASE_URL" => "ployz-secret://prod/database-url"}
    assert parsed.routes == [%{host: "example.com", path: "/", port: 4000}]
    assert parsed.volumes == [%{name: "data", mount: "/data"}]
  end

  test "rejects env values that are not refs" do
    manifest = """
    service: web
    image: web:latest
    env:
      DATABASE_URL: postgres://user:pass@example/db
    """

    assert {:error, {:manifest_invalid, "env.DATABASE_URL", :env_must_be_secret_reference}} =
             Manifest.parse_string(manifest)
  end

  test "rejects duplicate routes" do
    manifest = """
    service: web
    image: web:latest
    routes:
      - host: example.com
        path: /
        port: 4000
      - host: example.com
        path: /
        port: 5000
    """

    assert {:error, {:manifest_invalid, "routes", {:duplicate_route, "example.com", "/"}}} =
             Manifest.parse_string(manifest)
  end

  test "rejects raw PEM-looking material anywhere in the manifest" do
    manifest = """
    service: web
    image: web:latest
    metadata:
      cert: -----BEGIN PRIVATE KEY-----
    """

    assert {:error, {:manifest_invalid, :secret_material, :not_allowed}} =
             Manifest.parse_string(manifest)
  end
end
