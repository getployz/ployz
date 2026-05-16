import Config

if dir = System.get_env("PLOYZ_MNESIA_DIR") do
  config :ployz, metadata_dir: dir
end

if token = System.get_env("PLOYZ_LOCAL_OPERATOR_TOKEN") do
  config :ployz, auth_tokens: [token]
end
