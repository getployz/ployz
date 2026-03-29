fn main() -> Result<(), ployz_dns::DnsError> {
    ployzd::run_dns_process_from_env()
}
