fn main() -> Result<(), ployz_gateway::GatewayError> {
    ployzd::run_gateway_process_from_env()
}
