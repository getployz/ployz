use ployz_core::ingress::{
    AutomaticHostnameConfiguration, IngressConfiguration, PloyzDnsTargetIntent,
};
use ployzd::core_store::CoreStore;
use ployzd::intent::ingress_intent::IngressIntentStore;

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
