use crate::control::intent::ingress_intent::IngressIntentStore;
use crate::control::store::CoreStore;
use ployz_core::ingress::{
    AutomaticHostnameConfiguration, IngressConfiguration, PloyzDnsTargetIntent,
};

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
