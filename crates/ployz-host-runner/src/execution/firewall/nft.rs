use ipnet::Ipv6Net;

use super::CORROSION_CHAIN;

pub(super) fn manages_input(ruleset: &str) -> bool {
    let chains = parse_chains(ruleset);
    let input_chains = chains
        .iter()
        .filter(|chain| chain.lines.iter().any(|line| line.contains("hook input")))
        .collect::<Vec<_>>();
    if input_chains.is_empty() {
        return ruleset.lines().any(|line| line.contains("hook input"));
    }
    let [input] = input_chains.as_slice() else {
        return true;
    };
    if !ployz_input_chain(input) {
        return true;
    }
    let corrosion_chains = chains
        .iter()
        .filter(|chain| chain.table == "ip6 filter" && identifier_eq(&chain.name, CORROSION_CHAIN))
        .collect::<Vec<_>>();
    let [corrosion] = corrosion_chains.as_slice() else {
        return true;
    };
    !corrosion
        .lines
        .iter()
        .all(|line| ployz_corrosion_rule(line))
}

#[derive(Debug, PartialEq, Eq)]
struct NftChain {
    table: String,
    name: String,
    lines: Vec<String>,
}

fn parse_chains(ruleset: &str) -> Vec<NftChain> {
    let mut table = None;
    let mut chain = None;
    let mut chains = Vec::new();
    for line in ruleset.lines().map(str::trim) {
        if let Some(name) = line
            .strip_prefix("table ")
            .and_then(|line| line.strip_suffix(" {"))
        {
            table = Some(name.to_owned());
            continue;
        }
        if let Some(name) = line
            .strip_prefix("chain ")
            .and_then(|line| line.strip_suffix(" {"))
        {
            if let Some(table) = &table {
                chain = Some(NftChain {
                    table: table.clone(),
                    name: name.to_owned(),
                    lines: Vec::new(),
                });
            }
            continue;
        }
        if line == "}" {
            if let Some(chain) = chain.take() {
                chains.push(chain);
            } else {
                table = None;
            }
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(chain) = &mut chain {
            chain.lines.push(line.to_owned());
        }
    }
    chains
}

fn ployz_input_chain(chain: &NftChain) -> bool {
    if chain.table != "ip6 filter" || !identifier_eq(&chain.name, "INPUT") {
        return false;
    }
    let declarations = chain
        .lines
        .iter()
        .filter(|line| line.contains("hook input"))
        .collect::<Vec<_>>();
    let rules = chain
        .lines
        .iter()
        .filter(|line| !line.contains("hook input"))
        .collect::<Vec<_>>();
    matches!(declarations.as_slice(), [declaration] if input_policy_accept(declaration))
        && matches!(rules.as_slice(), [rule] if ployz_input_jump(rule))
}

fn input_policy_accept(declaration: &str) -> bool {
    let clauses = declaration
        .split(';')
        .map(str::trim)
        .filter(|clause| !clause.is_empty())
        .collect::<Vec<_>>();
    matches!(clauses.as_slice(), [hook, "policy accept"] if
        hook == &"type filter hook input priority filter"
            || hook == &"type filter hook input priority 0")
}

fn ployz_input_jump(rule: &str) -> bool {
    let tokens = without_counter(rule);
    let ["iifname", interface, "udp", "dport", port, "jump", chain] = tokens.as_slice() else {
        return false;
    };
    quoted_interface(interface)
        && port.parse::<u16>().is_ok_and(|port| port != 0)
        && identifier_eq(chain, CORROSION_CHAIN)
}

fn ployz_corrosion_rule(rule: &str) -> bool {
    let tokens = without_counter(rule);
    match tokens.as_slice() {
        ["ip6", "saddr", source, "accept"] => source
            .parse::<Ipv6Net>()
            .is_ok_and(|source| source.prefix_len() == 112),
        ["reject"] | ["reject", "with", "icmpv6", "type", _] => true,
        _ => false,
    }
}

fn without_counter(rule: &str) -> Vec<&str> {
    let tokens = rule.split_ascii_whitespace().collect::<Vec<_>>();
    let Some(counter) = tokens.iter().position(|token| *token == "counter") else {
        return tokens;
    };
    if tokens.get(counter..counter + 5).is_some_and(|tokens| {
        matches!(tokens, ["counter", "packets", packets, "bytes", bytes]
            if packets.parse::<u64>().is_ok() && bytes.parse::<u64>().is_ok())
    }) {
        tokens
            .iter()
            .take(counter)
            .chain(tokens.iter().skip(counter + 5))
            .copied()
            .collect()
    } else {
        tokens
    }
}

fn quoted_interface(value: &str) -> bool {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .is_some_and(|value| {
            !value.is_empty()
                && value.len() <= 15
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        })
}

pub(super) fn identifier_eq(value: &str, expected: &str) -> bool {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value)
        == expected
}

#[cfg(test)]
mod tests {
    use super::manages_input;

    #[test]
    fn differently_cased_nft_identifiers_are_foreign() {
        for ruleset in [
            r#"
table ip6 filter {
    chain input {
        type filter hook input priority filter; policy accept;
        iifname "ployz0" udp dport 8787 jump PLOYZ-CORROSION
    }
    chain PLOYZ-CORROSION { reject }
}
"#,
            r#"
table ip6 filter {
    chain INPUT {
        type filter hook input priority filter; policy accept;
        iifname "ployz0" udp dport 8787 jump ployz-corrosion
    }
    chain ployz-corrosion { reject }
}
"#,
        ] {
            assert!(manages_input(ruleset), "must preserve foreign {ruleset}");
        }
    }
}
