fn main() -> Result<(), ployz_gateway::GatewayError> {
    tracing_subscriber::fmt::init();
    ployz_gateway::run_gateway_process()
}
