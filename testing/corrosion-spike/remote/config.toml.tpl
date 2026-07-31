[db]
path = @@DB_PATH@@
schema_paths = [@@SCHEMA_PATH@@]
subscriptions_path = @@SUBSCRIPTIONS_PATH@@

[gossip]
addr = @@GOSSIP_ADDR@@
bootstrap = @@BOOTSTRAP_PEERS@@
plaintext = true
max_mtu = 1232

[api]
addr = @@API_ADDR@@
authz.bearer-token = @@BEARER_TOKEN@@

[admin]
path = @@ADMIN_SOCKET@@

@@TEST_REAPER_CONFIG@@
