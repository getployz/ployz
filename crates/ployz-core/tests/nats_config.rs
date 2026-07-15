use ployz_core::nats_config::{
    CredentialGrant, CredentialName, CredentialRole, NatsAuthorizationGrant, NatsInternalAuthority,
    NatsServerCertificatePem, NatsServerConfigError, NatsUserPublicKey, NatsUserSeed,
};

const CA_PEM: &str = "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n";

#[test]
fn authorized_user_record_keys_include_user_public_key() {
    let first = generated_user_public_key();
    let second = generated_user_public_key();
    let first_user = credential(first.clone(), "Nick's laptop");
    let second_user = credential(second.clone(), "Ployz Cloud");
    let controller = NatsAuthorizationGrant::Internal {
        authority: NatsInternalAuthority::Controller,
        public_key: generated_user_public_key(),
    };

    assert_eq!(
        first_user.authority_record_key(),
        format!("user.{}", first.as_str())
    );
    assert_eq!(
        second_user.authority_record_key(),
        format!("user.{}", second.as_str())
    );
    assert_ne!(
        first_user.authority_record_key(),
        second_user.authority_record_key()
    );
    assert_eq!(controller.authority_record_key(), "controller");
}

#[test]
fn credential_name_trims_unicode_text_and_rejects_blank_text() {
    let name = CredentialName::try_new("  Ník's laptop  ").expect("valid credential name");

    assert_eq!(name.as_str(), "Ník's laptop");
    assert!(CredentialName::try_new(" \n\t ").is_err());
    assert!(CredentialName::try_new("Ployz\nCloud").is_err());
}

#[test]
fn nats_user_key_material_is_validated_and_seed_debug_is_redacted() {
    let public = "UBCXCMGAZQZN55X5TTTWMB5CZNZIKJHEDZJOJ3TV63NKPJ6FRXSR2ZO4";
    let seed = "SUACH75SWCM5D2JMJM6EKLR2WDARVGZT4QC6LX3AGHSWOMVAKERABBBRWM";

    assert!(NatsUserPublicKey::try_new(public).is_ok());
    assert!(NatsUserPublicKey::try_new("UABC").is_err());
    assert!(NatsUserPublicKey::try_new(format!("X{}", "A".repeat(55))).is_err());
    assert!(NatsUserSeed::try_new(seed).is_ok());
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

fn generated_user_public_key() -> NatsUserPublicKey {
    NatsUserPublicKey::try_new(nkeys::KeyPair::new_user().public_key())
        .expect("generated user public key is valid")
}

fn credential(public_key: NatsUserPublicKey, name: &str) -> NatsAuthorizationGrant {
    NatsAuthorizationGrant::Credential(CredentialGrant {
        public_key,
        name: CredentialName::try_new(name).expect("credential name"),
        role: CredentialRole::Operator,
    })
}
