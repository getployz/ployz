import Config

if dir = System.get_env("PLOYZ_MNESIA_DIR") do
  config :ployz, metadata_dir: dir
end

if token = System.get_env("PLOYZ_LOCAL_OPERATOR_TOKEN") do
  config :ployz, auth_mode: :token, auth_tokens: [token]
end

if mode = System.get_env("PLOYZ_DISTRIBUTION_MODE") do
  config :ployz, distribution_mode: String.to_atom(mode)
end

if mode = System.get_env("PLOYZ_METADATA_STORAGE") do
  config :ployz, metadata_storage: String.to_atom(mode)
end

if helper = System.get_env("PLOYZ_SUBSTRATE_HELPER") do
  config :ployz, substrate_helper_path: helper
end
