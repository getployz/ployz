use std::path::PathBuf;

use ployz_core::nats_config::{
    NatsAdvertisedHost, NatsAuthorizedUser, NatsListener, NatsServerCertificatePem,
    NatsServerConfig, NatsServerConfigError, NatsServerTlsFiles, NatsUserPublicKey, NatsUserSeed,
    render_authorized_users,
};
use ployz_core::security::NatsPrincipal;
use ployz_test_support::ids::machine_id;

const CA_PEM: &str = "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n";

#[test]
fn single_machine_nats_config_renders_loopback_tls_jetstream_and_include() {
    let config = loopback_config(PathBuf::from("/var/lib/ployz/nats"));

    assert_eq!(config.client_host(), "127.0.0.1");
    assert_eq!(config.port(), 4222);
    assert_eq!(
        config.render(),
        "server_name: core_1\n\
         host: 127.0.0.1\n\
         port: 4222\n\
         tls {\n  cert_file: \"/var/lib/ployz/nats/server.crt\"\n  key_file: \"/var/lib/ployz/nats/server.key\"\n}\n\
         jetstream {\n  store_dir: \"/var/lib/ployz/nats\"\n}\n\
         include \"authorized-users.conf\"\n"
    );
}

#[test]
fn external_listener_binds_all_interfaces_and_advertises_the_public_host() {
    let config = NatsServerConfig::single_machine(
        machine_id("core_1"),
        PathBuf::from("/var/lib/ployz/nats"),
        NatsListener::External {
            advertise_host: NatsAdvertisedHost::try_new("203.0.113.10")
                .expect("valid advertised host"),
        },
        tls_files(),
        PathBuf::from("authorized-users.conf"),
    )
    .expect("external nats config is valid");

    assert_eq!(config.client_host(), "203.0.113.10");
    let rendered = config.render();
    assert!(rendered.contains("host: 0.0.0.0\n"));
    assert!(rendered.contains("client_advertise: 203.0.113.10:4222\n"));
    assert!(rendered.contains("tls {\n"));
}

#[test]
fn advertised_host_accepts_hostname_ipv4_and_bracketed_ipv6_only() {
    assert!(NatsAdvertisedHost::try_new("core.example.test").is_ok());
    assert!(NatsAdvertisedHost::try_new("203.0.113.10").is_ok());
    assert!(NatsAdvertisedHost::try_new("[2001:db8::1]").is_ok());
    assert!(NatsAdvertisedHost::try_new("").is_err());
    assert!(NatsAdvertisedHost::try_new("under_score").is_err());
    assert!(NatsAdvertisedHost::try_new("-leading.example").is_err());
    assert!(NatsAdvertisedHost::try_new("[not-an-ipv6]").is_err());
}

#[test]
fn single_machine_nats_config_escapes_store_dir() {
    let config = loopback_config(PathBuf::from("/var/lib/ployz/nats \"quoted\" \\ path"));

    assert!(
        config
            .render()
            .contains("store_dir: \"/var/lib/ployz/nats \\\"quoted\\\" \\\\ path\"")
    );
}

#[test]
fn single_machine_nats_config_requires_absolute_store_dir() {
    assert_eq!(
        NatsServerConfig::single_machine(
            machine_id("core_1"),
            PathBuf::from("relative/nats"),
            NatsListener::Loopback,
            tls_files(),
            PathBuf::from("authorized-users.conf"),
        ),
        Err(NatsServerConfigError::InvalidPath {
            field: "jetstream_store_dir",
            value: PathBuf::from("relative/nats"),
        })
    );
}

#[test]
fn single_machine_nats_config_requires_absolute_tls_paths() {
    assert_eq!(
        NatsServerConfig::single_machine(
            machine_id("core_1"),
            PathBuf::from("/var/lib/ployz/nats"),
            NatsListener::Loopback,
            NatsServerTlsFiles {
                cert_file: PathBuf::from("relative/server.crt"),
                key_file: PathBuf::from("/var/lib/ployz/nats/server.key"),
            },
            PathBuf::from("authorized-users.conf"),
        ),
        Err(NatsServerConfigError::InvalidPath {
            field: "tls.cert_file",
            value: PathBuf::from("relative/server.crt"),
        })
    );
}

#[test]
fn authorized_users_render_one_block_per_principal_with_own_inbox() {
    let rendered = render_authorized_users(&[
        NatsAuthorizedUser {
            principal: NatsPrincipal::Controller,
            nkey_public: user_public_key('A'),
        },
        NatsAuthorizedUser {
            principal: NatsPrincipal::Join,
            nkey_public: user_public_key('B'),
        },
    ]);

    assert!(rendered.starts_with("authorization {\n  users [\n"));
    assert!(rendered.ends_with("  ]\n}\n"));
    assert!(rendered.contains(&format!("nkey: {}", user_public_key('A').as_str())));
    assert!(rendered.contains(&format!("nkey: {}", user_public_key('B').as_str())));
    assert!(rendered.contains("\"_INBOX_ctl.>\""));
    assert!(rendered.contains("\"_INBOX_join.>\""));
    assert!(!rendered.contains("\"_INBOX.>\""));
    assert!(rendered.contains("\"plz.v1.svc.api.machine.join.redeem\""));
    assert!(rendered.contains("allow_responses: true"));
}

#[test]
fn authorized_machine_user_denies_core_kv_writes() {
    let rendered = render_authorized_users(&[NatsAuthorizedUser {
        principal: NatsPrincipal::Machine {
            machine_id: machine_id("machine_7"),
        },
        nkey_public: user_public_key('C'),
    }]);

    assert!(rendered.contains("deny: [\"$KV.KV_CORE.>\"]"));
    assert!(rendered.contains("\"_INBOX_machine_machine_7.>\""));
    assert!(rendered.contains("\"$JS.API.DIRECT.GET.KV_KV_CORE.>\""));
}

#[test]
fn nats_user_key_material_is_validated_and_seed_debug_is_redacted() {
    let public = format!("U{}", "A".repeat(55));
    let seed = format!("SU{}", "A".repeat(56));

    assert!(NatsUserPublicKey::try_new(public.as_str()).is_ok());
    assert!(NatsUserPublicKey::try_new("UABC").is_err());
    assert!(NatsUserPublicKey::try_new(format!("X{}", "A".repeat(55))).is_err());
    assert!(NatsUserSeed::try_new(seed.as_str()).is_ok());
    assert!(NatsUserSeed::try_new("SUABC").is_err());
    assert!(NatsUserSeed::try_new(format!("UA{}", "A".repeat(56))).is_err());

    let seed = NatsUserSeed::try_new(seed).expect("valid seed");
    let debug = format!("{seed:?}");
    assert!(!debug.contains("SUA"));
    assert!(debug.contains("redacted"));
}

#[test]
fn server_certificate_pem_requires_a_certificate_block() {
    assert_eq!(
        NatsServerCertificatePem::try_new(""),
        Err(NatsServerConfigError::InvalidServerCertificatePem)
    );
    assert_eq!(
        NatsServerCertificatePem::try_new("not-a-pem"),
        Err(NatsServerConfigError::InvalidServerCertificatePem)
    );
    let pem = NatsServerCertificatePem::try_new(CA_PEM).expect("valid certificate pem");
    assert_eq!(pem.as_str(), CA_PEM);
}

fn loopback_config(store_dir: PathBuf) -> NatsServerConfig {
    NatsServerConfig::single_machine(
        machine_id("core_1"),
        store_dir,
        NatsListener::Loopback,
        tls_files(),
        PathBuf::from("authorized-users.conf"),
    )
    .expect("single-machine nats config is valid")
}

fn tls_files() -> NatsServerTlsFiles {
    NatsServerTlsFiles {
        cert_file: PathBuf::from("/var/lib/ployz/nats/server.crt"),
        key_file: PathBuf::from("/var/lib/ployz/nats/server.key"),
    }
}

fn user_public_key(_fill: char) -> NatsUserPublicKey {
    NatsUserPublicKey::try_new("UBCXCMGAZQZN55X5TTTWMB5CZNZIKJHEDZJOJ3TV63NKPJ6FRXSR2ZO4")
        .expect("valid user public key")
}
