// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The address a request came from.
//!
//! The connection names its peer. Behind a reverse proxy that peer is the
//! proxy, and the client is named in a header the proxy appends to:
//! `Forwarded` (RFC 7239 §4) or `X-Forwarded-For`. Any client can send a
//! header, so it is read only from a peer in `trusted_proxies` (RFC 7239
//! §8.1), and only the hops those proxies appended are believed: read from
//! the right, the first hop that is no trusted proxy is the client. A hop
//! that names no address, such as `unknown` or an obfuscated identifier
//! (RFC 7239 §6), ends the walk at the last address a trusted proxy vouched
//! for. No specification governs the rest: our own design.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::{ConnectInfo, Request, State};
use axum::middleware::Next;
use axum::response::Response;
use http::HeaderMap;
use http::header::{FORWARDED, HeaderName};
use ipnet::IpNet;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::section::ServerSection;

/// The `X-Forwarded-For` header, which no RFC registers.
pub const X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");

/// The header a trusted proxy names the client in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ForwardedHeader {
    /// `Forwarded`, its `for=` parameters (RFC 7239 §4, §5.2).
    #[default]
    Forwarded,
    /// `X-Forwarded-For`, a comma-separated list of addresses.
    XForwardedFor,
}

/// One entry of `trusted_proxies`: an address, or a CIDR block such as
/// `10.0.0.0/8`.
///
/// # Examples
///
/// ```
/// use ferrohealth_server::client_address::TrustedProxy;
///
/// let block: TrustedProxy = "10.0.0.0/8".parse()?;
/// assert!(block.contains("10.1.2.3".parse()?));
/// let one: TrustedProxy = "192.0.2.7".parse()?;
/// assert!(!one.contains("192.0.2.8".parse()?));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrustedProxy(IpNet);

/// A `trusted_proxies` entry that is neither an address nor a CIDR block.
#[derive(Debug, thiserror::Error)]
#[error("a trusted proxy is an IP address or a CIDR block such as 10.0.0.0/8")]
pub struct TrustedProxyError;

impl TrustedProxy {
    /// Whether `address` is this proxy or inside its block.
    #[must_use]
    pub fn contains(&self, address: IpAddr) -> bool {
        self.0.contains(&address.to_canonical())
    }
}

impl std::str::FromStr for TrustedProxy {
    type Err = TrustedProxyError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        text.parse::<IpNet>()
            .or_else(|_block| text.parse::<IpAddr>().map(IpNet::from))
            .map(Self)
            .map_err(|_address| TrustedProxyError)
    }
}

impl std::fmt::Display for TrustedProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl<'de> Deserialize<'de> for TrustedProxy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

impl Serialize for TrustedProxy {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

/// The trusted proxies and the header they name the client in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Forwarding {
    proxies: Vec<TrustedProxy>,
    header: ForwardedHeader,
}

impl Forwarding {
    /// The forwarding of `proxies`, naming the client in `header`.
    #[must_use]
    pub const fn new(proxies: Vec<TrustedProxy>, header: ForwardedHeader) -> Self {
        Self { proxies, header }
    }

    /// The forwarding `section` configures.
    #[must_use]
    pub fn from_section(section: &ServerSection) -> Self {
        Self::new(section.trusted_proxies.clone(), section.forwarded_header)
    }

    /// Whether `address` is a trusted proxy.
    #[must_use]
    pub fn trusts(&self, address: IpAddr) -> bool {
        self.proxies.iter().any(|proxy| proxy.contains(address))
    }

    /// The address a request from `peer` with `headers` came from: `peer`
    /// unless it is a trusted proxy, and otherwise the rightmost hop of the
    /// configured header that is no trusted proxy.
    #[must_use]
    pub fn client(&self, peer: IpAddr, headers: &HeaderMap) -> IpAddr {
        let mut client = peer.to_canonical();
        if !self.trusts(client) {
            return client;
        }
        for hop in self.hops(headers).iter().rev() {
            let Some(address) = hop else {
                break;
            };
            client = *address;
            if !self.trusts(client) {
                break;
            }
        }
        client
    }

    /// The hops the configured header names, leftmost first; `None` for a
    /// hop that names no address.
    fn hops(&self, headers: &HeaderMap) -> Vec<Option<IpAddr>> {
        let name = match self.header {
            ForwardedHeader::Forwarded => FORWARDED,
            ForwardedHeader::XForwardedFor => X_FORWARDED_FOR,
        };
        let mut hops = Vec::new();
        for value in headers.get_all(name) {
            // NOTE: RFC 9110 §5.5, a value that is not visible ASCII names no
            // address a proxy wrote, so the hop is unknown.
            match value.to_str() {
                Ok(text) => match self.header {
                    ForwardedHeader::Forwarded => hops.extend(forwarded_hops(text)),
                    ForwardedHeader::XForwardedFor => hops.extend(listed_hops(text)),
                },
                Err(_opaque) => hops.push(None),
            }
        }
        hops
    }
}

/// The address a request came from, set on every request served over a
/// connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientAddress(pub IpAddr);

/// The middleware: names the address `request` came from
/// ([`ClientAddress`]) under `forwarding`, from the connection's peer.
///
/// A request served over no connection, as in a test calling the router
/// directly, gets none.
pub async fn attach(
    State(forwarding): State<Arc<Forwarding>>,
    mut request: Request,
    next: Next,
) -> Response {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(peer)| peer.ip());
    if let Some(peer) = peer {
        let client = forwarding.client(peer, request.headers());
        request.extensions_mut().insert(ClientAddress(client));
    }
    next.run(request).await
}

/// The `for=` hops of one `Forwarded` value (RFC 7239 §4): one per element,
/// in order; an element without `for=` names no address.
fn forwarded_hops(text: &str) -> impl Iterator<Item = Option<IpAddr>> + '_ {
    text.split(',').map(|element| {
        element
            .split(';')
            .find_map(|pair| {
                let (name, value) = pair.split_once('=')?;
                name.trim()
                    .eq_ignore_ascii_case("for")
                    .then(|| node(value.trim()))
            })
            .flatten()
    })
}

/// The hops of one `X-Forwarded-For` value, in order.
fn listed_hops(text: &str) -> impl Iterator<Item = Option<IpAddr>> + '_ {
    text.split(',').map(|hop| node(hop.trim()))
}

/// The address an RFC 7239 §6 node names, its port dropped: an IPv4
/// address, a bracketed IPv6 address, either quoted, or a bare IPv6 address
/// as `X-Forwarded-For` writes one; `None` for `unknown`, an obfuscated
/// identifier or anything else.
fn node(text: &str) -> Option<IpAddr> {
    let text = text
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(text);
    if let Ok(address) = text.parse::<IpAddr>() {
        return Some(address.to_canonical());
    }
    if let Some(bracketed) = text.strip_prefix('[') {
        let (address, _port) = bracketed.split_once(']')?;
        // NOTE: RFC 7239 §6, a bracketed node that holds no IPv6 address names
        // no client, so the hop is unknown.
        return address.parse::<IpAddr>().ok().map(|a| a.to_canonical());
    }
    // NOTE: RFC 7239 §6, an IPv4 node may carry a port after one colon.
    let (address, _port) = text.split_once(':')?;
    address.parse::<std::net::Ipv4Addr>().ok().map(IpAddr::V4)
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, SocketAddr};
    use std::sync::Arc;

    use axum::Router;
    use axum::body::Body;
    use axum::extract::{ConnectInfo, Extension};
    use axum::routing::get;
    use http::{HeaderMap, HeaderValue};
    use tower::ServiceExt as _;

    use super::{
        ClientAddress, ForwardedHeader, Forwarding, TrustedProxy, X_FORWARDED_FOR, attach,
        forwarded_hops, listed_hops, node,
    };

    fn ip(text: &str) -> IpAddr {
        text.parse().expect("an address")
    }

    fn proxies(list: &[&str]) -> Vec<TrustedProxy> {
        list.iter().map(|p| p.parse().expect("a proxy")).collect()
    }

    fn headers(name: &http::HeaderName, values: &[&str]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for value in values {
            map.append(name.clone(), HeaderValue::from_str(value).expect("a value"));
        }
        map
    }

    #[test]
    fn a_node_names_an_address_with_or_without_its_port() {
        assert_eq!(Some(ip("192.0.2.43")), node("192.0.2.43"));
        assert_eq!(Some(ip("192.0.2.43")), node("192.0.2.43:47011"));
        assert_eq!(
            Some(ip("2001:db8:cafe::17")),
            node("\"[2001:db8:cafe::17]:4711\"")
        );
        assert_eq!(Some(ip("2001:db8:cafe::17")), node("2001:db8:cafe::17"));
        assert_eq!(Some(ip("192.0.2.1")), node("::ffff:192.0.2.1"));
        for none in ["unknown", "_hidden", "", "[nope]"] {
            assert_eq!(None, node(none), "{none:?}");
        }
    }

    #[test]
    fn the_for_parameter_of_each_element_is_a_hop() {
        assert_eq!(
            vec![Some(ip("192.0.2.60")), Some(ip("198.51.100.17")), None],
            forwarded_hops(
                "for=192.0.2.60;proto=http;by=203.0.113.43, For=\"198.51.100.17\", by=10.0.0.1"
            )
            .collect::<Vec<_>>()
        );
        assert_eq!(
            vec![Some(ip("192.0.2.60")), None],
            listed_hops("192.0.2.60, unknown").collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_forwarded_header_is_believed_from_a_trusted_peer_only() {
        let forwarding = Forwarding::new(proxies(&["10.0.0.0/8"]), ForwardedHeader::Forwarded);
        let sent = headers(
            &http::header::FORWARDED,
            &["for=198.51.100.9", "for=10.0.0.2"],
        );
        assert_eq!(ip("198.51.100.9"), forwarding.client(ip("10.0.0.1"), &sent));
        assert_eq!(
            ip("203.0.113.5"),
            forwarding.client(ip("203.0.113.5"), &sent),
            "an untrusted peer is the client"
        );
        let spoofed = headers(
            &http::header::FORWARDED,
            &["for=192.0.2.1, for=198.51.100.9"],
        );
        assert_eq!(
            ip("198.51.100.9"),
            forwarding.client(ip("10.0.0.1"), &spoofed),
            "only the trusted hop is believed"
        );
    }

    #[test]
    fn x_forwarded_for_is_read_when_configured() {
        let forwarding = Forwarding::new(
            proxies(&["10.0.0.1", "10.0.0.2"]),
            ForwardedHeader::XForwardedFor,
        );
        let sent = headers(&X_FORWARDED_FOR, &["2001:db8::1, 10.0.0.2"]);
        assert_eq!(ip("2001:db8::1"), forwarding.client(ip("10.0.0.1"), &sent));
        let forwarded = headers(&http::header::FORWARDED, &["for=198.51.100.9"]);
        assert_eq!(
            ip("10.0.0.1"),
            forwarding.client(ip("10.0.0.1"), &forwarded),
            "the other header is not read"
        );
    }

    #[test]
    fn an_unknown_hop_ends_the_walk_at_the_last_vouched_address() {
        let forwarding = Forwarding::new(proxies(&["10.0.0.0/8"]), ForwardedHeader::Forwarded);
        let sent = headers(
            &http::header::FORWARDED,
            &["for=198.51.100.9, for=unknown, for=10.0.0.3"],
        );
        assert_eq!(ip("10.0.0.3"), forwarding.client(ip("10.0.0.1"), &sent));
        let hidden = headers(&http::header::FORWARDED, &["for=_hidden"]);
        assert_eq!(ip("10.0.0.1"), forwarding.client(ip("10.0.0.1"), &hidden));
    }

    #[test]
    fn a_trusted_proxy_is_an_address_or_a_block() {
        assert!("10.0.0.0/8".parse::<TrustedProxy>().is_ok());
        assert!("::1".parse::<TrustedProxy>().is_ok());
        assert!("a proxy".parse::<TrustedProxy>().is_err());
        assert_eq!(
            "192.0.2.7/32",
            "192.0.2.7"
                .parse::<TrustedProxy>()
                .expect("an address")
                .to_string()
        );
    }

    #[tokio::test]
    async fn the_middleware_sets_the_client_address_from_the_connection() {
        let forwarding = Arc::new(Forwarding::new(
            proxies(&["127.0.0.1"]),
            ForwardedHeader::XForwardedFor,
        ));
        let app = Router::new()
            .route(
                "/",
                get(|client: Option<Extension<ClientAddress>>| async move {
                    client.map_or_else(
                        || "none".to_owned(),
                        |Extension(ClientAddress(a))| a.to_string(),
                    )
                }),
            )
            .layer(axum::middleware::from_fn_with_state(forwarding, attach));
        let call = |connected: bool| {
            let app = app.clone();
            async move {
                let mut request = http::Request::builder()
                    .uri("/")
                    .header(X_FORWARDED_FOR, "198.51.100.9")
                    .body(Body::empty())
                    .expect("the request builds");
                if connected {
                    request
                        .extensions_mut()
                        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40_000))));
                }
                let response = app.oneshot(request).await.expect("served");
                let body = axum::body::to_bytes(response.into_body(), 64)
                    .await
                    .expect("the body");
                String::from_utf8(body.to_vec()).expect("UTF-8")
            }
        };
        assert_eq!("198.51.100.9", call(true).await);
        assert_eq!("none", call(false).await);
    }
}
