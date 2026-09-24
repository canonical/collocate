use base64::Engine;
use collocate_core::auth::{valid_projects, Caller, Role};
use collocate_core::{Error, Result};
use rcgen::{CertificateParams, DnType, ExtendedKeyUsagePurpose, KeyPair, PKCS_ECDSA_P384_SHA384};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::CertificateDer;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub const SERVER_CERT: &str = "server.crt";
pub const SERVER_KEY: &str = "server.key";
const CERTIFICATES: &str = "certificates.json";
const TOKENS: &str = "tokens.json";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn fingerprint_der(der: &[u8]) -> String {
    hex(&Sha256::digest(der))
}

pub fn certificate_der(pem: &str) -> Result<Vec<u8>> {
    CertificateDer::from_pem_slice(pem.as_bytes())
        .map(|c| c.as_ref().to_vec())
        .map_err(|e| Error::Invalid(format!("not a PEM certificate: {e}")))
}

pub fn fingerprint_pem(pem: &str) -> Result<String> {
    Ok(fingerprint_der(&certificate_der(pem)?))
}

pub fn der_to_pem(der: &[u8]) -> String {
    let body = base64::engine::general_purpose::STANDARD.encode(der);
    let mut out = String::from("-----BEGIN CERTIFICATE-----\n");
    for chunk in body.as_bytes().chunks(64) {
        out.push_str(&String::from_utf8_lossy(chunk));
        out.push('\n');
    }
    out.push_str("-----END CERTIFICATE-----\n");
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub certificate: String,
    pub key: String,
    pub fingerprint: String,
}

fn generate(common_name: &str, names: &[String], purpose: ExtendedKeyUsagePurpose) -> Result<Identity> {
    let internal = |e: rcgen::Error| Error::Internal(format!("certificate generation: {e}"));
    let mut params = CertificateParams::new(names.to_vec()).map_err(internal)?;
    params.distinguished_name.push(DnType::CommonName, common_name);
    params.distinguished_name.push(DnType::OrganizationName, "collocate");
    params.extended_key_usages = vec![purpose];
    params.not_before = rcgen::date_time_ymd(2024, 1, 1);
    params.not_after = rcgen::date_time_ymd(2124, 1, 1);
    let key = KeyPair::generate_for(&PKCS_ECDSA_P384_SHA384).map_err(internal)?;
    let cert = params.self_signed(&key).map_err(internal)?;
    Ok(Identity { certificate: cert.pem(), key: key.serialize_pem(), fingerprint: fingerprint_der(cert.der()) })
}

pub fn generate_server(names: &[String]) -> Result<Identity> {
    let mut all = vec!["collocate".to_string(), "localhost".to_string()];
    for n in names {
        if !all.contains(n) {
            all.push(n.clone());
        }
    }
    generate("collocate", &all, ExtendedKeyUsagePurpose::ServerAuth)
}

pub fn generate_client(name: &str) -> Result<Identity> {
    generate(name, &[], ExtendedKeyUsagePurpose::ClientAuth)
}

fn write_private(path: &Path, content: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, content)?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn save_identity(dir: &Path, cert_file: &str, key_file: &str, id: &Identity) -> Result<()> {
    write_private(&dir.join(key_file), id.key.as_bytes())?;
    write_private(&dir.join(cert_file), id.certificate.as_bytes())?;
    fs::set_permissions(dir.join(cert_file), fs::Permissions::from_mode(0o644))?;
    Ok(())
}

pub fn load_identity(dir: &Path, cert_file: &str, key_file: &str) -> Result<Option<Identity>> {
    let cert = match fs::read_to_string(dir.join(cert_file)) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let key = fs::read_to_string(dir.join(key_file))?;
    Ok(Some(Identity { fingerprint: fingerprint_pem(&cert)?, certificate: cert, key }))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Token {
    pub client_name: String,
    pub fingerprint: String,
    pub addresses: Vec<String>,
    pub secret: String,
    #[serde(default)]
    pub expires_at: Option<u64>,
    #[serde(default)]
    pub role: Role,
    #[serde(default)]
    pub projects: Vec<String>,
}

impl Token {
    pub fn encode(&self) -> Result<String> {
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(self)?))
    }

    pub fn decode(text: &str) -> Result<Token> {
        let t = text.trim();
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(t.trim_end_matches('='))
            .or_else(|_| base64::engine::general_purpose::STANDARD.decode(t))
            .map_err(|_| Error::Invalid("the trust token is not valid base64".into()))?;
        let token: Token = serde_json::from_slice(&bytes).map_err(|_| Error::Invalid("the trust token is malformed".into()))?;
        if token.secret.is_empty() || token.fingerprint.len() != 64 || token.addresses.is_empty() {
            return Err(Error::Invalid("the trust token is incomplete".into()));
        }
        Ok(token)
    }
}

pub fn hash_secret(secret: &str) -> String {
    hex(&Sha256::digest(secret.as_bytes()))
}

pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedCertificate {
    pub name: String,
    pub fingerprint: String,
    pub role: Role,
    #[serde(default)]
    pub projects: Vec<String>,
    pub certificate: String,
    pub added_at: u64,
}

impl TrustedCertificate {
    pub fn caller(&self) -> Caller {
        Caller { name: self.name.clone(), fingerprint: self.fingerprint.clone(), role: self.role, projects: self.projects.clone() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingToken {
    pub name: String,
    pub secret_hash: String,
    pub role: Role,
    #[serde(default)]
    pub projects: Vec<String>,
    pub created_at: u64,
    #[serde(default)]
    pub expires_at: Option<u64>,
}

impl PendingToken {
    pub fn expired(&self, now: u64) -> bool {
        self.expires_at.is_some_and(|e| now >= e)
    }
}

fn valid_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 64 || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@')) {
        return Err(Error::Invalid(format!("trust name {name:?} must be 1-64 letters, digits or '-', '_', '.', '@'")));
    }
    Ok(())
}

pub struct TrustStore {
    dir: PathBuf,
}

impl TrustStore {
    pub fn open(dir: impl AsRef<Path>) -> Result<TrustStore> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        Ok(TrustStore { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn read<T: serde::de::DeserializeOwned + Default>(&self, file: &str) -> Result<T> {
        match fs::read(self.dir.join(file)) {
            Ok(b) => Ok(serde_json::from_slice(&b)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
            Err(e) => Err(e.into()),
        }
    }

    fn write<T: Serialize>(&self, file: &str, value: &T) -> Result<()> {
        write_private(&self.dir.join(file), &serde_json::to_vec_pretty(value)?)
    }

    pub fn server_identity(&self) -> Result<Option<Identity>> {
        load_identity(&self.dir, SERVER_CERT, SERVER_KEY)
    }

    pub fn ensure_server_identity(&self, names: &[String]) -> Result<Identity> {
        if let Some(id) = self.server_identity()? {
            return Ok(id);
        }
        let id = generate_server(names)?;
        save_identity(&self.dir, SERVER_CERT, SERVER_KEY, &id)?;
        Ok(id)
    }

    pub fn certificates(&self) -> Result<Vec<TrustedCertificate>> {
        self.read(CERTIFICATES)
    }

    pub fn tokens(&self) -> Result<Vec<PendingToken>> {
        self.read(TOKENS)
    }

    fn name_taken(&self, name: &str) -> Result<bool> {
        Ok(self.certificates()?.iter().any(|c| c.name == name) || self.tokens()?.iter().any(|t| t.name == name))
    }

    pub fn create_token(
        &self,
        name: &str,
        role: Role,
        projects: &[String],
        expiry_secs: Option<u64>,
        now: u64,
    ) -> Result<(String, PendingToken)> {
        valid_name(name)?;
        valid_projects(projects)?;
        if self.name_taken(name)? {
            return Err(Error::Conflict(format!("a trusted client or pending token is already named {name}")));
        }
        let secret = collocate_core::id::random_hex(32)?;
        let pending = PendingToken {
            name: name.to_string(),
            secret_hash: hash_secret(&secret),
            role,
            projects: projects.to_vec(),
            created_at: now,
            expires_at: expiry_secs.filter(|s| *s > 0).map(|s| now + s),
        };
        let mut tokens = self.tokens()?;
        tokens.push(pending.clone());
        self.write(TOKENS, &tokens)?;
        Ok((secret, pending))
    }

    pub fn revoke_token(&self, name: &str) -> Result<()> {
        let mut tokens = self.tokens()?;
        let before = tokens.len();
        tokens.retain(|t| t.name != name);
        if tokens.len() == before {
            return Err(Error::NotFound(format!("pending token {name}")));
        }
        self.write(TOKENS, &tokens)
    }

    pub fn add_certificate(&self, name: &str, pem: &str, role: Role, projects: &[String], now: u64) -> Result<TrustedCertificate> {
        valid_name(name)?;
        valid_projects(projects)?;
        let fingerprint = fingerprint_pem(pem)?;
        let mut certs = self.certificates()?;
        if certs.iter().any(|c| c.fingerprint == fingerprint) {
            return Err(Error::Conflict("this certificate is already trusted".into()));
        }
        if self.name_taken(name)? {
            return Err(Error::Conflict(format!("a trusted client or pending token is already named {name}")));
        }
        let der = certificate_der(pem)?;
        let cert = TrustedCertificate {
            name: name.to_string(),
            fingerprint,
            role,
            projects: projects.to_vec(),
            certificate: der_to_pem(&der),
            added_at: now,
        };
        certs.push(cert.clone());
        self.write(CERTIFICATES, &certs)?;
        Ok(cert)
    }

    pub fn enroll(&self, secret: &str, pem: &str, name: Option<&str>, now: u64) -> Result<TrustedCertificate> {
        let hash = hash_secret(secret);
        let mut tokens = self.tokens()?;
        let idx = tokens
            .iter()
            .position(|t| constant_time_eq(&t.secret_hash, &hash))
            .ok_or_else(|| Error::Forbidden("the trust token is unknown or was already used".into()))?;
        let token = tokens.remove(idx);
        if token.expired(now) {
            self.write(TOKENS, &tokens)?;
            return Err(Error::Forbidden("the trust token has expired".into()));
        }
        let fingerprint = fingerprint_pem(pem)?;
        let mut certs = self.certificates()?;
        if certs.iter().any(|c| c.fingerprint == fingerprint) {
            return Err(Error::Conflict("this certificate is already trusted".into()));
        }
        let name = name.filter(|n| !n.is_empty()).map(String::from).unwrap_or_else(|| token.name.clone());
        valid_name(&name)?;
        if certs.iter().any(|c| c.name == name) {
            return Err(Error::Conflict(format!("a trusted client is already named {name}")));
        }
        let der = certificate_der(pem)?;
        let cert = TrustedCertificate {
            name,
            fingerprint,
            role: token.role,
            projects: token.projects.clone(),
            certificate: der_to_pem(&der),
            added_at: now,
        };
        certs.push(cert.clone());
        self.write(CERTIFICATES, &certs)?;
        self.write(TOKENS, &tokens)?;
        Ok(cert)
    }

    pub fn lookup(&self, fingerprint: &str) -> Result<Option<TrustedCertificate>> {
        Ok(self.certificates()?.into_iter().find(|c| c.fingerprint == fingerprint))
    }

    pub fn remove(&self, name_or_fingerprint: &str) -> Result<TrustedCertificate> {
        let mut certs = self.certificates()?;
        let key = name_or_fingerprint.to_ascii_lowercase();
        let matches: Vec<usize> = certs
            .iter()
            .enumerate()
            .filter(|(_, c)| c.name == name_or_fingerprint || (key.len() >= 12 && c.fingerprint.starts_with(&key)))
            .map(|(i, _)| i)
            .collect();
        match matches.as_slice() {
            [] => Err(Error::NotFound(format!("trusted client {name_or_fingerprint}"))),
            [i] => {
                let removed = certs.remove(*i);
                self.write(CERTIFICATES, &certs)?;
                Ok(removed)
            }
            _ => Err(Error::Ambiguous(matches.iter().map(|i| certs[*i].name.clone()).collect())),
        }
    }
}
