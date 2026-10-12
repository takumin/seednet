//! The URL template of `seednet.url=`: level 1 of RFC 6570, with the names that DHCP gives the
//! host.

use std::fmt;

use anyhow::{Result, bail};

/// A variable of the template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Var {
    /// `{hostname}`: the host name, without the domain.
    Hostname,
    /// `{domainname}`: the domain.
    Domainname,
    /// `{fqdn}`: the host name with the domain.
    Fqdn,
}

impl Var {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "hostname" => Some(Self::Hostname),
            "domainname" => Some(Self::Domainname),
            "fqdn" => Some(Self::Fqdn),
            _ => None,
        }
    }
}

impl fmt::Display for Var {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Hostname => "{hostname}",
            Self::Domainname => "{domainname}",
            Self::Fqdn => "{fqdn}",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Literal(String),
    Var(Var),
}

/// A URL with variables in it, as `http://boot.example/seed/{hostname}/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    parts: Vec<Part>,
}

/// The names of the host, as DHCP gives them, from which the variables expand.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Names {
    pub hostname: Option<String>,
    pub domainname: Option<String>,
}

impl Names {
    /// The names from a host name and a domain as DHCP gives them: a host name with dots is split
    /// at the first, its rest being the domain unless DHCP gives one of its own.
    pub fn from_dhcp(hostname: &str, domainname: Option<&str>) -> Self {
        let hostname = hostname.trim().trim_end_matches('.');
        let domainname = domainname
            .map(|domain| domain.trim().trim_end_matches('.'))
            .filter(|domain| !domain.is_empty());
        let (hostname, rest) = match hostname.split_once('.') {
            Some((hostname, rest)) => (hostname, Some(rest)),
            None => (hostname, None),
        };
        Self {
            hostname: Some(hostname.to_owned()).filter(|hostname| !hostname.is_empty()),
            domainname: domainname.or(rest).map(str::to_owned),
        }
    }

    fn get(&self, var: Var) -> Option<String> {
        match var {
            Var::Hostname => self.hostname.clone(),
            Var::Domainname => self.domainname.clone(),
            Var::Fqdn => Some(format!("{}.{}", self.hostname.as_ref()?, self.domainname.as_ref()?)),
        }
    }
}

impl Template {
    /// Parses the template. Only the simple expansion of level 1 is taken, as `{hostname}`, and
    /// only of the variables there are: an operator, such as the `+` of `{+hostname}`, or another
    /// variable fails, as do braces that do not pair.
    pub fn parse(url: &str) -> Result<Self> {
        let mut parts = Vec::new();
        let mut rest = url;
        while let Some(start) = rest.find(['{', '}']) {
            if rest[start..].starts_with('}') {
                bail!("a }} without a {{ in {url}");
            }
            let Some(len) = rest[start + 1..].find(['{', '}']) else {
                bail!("a {{ without a }} in {url}");
            };
            let end = start + 1 + len;
            if rest[end..].starts_with('{') {
                bail!("a {{ without a }} in {url}");
            }
            if start > 0 {
                parts.push(Part::Literal(rest[..start].to_owned()));
            }
            let name = &rest[start + 1..end];
            let Some(var) = Var::parse(name) else {
                bail!("{{{name}}} in {url} is none of {{hostname}}, {{domainname}} and {{fqdn}}");
            };
            parts.push(Part::Var(var));
            rest = &rest[end + 1..];
        }
        if !rest.is_empty() {
            parts.push(Part::Literal(rest.to_owned()));
        }
        Ok(Self { parts })
    }

    /// The variables in the template, each once.
    pub fn vars(&self) -> Vec<Var> {
        let mut vars = Vec::new();
        for part in &self.parts {
            if let Part::Var(var) = part
                && !vars.contains(var)
            {
                vars.push(*var);
            }
        }
        vars
    }

    /// The URL with the variables expanded, each value percent-encoded but for the unreserved
    /// characters, as RFC 6570 does. Fails with the variables that have no value.
    pub fn expand(&self, names: &Names) -> Result<String, Vec<Var>> {
        let mut url = String::new();
        let mut missing = Vec::new();
        for part in &self.parts {
            match part {
                Part::Literal(literal) => url.push_str(literal),
                Part::Var(var) => match names.get(*var) {
                    Some(value) => encode(&value, &mut url),
                    None if !missing.contains(var) => missing.push(*var),
                    None => {}
                },
            }
        }
        if missing.is_empty() {
            Ok(url)
        } else {
            Err(missing)
        }
    }
}

/// Percent-encodes all but the unreserved characters of RFC 3986.
fn encode(value: &str, out: &mut String) {
    use fmt::Write as _;

    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(hostname: &str, domainname: Option<&str>) -> Names {
        Names::from_dhcp(hostname, domainname)
    }

    #[test]
    fn expands_the_names() {
        let template = Template::parse("http://a/{hostname}/{domainname}/{fqdn}/").unwrap();
        assert_eq!(
            template
                .expand(&names("host", Some("example.com")))
                .unwrap(),
            "http://a/host/example.com/host.example.com/"
        );
        assert_eq!(template.vars(), [Var::Hostname, Var::Domainname, Var::Fqdn]);
    }

    #[test]
    fn a_url_without_variables_stays() {
        let template = Template::parse("http://a/b?c=d").unwrap();
        assert!(template.vars().is_empty());
        assert_eq!(template.expand(&Names::default()).unwrap(), "http://a/b?c=d");
    }

    #[test]
    fn splits_a_host_name_with_dots() {
        assert_eq!(
            names("host.example.com.", None),
            Names {
                hostname: Some("host".to_owned()),
                domainname: Some("example.com".to_owned()),
            }
        );
        // The domain of DHCP wins over the rest of the host name.
        assert_eq!(
            names("host.example.com", Some("example.org"))
                .domainname
                .as_deref(),
            Some("example.org")
        );
        assert_eq!(names("", Some("")), Names::default());
    }

    #[test]
    fn tells_what_has_no_value() {
        let template = Template::parse("http://a/{fqdn}/{hostname}/{fqdn}").unwrap();
        assert_eq!(template.expand(&names("host", None)), Err(vec![Var::Fqdn]));
        assert_eq!(template.expand(&Names::default()), Err(vec![Var::Fqdn, Var::Hostname]));
    }

    #[test]
    fn percent_encodes_the_values() {
        let template = Template::parse("http://a/{hostname}").unwrap();
        assert_eq!(
            template.expand(&names("a/b?c d~é", None)).unwrap(),
            "http://a/a%2Fb%3Fc%20d~%C3%A9"
        );
    }

    #[test]
    fn rejects_what_level_1_does_not_have() {
        for url in [
            "http://a/{+hostname}",
            "http://a/{hostname,fqdn}",
            "http://a/{host}",
            "http://a/{}",
            "http://a/{hostname",
            "http://a/hostname}",
            "http://a/{{hostname}}",
            "http://a/{ hostname }",
        ] {
            assert!(Template::parse(url).is_err(), "{url}");
        }
    }
}
