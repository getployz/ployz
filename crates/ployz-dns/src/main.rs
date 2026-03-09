fn main() -> Result<(), ployz_dns::DnsError> {
    tracing_subscriber::fmt::init();
    ployz_dns::run_dns_process()
}
