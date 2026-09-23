use crate::auth::{parse_bearer_challenge, Credentials};
use crate::error::{Error, Result};
use crate::manifest::{Index, Manifest, ACCEPT_MANIFEST_TYPES, DOCKER_MANIFEST_LIST, OCI_INDEX};
use crate::reference::Selector;
use sha2::{Digest as _, Sha256};
use std::cell::RefCell;
use std::io::{Read, Write};

pub struct Client {
    agent: ureq::Agent,
    registry: String,
    scheme: &'static str,
    token: RefCell<Option<String>>,
}

fn scheme_for(registry: &str) -> &'static str {
    let host = registry.split(':').next().unwrap_or(registry);
    if host == "localhost" || host == "127.0.0.1" {
        "http"
    } else {
        "https"
    }
}

impl Client {
    pub fn new(registry: &str) -> Client {
        Client { agent: ureq::AgentBuilder::new().build(), registry: registry.to_string(), scheme: scheme_for(registry), token: RefCell::new(None) }
    }

    fn url(&self, repo: &str, kind: &str, id: &str) -> String {
        format!("{}://{}/v2/{repo}/{kind}/{id}", self.scheme, self.registry)
    }

    fn authenticate(&self, www_authenticate: &str, creds: &dyn Credentials) -> Result<()> {
        let challenge = parse_bearer_challenge(www_authenticate)?;
        let mut req = self.agent.get(&challenge.realm);
        if let Some(service) = &challenge.service {
            req = req.query("service", service);
        }
        if let Some(scope) = &challenge.scope {
            req = req.query("scope", scope);
        }
        let response = match creds.for_registry(&self.registry) {
            Some((user, pass)) => req.set("Authorization", &basic_auth(&user, &pass)).call(),
            None => req.call(),
        }
        .map_err(|e| Error::Auth(e.to_string()))?;
        let body: serde_json::Value = response.into_json().map_err(|e| Error::Auth(e.to_string()))?;
        let token = body
            .get("token")
            .or_else(|| body.get("access_token"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Auth("token response missing token field".into()))?;
        *self.token.borrow_mut() = Some(token.to_string());
        Ok(())
    }

    fn request(&self, method: &str, url: &str, accept: Option<&str>, creds: &dyn Credentials) -> Result<ureq::Response> {
        let build = |token: &Option<String>| {
            let mut req = self.agent.request(method, url);
            if let Some(accept) = accept {
                req = req.set("Accept", accept);
            }
            if let Some(t) = token {
                req = req.set("Authorization", &format!("Bearer {t}"));
            }
            req
        };
        let first = build(&self.token.borrow()).call();
        match first {
            Ok(resp) => Ok(resp),
            Err(ureq::Error::Status(401, resp)) => {
                let challenge = resp.header("WWW-Authenticate").ok_or_else(|| Error::Auth("401 without WWW-Authenticate".into()))?.to_string();
                self.authenticate(&challenge, creds)?;
                build(&self.token.borrow()).call().map_err(|e| Error::Http(e.to_string()))
            }
            Err(e) => Err(Error::Http(e.to_string())),
        }
    }

    pub fn resolve_manifest(&self, repo: &str, selector: &Selector, creds: &dyn Credentials) -> Result<(String, Manifest)> {
        let id = match selector {
            Selector::Tag(t) => t.clone(),
            Selector::Digest(d) => d.clone(),
        };
        let url = self.url(repo, "manifests", &id);
        let resp = self.request("GET", &url, Some(ACCEPT_MANIFEST_TYPES), creds)?;
        let content_type = resp.header("Content-Type").unwrap_or("").to_string();
        let mut bytes = Vec::new();
        resp.into_reader().read_to_end(&mut bytes)?;

        if content_type.starts_with(DOCKER_MANIFEST_LIST) || content_type.starts_with(OCI_INDEX) {
            let index: Index = serde_json::from_slice(&bytes)?;
            let desc = crate::manifest::select_platform(&index, "linux", crate::manifest::host_arch())
                .ok_or_else(|| Error::NoMatchingPlatform { os: "linux".into(), arch: crate::manifest::host_arch().into() })?;
            return self.fetch_manifest_by_digest(repo, &desc.digest, creds);
        }

        let digest = digest_of(&bytes);
        let manifest: Manifest = serde_json::from_slice(&bytes)?;
        Ok((digest, manifest))
    }

    fn fetch_manifest_by_digest(&self, repo: &str, digest: &str, creds: &dyn Credentials) -> Result<(String, Manifest)> {
        let url = self.url(repo, "manifests", digest);
        let resp = self.request("GET", &url, Some(ACCEPT_MANIFEST_TYPES), creds)?;
        let mut bytes = Vec::new();
        resp.into_reader().read_to_end(&mut bytes)?;
        let manifest: Manifest = serde_json::from_slice(&bytes)?;
        Ok((digest.to_string(), manifest))
    }

    pub fn get_blob(&self, repo: &str, digest: &str, out: &mut dyn Write, creds: &dyn Credentials) -> Result<()> {
        let url = self.url(repo, "blobs", digest);
        let resp = self.request("GET", &url, None, creds)?;
        let mut reader = resp.into_reader();
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            out.write_all(&buf[..n])?;
        }
        let actual = format!("sha256:{:x}", hasher.finalize());
        if actual != digest {
            return Err(Error::DigestMismatch { expected: digest.to_string(), actual });
        }
        Ok(())
    }
}

fn digest_of(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn basic_auth(user: &str, pass: &str) -> String {
    use std::fmt::Write as _;
    let mut encoded = String::new();
    let _ = write!(encoded, "{user}:{pass}");
    format!("Basic {}", base64(encoded.as_bytes()))
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        out.push(B64[(b0 >> 2) as usize] as char);
        out.push(B64[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 { B64[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[(b2 & 0x3f) as usize] as char } else { '=' });
    }
    out
}

