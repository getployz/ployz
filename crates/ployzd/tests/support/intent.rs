use ployz_core::ingress::{
    AutomaticHostnameConfiguration, IngressConfiguration, PloyzDnsTargetIntent,
};
use ployzd::control::intent::ingress_intent::IngressIntentStore;
use ployzd::control::store::CoreStore;

pub async fn initialize_disabled_ingress(store: &CoreStore) {
    IngressIntentStore::new(store.clone())
        .replace(
            IngressConfiguration::try_new(
                AutomaticHostnameConfiguration::Disabled,
                PloyzDnsTargetIntent::Disabled,
            )
            .expect("valid disabled ingress configuration"),
        )
        .await
        .expect("disabled ingress intent initializes");
}
