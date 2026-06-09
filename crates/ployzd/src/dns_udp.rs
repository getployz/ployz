//! Tiny UDP DNS wire helpers for the DNS role.

use crate::dns::{DnsAnswer, DnsAnswerTable};
use ployz_core::ops::RouteHostname;

const DNS_TTL_SECONDS: u32 = 30;

#[must_use]
pub fn dns_response_from_query(table: &DnsAnswerTable, query: &[u8]) -> Option<Vec<u8>> {
    let question = parse_dns_question(query)?;
    let id = query.get(0..2)?;
    let question_wire = query.get(12..question.end_offset)?;
    let mut response = Vec::with_capacity(512);
    response.extend_from_slice(id);
    response.extend_from_slice(&0x8180_u16.to_be_bytes());
    response.extend_from_slice(&1_u16.to_be_bytes());
    let answers = dns_answers_for_question(table, &question);
    response.extend_from_slice(&(answers.len() as u16).to_be_bytes());
    response.extend_from_slice(&0_u16.to_be_bytes());
    response.extend_from_slice(&0_u16.to_be_bytes());
    response.extend_from_slice(question_wire);

    for answer in answers {
        response.extend_from_slice(&[0xc0, 0x0c]);
        match answer {
            DnsAnswer::Ipv4(address) => {
                response.extend_from_slice(&1_u16.to_be_bytes());
                response.extend_from_slice(&1_u16.to_be_bytes());
                response.extend_from_slice(&DNS_TTL_SECONDS.to_be_bytes());
                response.extend_from_slice(&4_u16.to_be_bytes());
                response.extend_from_slice(&address.octets());
            }
            DnsAnswer::Ipv6(address) => {
                response.extend_from_slice(&28_u16.to_be_bytes());
                response.extend_from_slice(&1_u16.to_be_bytes());
                response.extend_from_slice(&DNS_TTL_SECONDS.to_be_bytes());
                response.extend_from_slice(&16_u16.to_be_bytes());
                response.extend_from_slice(&address.octets());
            }
        }
    }

    Some(response)
}

fn dns_answers_for_question(table: &DnsAnswerTable, question: &DnsQuestion) -> Vec<DnsAnswer> {
    let Ok(hostname) = RouteHostname::try_new(question.hostname.clone()) else {
        return Vec::new();
    };
    table
        .answers_for(&hostname)
        .iter()
        .filter(|answer| matches_dns_question_type(answer, question.qtype))
        .cloned()
        .collect()
}

fn matches_dns_question_type(answer: &DnsAnswer, qtype: u16) -> bool {
    matches!(
        (answer, qtype),
        (DnsAnswer::Ipv4(_), 1) | (DnsAnswer::Ipv6(_), 28)
    )
}

struct DnsQuestion {
    hostname: String,
    qtype: u16,
    end_offset: usize,
}

fn parse_dns_question(query: &[u8]) -> Option<DnsQuestion> {
    let question_count = read_u16(query, 4)?;
    if query.len() < 12 || question_count == 0 {
        return None;
    }
    let flags = read_u16(query, 2)?;
    if flags & 0x8000 != 0 {
        return None;
    }

    let mut offset = 12;
    let mut labels = Vec::new();
    loop {
        let len = *query.get(offset)? as usize;
        offset += 1;
        if len == 0 {
            break;
        }
        if len > 63 || offset + len > query.len() {
            return None;
        }
        labels.push(std::str::from_utf8(query.get(offset..offset + len)?).ok()?);
        offset += len;
    }
    if offset + 4 > query.len() || labels.is_empty() {
        return None;
    }
    let qtype = read_u16(query, offset)?;
    let qclass = read_u16(query, offset + 2)?;
    if qclass != 1 {
        return None;
    }

    Some(DnsQuestion {
        hostname: labels.join("."),
        qtype,
        end_offset: offset + 4,
    })
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let bytes = bytes.get(offset..offset + 2)?;
    let [high, low] = bytes else {
        return None;
    };
    Some(u16::from_be_bytes([*high, *low]))
}

#[cfg(test)]
mod tests {
    use super::dns_response_from_query;
    use crate::dns::{
        DnsAnswer, DnsProjectionInput, DnsProjectionUpdate, DnsRecordSet, DnsRuntime,
    };
    use ployz_core::ops::RouteHostname;

    #[test]
    fn dns_udp_answers_a_records_from_table() {
        let response = dns_response_from_query(
            runtime([record("api.example.com", ["203.0.113.10"])]).answers(),
            &query_packet(0x1234, "api.example.com", 1),
        )
        .expect("query has a response");

        assert_eq!(first_answer(&response), Some((1, vec![203, 0, 113, 10])));
    }

    #[test]
    fn dns_udp_answers_aaaa_records_from_table() {
        let response = dns_response_from_query(
            runtime([record("api.example.com", ["2001:db8::10"])]).answers(),
            &query_packet(0x1234, "api.example.com", 28),
        )
        .expect("query has a response");

        assert_eq!(
            first_answer(&response),
            Some((
                28,
                vec![
                    0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10
                ]
            ))
        );
    }

    #[test]
    fn dns_udp_returns_empty_answer_section_for_unknown_names() {
        let response = dns_response_from_query(
            runtime([record("api.example.com", ["203.0.113.10"])]).answers(),
            &query_packet(0x1234, "missing.example.com", 1),
        )
        .expect("query has a response");

        assert_eq!(u16_at(&response, 6), Some(0));
    }

    #[test]
    fn dns_udp_ignores_malformed_queries() {
        assert_eq!(
            dns_response_from_query(runtime([]).answers(), &[0, 1, 2]),
            None
        );
    }

    fn runtime<const N: usize>(records: [DnsRecordSet; N]) -> DnsRuntime {
        let mut runtime = DnsRuntime::new();
        runtime.apply_source_update(DnsProjectionUpdate::SourceAvailable(DnsProjectionInput {
            records: records.into(),
        }));
        runtime
    }

    fn record<const N: usize>(hostname: &str, answers: [&str; N]) -> DnsRecordSet {
        DnsRecordSet {
            hostname: RouteHostname::try_new(hostname).expect("valid hostname"),
            answers: answers
                .into_iter()
                .map(|answer| DnsAnswer::try_new(answer).expect("valid answer"))
                .collect(),
        }
    }

    fn query_packet(id: u16, hostname: &str, qtype: u16) -> Vec<u8> {
        let mut packet = Vec::new();
        packet.extend_from_slice(&id.to_be_bytes());
        packet.extend_from_slice(&0x0100_u16.to_be_bytes());
        packet.extend_from_slice(&1_u16.to_be_bytes());
        packet.extend_from_slice(&0_u16.to_be_bytes());
        packet.extend_from_slice(&0_u16.to_be_bytes());
        packet.extend_from_slice(&0_u16.to_be_bytes());
        for label in hostname.split('.') {
            packet.push(label.len() as u8);
            packet.extend_from_slice(label.as_bytes());
        }
        packet.push(0);
        packet.extend_from_slice(&qtype.to_be_bytes());
        packet.extend_from_slice(&1_u16.to_be_bytes());
        packet
    }

    fn first_answer(response: &[u8]) -> Option<(u16, Vec<u8>)> {
        let mut offset = 12;
        while *response.get(offset)? != 0 {
            offset += 1 + *response.get(offset)? as usize;
        }
        offset += 5;
        if response.get(offset..offset + 2)? != [0xc0, 0x0c] {
            return None;
        }
        offset += 2;
        let answer_type = u16_at(response, offset)?;
        offset += 8;
        let data_len = u16_at(response, offset)? as usize;
        offset += 2;
        Some((
            answer_type,
            response.get(offset..offset + data_len)?.to_vec(),
        ))
    }

    fn u16_at(bytes: &[u8], offset: usize) -> Option<u16> {
        let bytes = bytes.get(offset..offset + 2)?;
        let [high, low] = bytes else {
            return None;
        };
        Some(u16::from_be_bytes([*high, *low]))
    }
}
