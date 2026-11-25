//! Forgetting the kernel's tracked flows, over ctnetlink. The fence returns an established
//! flow before any preview rule reads it, so a flow that began while an address sat in one
//! preview's set would otherwise carry on to whoever takes the address next.
//!
//! Each flow is dumped and deleted by its own tuple. A delete by filter would be one call,
//! but a kernel older than the filter ignores it and flushes every tracked flow here.

use std::{io, net::Ipv4Addr};

use netlink_sys::{Socket, SocketAddr, protocols::NETLINK_NETFILTER};

/// `NFNL_SUBSYS_CTNETLINK << 8` with `IPCTNL_MSG_CT_GET` and `IPCTNL_MSG_CT_DELETE`.
const CT_GET: u16 = 0x101;
const CT_DELETE: u16 = 0x102;
const NLM_F_REQUEST: u16 = 1;
const NLM_F_ACK: u16 = 4;
const NLM_F_DUMP: u16 = 0x300;
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
/// `CTA_TUPLE_ORIG`, and within it `CTA_TUPLE_IP`, whose `CTA_IP_V4_SRC` and `CTA_IP_V4_DST`
/// are 1 and 2 too; `CTA_ZONE` keeps a delete to the flow's own zone.
const CTA_TUPLE: u16 = 1;
const CTA_ZONE: u16 = 18;
const NLA_F_NESTED: u16 = 0x8000;
const AF_INET: u8 = 2;

/// Delete every IPv4 flow whose original source and destination `doomed` picks. A flow
/// already gone by its delete counts as deleted.
///
/// # Errors
/// Returns why the kernel could not be asked, or refused a dump or a delete.
pub(super) fn forget_flows(doomed: impl Fn(Ipv4Addr, Ipv4Addr) -> bool) -> io::Result<()> {
    let mut socket = Socket::new(NETLINK_NETFILTER)?;
    socket.bind_auto()?;
    socket.connect(&SocketAddr::new(0, 0))?;
    socket.send(&message(CT_GET, NLM_F_REQUEST | NLM_F_DUMP, &[]), 0)?;
    let mut deletes = Vec::new();
    while let Some(flows) = replies(&socket)? {
        for flow in flows {
            let (tuple, zone) = (attribute(&flow, CTA_TUPLE), attribute(&flow, CTA_ZONE));
            let ends = tuple
                .and_then(|tuple| attribute(tuple, CTA_TUPLE))
                .map(|ip| [1, 2].map(|end| attribute(ip, end).and_then(ipv4)));
            if let (Some(tuple), Some([Some(source), Some(destination)])) = (tuple, ends)
                && doomed(source, destination)
            {
                let mut request = nla(CTA_TUPLE | NLA_F_NESTED, tuple);
                request.extend(zone.map(|zone| nla(CTA_ZONE, zone)).unwrap_or_default());
                deletes.push(request);
            }
        }
    }
    for flow in deletes {
        socket.send(&message(CT_DELETE, NLM_F_REQUEST | NLM_F_ACK, &flow), 0)?;
        replies(&socket)?;
    }
    Ok(())
}

/// The attributes of each flow in the next datagram, `None` once the dump is done. An error
/// reply other than a missing flow fails it.
fn replies(socket: &Socket) -> io::Result<Option<Vec<Vec<u8>>>> {
    let mut buffer = vec![0; 1 << 16];
    let read = socket.recv(&mut &mut buffer[..], 0)?;
    let (mut rest, mut flows) = (buffer.get(..read).unwrap_or_default(), Vec::new());
    while let Some((kind, payload, next)) = split(rest, 16) {
        rest = next;
        match kind {
            NLMSG_DONE => return Ok(None),
            NLMSG_ERROR => {
                let code = payload
                    .first_chunk()
                    .map_or(0, |code| i32::from_ne_bytes(*code));
                if code != 0 && code != -nix::libc::ENOENT {
                    return Err(io::Error::from_raw_os_error(-code));
                }
                return Ok(Some(flows));
            }
            _ => flows.push(payload.get(4..).unwrap_or_default().to_vec()),
        }
    }
    Ok(Some(flows))
}

/// The value of the first attribute `kind` among `attributes`.
fn attribute(mut attributes: &[u8], kind: u16) -> Option<&[u8]> {
    while let Some((found, value, rest)) = split(attributes, 4) {
        if found & 0x3fff == kind {
            return Some(value);
        }
        attributes = rest;
    }
    None
}

/// The first netlink message or attribute in `bytes`, whose header is `header` long with its
/// length first and its type after it: its type, its payload and what follows it, aligned.
fn split(bytes: &[u8], header: usize) -> Option<(u16, &[u8], &[u8])> {
    let (length, kind_at) = match header {
        16 => (u32::from_ne_bytes(*bytes.first_chunk()?), 4),
        _ => (u32::from(u16::from_ne_bytes(*bytes.first_chunk()?)), 2),
    };
    let length = usize::try_from(length).ok()?;
    let kind = u16::from_ne_bytes(*bytes.get(kind_at..)?.first_chunk()?);
    let payload = bytes.get(header..length)?;
    Some((
        kind,
        payload,
        bytes.get(length.next_multiple_of(4)..).unwrap_or_default(),
    ))
}

fn ipv4(value: &[u8]) -> Option<Ipv4Addr> {
    value
        .first_chunk()
        .map(|octets: &[u8; 4]| Ipv4Addr::from(*octets))
}

fn nla(kind: u16, value: &[u8]) -> Vec<u8> {
    let length = u16::try_from(value.len() + 4).unwrap_or(u16::MAX);
    let mut nla = [&length.to_ne_bytes()[..], &kind.to_ne_bytes(), value].concat();
    nla.resize(nla.len().next_multiple_of(4), 0);
    nla
}

/// A ctnetlink request: the netlink header, an IPv4 `nfgenmsg`, then `attributes`.
fn message(kind: u16, flags: u16, attributes: &[u8]) -> Vec<u8> {
    let length = u32::try_from(20 + attributes.len()).unwrap_or(u32::MAX);
    let header = [
        &length.to_ne_bytes()[..],
        &kind.to_ne_bytes(),
        &flags.to_ne_bytes(),
    ];
    [
        &header.concat()[..],
        &[0; 8],
        &[AF_INET, 0, 0, 0],
        attributes,
    ]
    .concat()
}
