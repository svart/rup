use std::fmt;
use std::net::SocketAddr;
use std::str::FromStr;

/// HTTP protocol version used by the HTTP transport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HttpVersion {
    /// HTTP/1.1 with a new connection for every request.
    Http1,
    /// HTTP/2 with one persistent connection shared by all requests.
    Http2,
}

/// URL scheme used by an [`HttpTarget`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HttpScheme {
    /// Plain HTTP, default port 80.
    Http,
    /// HTTP over TLS, default port 443.
    Https,
}

impl HttpScheme {
    /// Lowercase scheme name used in URLs.
    pub const fn as_str(self) -> &'static str {
        match self {
            HttpScheme::Http => "http",
            HttpScheme::Https => "https",
        }
    }

    /// Port used when the URL does not specify one.
    pub const fn default_port(self) -> u16 {
        match self {
            HttpScheme::Http => 80,
            HttpScheme::Https => 443,
        }
    }
}

/// URL requested with `GET` by the HTTP transport.
///
/// Accepts `http://` and `https://` URLs; a URL without a scheme uses `http`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpTarget {
    scheme: HttpScheme,
    /// Host name or IP address, without IPv6 brackets.
    host: String,
    port: u16,
    /// Origin-form path with optional query; always starts with `/`.
    path: String,
}

impl HttpTarget {
    /// Plain HTTP target for `GET /` on `remote`.
    pub fn from_socket_addr(remote: SocketAddr) -> Self {
        Self {
            scheme: HttpScheme::Http,
            host: remote.ip().to_string(),
            port: remote.port(),
            path: "/".to_owned(),
        }
    }

    /// Scheme parsed from the URL.
    pub fn scheme(&self) -> HttpScheme {
        self.scheme
    }

    /// Host name or IP address, without IPv6 brackets.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Explicit port, or the scheme default when the URL omits it.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Origin-form request path, including the query string when present.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// `Host` header value; omits the port when it is the scheme default.
    pub fn authority(&self) -> String {
        if self.port == self.scheme.default_port() {
            self.bracketed_host()
        } else {
            self.host_port()
        }
    }

    /// `host:port` string for DNS resolution.
    pub fn host_port(&self) -> String {
        format!("{}:{}", self.bracketed_host(), self.port)
    }

    fn bracketed_host(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        }
    }
}

impl fmt::Display for HttpTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}://{}{}",
            self.scheme.as_str(),
            self.authority(),
            self.path
        )
    }
}

impl FromStr for HttpTarget {
    type Err = String;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let (scheme, rest) = match input.split_once("://") {
            Some((scheme, rest)) => match scheme.to_ascii_lowercase().as_str() {
                "http" => (HttpScheme::Http, rest),
                "https" => (HttpScheme::Https, rest),
                _ => return Err(format!("unsupported URL scheme: {scheme}")),
            },
            None => (HttpScheme::Http, input),
        };

        let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(authority_end);
        if authority.contains('@') {
            return Err(format!("credentials in URL are not supported: {input}"));
        }

        let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
            let (host, after) = bracketed
                .split_once(']')
                .ok_or_else(|| format!("unterminated IPv6 address in URL: {input}"))?;
            let port =
                match after {
                    "" => None,
                    _ => Some(after.strip_prefix(':').ok_or_else(|| {
                        format!("unexpected characters after IPv6 address: {input}")
                    })?),
                };
            (host, port)
        } else if authority.matches(':').count() > 1 {
            return Err(format!("IPv6 address in URL must be bracketed: {input}"));
        } else {
            match authority.split_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (authority, None),
            }
        };

        if host.is_empty() {
            return Err(format!("missing host in URL: {input}"));
        }
        let port = match port {
            None | Some("") => scheme.default_port(),
            Some(port) => port
                .parse()
                .map_err(|_| format!("invalid port in URL: {input}"))?,
        };

        let path = tail.split('#').next().unwrap_or_default();
        let path = if path.starts_with('/') {
            path.to_owned()
        } else {
            format!("/{path}")
        };

        Ok(Self {
            scheme,
            host: host.to_owned(),
            port,
            path,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> HttpTarget {
        input.parse().unwrap()
    }

    #[test]
    fn parses_https_with_default_port_and_root_path() {
        let target = parse("https://example.com");

        assert_eq!(target.scheme(), HttpScheme::Https);
        assert_eq!(target.host(), "example.com");
        assert_eq!(target.port(), 443);
        assert_eq!(target.path(), "/");
        assert_eq!(target.authority(), "example.com");
        assert_eq!(target.host_port(), "example.com:443");
        assert_eq!(target.to_string(), "https://example.com/");
    }

    #[test]
    fn missing_scheme_defaults_to_http() {
        let target = parse("example.com:8080");

        assert_eq!(target.scheme(), HttpScheme::Http);
        assert_eq!(target.port(), 8080);
        assert_eq!(target.authority(), "example.com:8080");
    }

    #[test]
    fn keeps_path_and_query_but_drops_fragment() {
        assert_eq!(parse("http://h/a/b?x=1#frag").path(), "/a/b?x=1");
        assert_eq!(parse("http://h?x=1").path(), "/?x=1");
        assert_eq!(parse("http://h#frag").path(), "/");
    }

    #[test]
    fn scheme_is_case_insensitive() {
        assert_eq!(parse("HTTPS://example.com").scheme(), HttpScheme::Https);
    }

    #[test]
    fn parses_bracketed_ipv6() {
        let target = parse("http://[::1]:8080/x");

        assert_eq!(target.host(), "::1");
        assert_eq!(target.port(), 8080);
        assert_eq!(target.host_port(), "[::1]:8080");
        assert_eq!(target.to_string(), "http://[::1]:8080/x");
        assert_eq!(parse("https://[::1]").authority(), "[::1]");
    }

    #[test]
    fn empty_port_uses_scheme_default() {
        assert_eq!(parse("https://example.com:/").port(), 443);
    }

    #[test]
    fn from_socket_addr_requests_root_over_plain_http() {
        let target = HttpTarget::from_socket_addr("[::1]:8080".parse().unwrap());

        assert_eq!(target.to_string(), "http://[::1]:8080/");
    }

    #[test]
    fn rejects_invalid_urls() {
        for input in [
            "ftp://example.com",
            "https://",
            "https://:443",
            "https://user@example.com",
            "https://example.com:99999",
            "https://example.com:http",
            "http://[::1",
            "http://[::1]x",
            "http://::1",
        ] {
            assert!(input.parse::<HttpTarget>().is_err(), "{input}");
        }
    }
}
