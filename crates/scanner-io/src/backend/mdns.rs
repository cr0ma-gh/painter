//! Finding eSCL (AirScan) scanners on the local network: a DNS-SD query for
//! `_uscan._tcp.local` sent to the mDNS group (224.0.0.251:5353) from an
//! ordinary port, so responders answer us directly ("legacy unicast",
//! RFC 6762 §6.7). The answers name each scanner (PTR), where it listens
//! (SRV, A) and the eSCL root path (TXT `rs`, usually `eSCL`).
//!
//! On iPadOS apps may not use multicast themselves; the app asks Bonjour
//! instead and hands the results to [`super::escl::add_targets`].

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use super::escl::Target;

const SERVICE: &str = "_uscan._tcp.local";
const PTR: u16 = 12;
const SRV: u16 = 33;
const TXT: u16 = 16;
const A: u16 = 1;

/// A DNS query for `name` / `qtype`, asking for a unicast answer.
pub fn query(name: &str, qtype: u16) -> Vec<u8> {
    let mut q = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in name.split('.').filter(|l| !l.is_empty()) {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&qtype.to_be_bytes());
    q.extend_from_slice(&0x8001u16.to_be_bytes()); // IN, unicast response wanted
    q
}

/// A (possibly compressed) name at `at`; returns it and the offset after it.
fn read_name(msg: &[u8], mut at: usize) -> Option<(String, usize)> {
    let mut labels = Vec::new();
    let mut end = None;
    for _ in 0..64 {
        let len = *msg.get(at)? as usize;
        if len == 0 {
            return Some((labels.join("."), end.unwrap_or(at + 1)));
        }
        if len & 0xC0 == 0xC0 {
            let ptr = ((len & 0x3F) << 8) | *msg.get(at + 1)? as usize;
            end.get_or_insert(at + 2);
            at = ptr;
            continue;
        }
        labels.push(String::from_utf8_lossy(msg.get(at + 1..at + 1 + len)?).into_owned());
        at += 1 + len;
    }
    None
}

/// One resource record.
#[derive(Debug, Clone, PartialEq)]
pub enum Record {
    Ptr { name: String, target: String },
    Srv { name: String, port: u16, target: String },
    Txt { name: String, entries: Vec<(String, String)> },
    A { name: String, addr: Ipv4Addr },
}

/// The answer and additional records of a DNS message.
pub fn parse(msg: &[u8]) -> Option<Vec<Record>> {
    let count = |i: usize| -> Option<usize> { Some(u16::from_be_bytes(msg.get(i..i + 2)?.try_into().ok()?) as usize) };
    let (qd, an, ns, ar) = (count(4)?, count(6)?, count(8)?, count(10)?);
    let mut at = 12;
    for _ in 0..qd {
        at = read_name(msg, at)?.1 + 4;
    }
    let mut out = Vec::new();
    for _ in 0..an + ns + ar {
        let (name, after) = read_name(msg, at)?;
        let rtype = u16::from_be_bytes(msg.get(after..after + 2)?.try_into().ok()?);
        let len = count(after + 8)?;
        let data = after + 10;
        let rdata = msg.get(data..data + len)?;
        match rtype {
            PTR => out.push(Record::Ptr { name, target: read_name(msg, data)?.0 }),
            SRV if len >= 7 => {
                let port = u16::from_be_bytes([rdata[4], rdata[5]]);
                out.push(Record::Srv { name, port, target: read_name(msg, data + 6)?.0 });
            }
            TXT => {
                let mut entries = Vec::new();
                let mut i = 0;
                while i < rdata.len() {
                    let l = rdata[i] as usize;
                    let s = String::from_utf8_lossy(rdata.get(i + 1..i + 1 + l).unwrap_or_default()).into_owned();
                    let (k, v) = s.split_once('=').unwrap_or((s.as_str(), ""));
                    entries.push((k.to_ascii_lowercase(), v.to_string()));
                    i += 1 + l;
                }
                out.push(Record::Txt { name, entries });
            }
            A if len == 4 => out.push(Record::A { name, addr: Ipv4Addr::new(rdata[0], rdata[1], rdata[2], rdata[3]) }),
            _ => {}
        }
        at = data + len;
    }
    Some(out)
}

/// Scanners from the records gathered (and the address each answer came
/// from, used when a scanner sends no A record).
pub fn targets(records: &[(Record, IpAddr)]) -> Vec<Target> {
    let mut srv: HashMap<&str, (u16, &str)> = HashMap::new();
    let mut txt: HashMap<&str, &Vec<(String, String)>> = HashMap::new();
    let mut a: HashMap<&str, Ipv4Addr> = HashMap::new();
    for (r, _) in records {
        match r {
            Record::Srv { name, port, target } => {
                srv.insert(name, (*port, target));
            }
            Record::Txt { name, entries } => {
                txt.insert(name, entries);
            }
            Record::A { name, addr } => {
                a.insert(name, *addr);
            }
            Record::Ptr { .. } => {}
        }
    }
    let mut out: Vec<Target> = Vec::new();
    for (r, from) in records {
        let Record::Ptr { name, target: instance } = r else { continue };
        if !name.eq_ignore_ascii_case(SERVICE) {
            continue;
        }
        let (port, host) = srv.get(instance.as_str()).copied().unwrap_or((80, ""));
        let addr = a.get(host).map(|ip| IpAddr::V4(*ip)).unwrap_or(*from);
        let entries = txt.get(instance.as_str());
        let get = |k: &str| entries.and_then(|e| e.iter().find(|(key, _)| key == k)).map(|(_, v)| v.clone());
        let rs = get("rs").unwrap_or_else(|| "eSCL".into());
        let label = instance.strip_suffix(&format!(".{SERVICE}")).unwrap_or(instance).to_string();
        let name = get("ty").filter(|t| !t.is_empty()).unwrap_or(label);
        let host = match addr {
            IpAddr::V4(v4) => v4.to_string(),
            IpAddr::V6(v6) => format!("[{v6}]"),
        };
        let url = format!("http://{host}:{port}/{}", rs.trim_matches('/'));
        if !out.iter().any(|t| t.url == url) {
            out.push(Target { name, url });
        }
    }
    out
}

/// Asks the network for eSCL scanners for about `wait`.
pub fn discover(wait: Duration) -> Vec<Target> {
    let Ok(socket) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) else { return Vec::new() };
    let _ = socket.set_read_timeout(Some(Duration::from_millis(200)));
    let group = SocketAddr::from((Ipv4Addr::new(224, 0, 0, 251), 5353));
    let mut records = Vec::new();
    let start = Instant::now();
    let mut asked = 0;
    let mut buf = [0u8; 9000];
    while start.elapsed() < wait {
        // Ask a few times: UDP may drop one.
        if asked < 3 && start.elapsed() >= Duration::from_millis(400 * asked) {
            let _ = socket.send_to(&query(SERVICE, PTR), group);
            asked += 1;
        }
        if let Ok((n, from)) = socket.recv_from(&mut buf) {
            if let Some(found) = parse(&buf[..n]) {
                records.extend(found.into_iter().map(|r| (r, from.ip())));
            }
        }
    }
    // Instances whose address or port didn't come with the answer: ask for them.
    let mut found = targets(&records);
    let missing: Vec<String> = records
        .iter()
        .filter_map(|(r, _)| match r {
            Record::Ptr { target, .. } if !records.iter().any(|(s, _)| matches!(s, Record::Srv { name, .. } if name == target)) => {
                Some(target.clone())
            }
            _ => None,
        })
        .collect();
    if !missing.is_empty() {
        for name in &missing {
            let _ = socket.send_to(&query(name, SRV), group);
            let _ = socket.send_to(&query(name, TXT), group);
        }
        let again = Instant::now();
        while again.elapsed() < Duration::from_millis(800) {
            if let Ok((n, from)) = socket.recv_from(&mut buf) {
                if let Some(more) = parse(&buf[..n]) {
                    records.extend(more.into_iter().map(|r| (r, from.ip())));
                }
            }
        }
        found = targets(&records);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scanner's answer: PTR, SRV, TXT and A, with name compression.
    fn answer() -> Vec<u8> {
        let mut m = vec![0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 3];
        let name = |m: &mut Vec<u8>, s: &str| {
            for l in s.split('.') {
                m.push(l.len() as u8);
                m.extend_from_slice(l.as_bytes());
            }
            m.push(0);
        };
        let rr = |m: &mut Vec<u8>, t: u16, data: Vec<u8>| {
            m.extend_from_slice(&t.to_be_bytes());
            m.extend_from_slice(&[0, 1, 0, 0, 0x11, 0x94]);
            m.extend_from_slice(&(data.len() as u16).to_be_bytes());
            m.extend(data);
        };
        // PTR _uscan._tcp.local → "Brother MFC._uscan._tcp.local" (pointing back into the name).
        let service_at = m.len();
        name(&mut m, "_uscan._tcp.local");
        let mut ptr = vec![11];
        ptr.extend_from_slice(b"Brother MFC");
        ptr.extend_from_slice(&[0xC0, service_at as u8]);
        rr(&mut m, PTR, ptr);
        // SRV for the instance: port 80, target brother.local.
        let instance_at = m.len();
        m.push(11);
        m.extend_from_slice(b"Brother MFC");
        m.extend_from_slice(&[0xC0, service_at as u8]);
        let mut srv = vec![0, 0, 0, 0, 0, 80];
        name(&mut srv, "brother.local");
        rr(&mut m, SRV, srv);
        // TXT rs=eSCL, ty=Brother MFC-J5340DW.
        m.extend_from_slice(&[0xC0, instance_at as u8]);
        let mut txt = Vec::new();
        for e in ["rs=eSCL", "ty=Brother MFC-J5340DW"] {
            txt.push(e.len() as u8);
            txt.extend_from_slice(e.as_bytes());
        }
        rr(&mut m, TXT, txt);
        name(&mut m, "brother.local");
        rr(&mut m, A, vec![192, 168, 1, 30]);
        m
    }

    #[test]
    fn queries_and_answers() {
        let q = query(SERVICE, PTR);
        assert_eq!(&q[12..19], b"\x06_uscan");
        assert_eq!(&q[q.len() - 4..], &[0, 12, 0x80, 1]);
        let records = parse(&answer()).unwrap();
        assert_eq!(records.len(), 4);
        assert!(matches!(&records[0], Record::Ptr { target, .. } if target == "Brother MFC._uscan._tcp.local"));
        let from = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 99));
        let found = targets(&records.into_iter().map(|r| (r, from)).collect::<Vec<_>>());
        assert_eq!(found, vec![Target { name: "Brother MFC-J5340DW".into(), url: "http://192.168.1.30:80/eSCL".into() }]);
        assert!(parse(b"\x00\x01").is_none());
    }
}
