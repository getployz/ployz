use std::{convert::Infallible, future::Ready};

use tonic::{
    body::Body,
    codegen::{Service, http},
    server::NamedService,
};

#[derive(Clone, Default)]
pub struct EchoService {
    pub identity: Option<http::HeaderValue>,
}

impl Service<http::Request<Body>> for EchoService {
    type Response = http::Response<Body>;
    type Error = Infallible;
    type Future = Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<Body>) -> Self::Future {
        let marks = [
            ployz_core::CLOUD_ROUTED_HEADER,
            ployz_core::OPERATOR_SCOPE_HEADER,
        ]
        .map(|name| (name, request.headers().get(name).cloned()));
        let mut response = http::Response::new(request.into_body());
        for (name, value) in marks {
            if let Some(value) = value {
                response.headers_mut().insert(name, value);
            }
        }
        response.headers_mut().insert(
            "content-type",
            http::HeaderValue::from_static("application/grpc"),
        );
        if let Some(identity) = &self.identity {
            response
                .headers_mut()
                .insert("test-machine-id", identity.clone());
        }
        std::future::ready(Ok(response))
    }
}

impl NamedService for EchoService {
    const NAME: &'static str = "ployz.rpc.v1.MachineRpc";
}
