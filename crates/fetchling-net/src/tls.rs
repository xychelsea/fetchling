//! rustls connector construction, certificate pinning, and a small HSTS store.

use std::path::Path;
use std::sync::Arc;

use fetchling_core::{Config, Error, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::Resumption;
use rustls::client::WebPkiServerVerifier;
use rustls::pki_types::{
    pem::PemObject, CertificateDer, CertificateRevocationListDer, PrivateKeyDer, ServerName,
    UnixTime,
};
use rustls::{
    ClientConfig, DigitallySignedStruct, Error as TlsError, RootCertStore, SignatureScheme,
};
use sha2::{Digest, Sha256};
use tokio_rustls::TlsConnector;

use rustls::crypto::CryptoProvider;

/// Build a rustls client connector with session resumption enabled.
///
/// Roots come from webpki-roots plus optional `ca_certificate` / `ca_directory`.
/// `secure_protocol` selects `auto`, `TLSv1_2`, or `TLSv1_3`. When
/// `check_certificate` is false, server certificates are not verified. Optional
/// `pinnedpubkey` (`sha256//…` or a key/cert file), `crl_file`, and client
/// certificates (`certificate` + `private_key`) are applied when set.
///
/// # Errors
///
/// Returns [`Error::Tls`] for unsupported protocol
/// versions, missing client-cert pairs, pin/CRL/CA parse failures, or rustls
/// configuration errors. Returns [`Error::Io`] when
/// a configured file cannot be read.
pub fn build_connector(cfg: &Config) -> Result<TlsConnector> {
    build_connector_resumable(cfg, true)
}

/// Build a rustls client connector, optionally disabling session resumption.
///
/// Same as [`build_connector`], but `resume: false` disables TLS session
/// resumption (needed for some FTPS data channels).
///
/// # Errors
///
/// Same as [`build_connector`].
pub fn build_connector_resumable(cfg: &Config, resume: bool) -> Result<TlsConnector> {
    ensure_crypto_provider();
    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    if let Some(ca) = &cfg.ca_certificate {
        load_ca_pem_file(&mut root_store, Path::new(ca))?;
    }
    if let Some(dir) = &cfg.ca_directory {
        load_ca_directory(&mut root_store, Path::new(dir))?;
    }

    let versions = protocol_versions(&cfg.secure_protocol)?;
    let builder = ClientConfig::builder_with_protocol_versions(versions);

    let crls = if let Some(path) = &cfg.crl_file {
        load_crl_file(Path::new(path))?
    } else {
        Vec::new()
    };

    let builder = if !cfg.check_certificate {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerifier))
    } else if let Some(pin) = &cfg.pinnedpubkey {
        let pins = parse_pinnedpubkey(pin)?;
        let inner = build_server_verifier(root_store, crls)?;
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinningVerifier { inner, pins }))
    } else if !crls.is_empty() {
        let verifier = build_server_verifier(root_store, crls)?;
        builder
            .dangerous()
            .with_custom_certificate_verifier(verifier)
    } else {
        builder.with_root_certificates(root_store)
    };

    let mut config = match (&cfg.certificate, &cfg.private_key) {
        (Some(cert), Some(key)) => {
            let certs = load_client_certs(Path::new(cert), &cfg.certificate_type)?;
            let key = load_private_key(Path::new(key), &cfg.private_key_type)?;
            builder
                .with_client_auth_cert(certs, key)
                .map_err(|e| Error::Tls(format!("client certificate: {e}")))?
        }
        (None, None) => builder.with_no_client_auth(),
        (Some(_), None) => {
            return Err(Error::Tls("--certificate requires --private-key".into()));
        }
        (None, Some(_)) => {
            return Err(Error::Tls("--private-key requires --certificate".into()));
        }
    };

    if !resume {
        config.resumption = Resumption::disabled();
    }

    Ok(TlsConnector::from(Arc::new(config)))
}

fn parse_pinnedpubkey(spec: &str) -> Result<Vec<[u8; 32]>> {
    let path = Path::new(spec);
    if path.is_file() {
        return Ok(vec![spki_sha256_from_file(path)?]);
    }
    let mut pins = Vec::new();
    for part in spec.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let b64 = part
            .strip_prefix("sha256//")
            .or_else(|| part.strip_prefix("SHA256//"))
            .ok_or_else(|| {
                Error::Tls(format!(
                    "bad --pinnedpubkey '{part}' (expected sha256//BASE64 or a key/cert file)"
                ))
            })?;
        let bytes = decode_b64(b64)
            .map_err(|_| Error::Tls(format!("bad --pinnedpubkey base64 in '{part}'")))?;
        if bytes.len() != 32 {
            return Err(Error::Tls(format!(
                "--pinnedpubkey digest must be 32 bytes, got {}",
                bytes.len()
            )));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        pins.push(arr);
    }
    if pins.is_empty() {
        return Err(Error::Tls("empty --pinnedpubkey".into()));
    }
    Ok(pins)
}

fn decode_b64(s: &str) -> std::result::Result<Vec<u8>, ()> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(s.trim()))
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(s.trim()))
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s.trim()))
        .map_err(|_| ())
}

fn spki_sha256_from_file(path: &Path) -> Result<[u8; 32]> {
    let data = std::fs::read(path)?;
    if let Ok(spki) = spki_from_cert_der(&data) {
        return Ok(sha256_32(&spki));
    }
    for cert in CertificateDer::pem_slice_iter(&data).flatten() {
        if let Ok(spki) = spki_from_cert_der(cert.as_ref()) {
            return Ok(sha256_32(&spki));
        }
    }
    if let Some(spki) = extract_spki_pem(&data) {
        return Ok(sha256_32(&spki));
    }
    if data.len() > 20 {
        return Ok(sha256_32(&data));
    }
    Err(Error::Tls(format!(
        "could not parse public key from {}",
        path.display()
    )))
}

fn extract_spki_pem(data: &[u8]) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(data).ok()?;
    let start = text.find("-----BEGIN PUBLIC KEY-----")?;
    let end = text.find("-----END PUBLIC KEY-----")?;
    let b64: String = text[start..end]
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect();
    decode_b64(&b64).ok()
}

fn spki_from_cert_der(der: &[u8]) -> Result<Vec<u8>> {
    let (_, cert) = x509_parser::parse_x509_certificate(der)
        .map_err(|e| Error::Tls(format!("parse certificate: {e}")))?;
    Ok(cert.public_key().raw.to_vec())
}

fn sha256_32(data: &[u8]) -> [u8; 32] {
    let d = Sha256::digest(data);
    let mut out = [0u8; 32];
    out.copy_from_slice(&d);
    out
}

fn leaf_spki_sha256(end_entity: &CertificateDer<'_>) -> std::result::Result<[u8; 32], TlsError> {
    let spki =
        spki_from_cert_der(end_entity.as_ref()).map_err(|e| TlsError::General(e.to_string()))?;
    Ok(sha256_32(&spki))
}

#[derive(Debug)]
struct PinningVerifier {
    inner: Arc<WebPkiServerVerifier>,
    pins: Vec<[u8; 32]>,
}

fn build_server_verifier(
    root_store: RootCertStore,
    crls: Vec<CertificateRevocationListDer<'static>>,
) -> Result<Arc<WebPkiServerVerifier>> {
    let builder = WebPkiServerVerifier::builder(Arc::new(root_store));
    let builder = if crls.is_empty() {
        builder
    } else {
        builder.with_crls(crls)
    };
    builder
        .build()
        .map_err(|e| Error::Tls(format!("TLS verifier: {e}")))
}

fn load_crl_file(path: &Path) -> Result<Vec<CertificateRevocationListDer<'static>>> {
    let data =
        std::fs::read(path).map_err(|e| Error::Tls(format!("CRL {}: {e}", path.display())))?;
    if data.is_empty() {
        return Err(Error::Tls(format!("CRL file is empty: {}", path.display())));
    }
    let looks_pem = data.windows(11).any(|w| w == b"-----BEGIN ");
    if looks_pem {
        let mut crls = Vec::new();
        for item in CertificateRevocationListDer::pem_slice_iter(&data) {
            let crl = item.map_err(|e| Error::Tls(format!("CRL {}: {e}", path.display())))?;
            crls.push(crl);
        }
        if crls.is_empty() {
            return Err(Error::Tls(format!(
                "CRL file contained no CRLs: {}",
                path.display()
            )));
        }
        return Ok(crls);
    }
    Ok(vec![CertificateRevocationListDer::from(data)])
}

impl ServerCertVerifier for PinningVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, TlsError> {
        self.inner.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        )?;
        let got = leaf_spki_sha256(end_entity)?;
        if self.pins.iter().any(|p| p == &got) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(TlsError::General(format!(
                "public key does not match --pinnedpubkey (got sha256//{})",
                base64_encode(&got)
            )))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn protocol_versions(name: &str) -> Result<&'static [&'static rustls::SupportedProtocolVersion]> {
    static TLS12_ONLY: &[&rustls::SupportedProtocolVersion] = &[&rustls::version::TLS12];
    static TLS13_ONLY: &[&rustls::SupportedProtocolVersion] = &[&rustls::version::TLS13];
    match name.to_ascii_lowercase().as_str() {
        "auto" | "" => Ok(rustls::DEFAULT_VERSIONS),
        "tlsv1_2" | "tls1_2" => Ok(TLS12_ONLY),
        "tlsv1_3" | "tls1_3" => Ok(TLS13_ONLY),
        other => Err(Error::Tls(format!(
            "unsupported --secure-protocol={other} (supported: auto, TLSv1_2, TLSv1_3)"
        ))),
    }
}

fn load_ca_pem_file(root_store: &mut rustls::RootCertStore, path: &Path) -> Result<()> {
    let pem = std::fs::read(path)?;
    for item in CertificateDer::pem_slice_iter(&pem) {
        let cert = item.map_err(|e| Error::Tls(format!("CA cert {}: {e}", path.display())))?;
        root_store
            .add(cert)
            .map_err(|e| Error::Tls(format!("add CA {}: {e}", path.display())))?;
    }
    Ok(())
}

fn load_ca_directory(root_store: &mut rustls::RootCertStore, dir: &Path) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| Error::Tls(format!("ca-directory {}: {e}", dir.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| Error::Tls(format!("ca-directory: {e}")))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !(name.ends_with(".pem") || name.ends_with(".crt") || name.ends_with(".cer")) {
            continue;
        }
        load_ca_pem_file(root_store, &path)?;
    }
    Ok(())
}

fn load_client_certs(path: &Path, cert_type: &str) -> Result<Vec<CertificateDer<'static>>> {
    let data = std::fs::read(path)?;
    let t = cert_type.to_ascii_lowercase();
    if t.is_empty() || t == "pem" {
        let mut certs = Vec::new();
        for item in CertificateDer::pem_slice_iter(&data) {
            let cert = item.map_err(|e| Error::Tls(format!("client cert: {e}")))?;
            certs.push(cert);
        }
        if certs.is_empty() {
            return Err(Error::Tls(
                "client certificate file contained no certificates".into(),
            ));
        }
        return Ok(certs);
    }
    if t == "der" || t == "asn1" {
        return Ok(vec![CertificateDer::from(data)]);
    }
    Err(Error::Tls(format!(
        "unsupported --certificate-type={cert_type} (supported: PEM, DER, ASN1)"
    )))
}

fn load_private_key(path: &Path, key_type: &str) -> Result<PrivateKeyDer<'static>> {
    let data = std::fs::read(path)?;
    let t = key_type.to_ascii_lowercase();
    if t.is_empty() || t == "pem" {
        return PrivateKeyDer::from_pem_slice(&data)
            .map_err(|e| Error::Tls(format!("private key: {e}")));
    }
    if t == "der" || t == "asn1" {
        return PrivateKeyDer::try_from(data)
            .map_err(|e| Error::Tls(format!("private key DER: {e}")));
    }
    Err(Error::Tls(format!(
        "unsupported --private-key-type={key_type} (supported: PEM, DER, ASN1)"
    )))
}

fn ensure_crypto_provider() {
    let _ = CryptoProvider::install_default(rustls::crypto::ring::default_provider());
}

#[derive(Debug)]
struct NoVerifier;

impl ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, TlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Host → (`includeSubDomains`, expiry unix) HSTS store.
///
/// Persistence uses tab-separated lines: `host\tinclude_subdomains\texpiry_unix`
/// (`include_subdomains` is `1` or `0`). [`HstsStore::learn`] parses
/// `Strict-Transport-Security`; `max-age=0` deletes the host.
/// `includeSubDomains` on a bare TLD (no `.` in the stored host) does not match
/// every `*.tld` name.
#[derive(Debug, Default, Clone)]
pub struct HstsStore {
    entries: std::collections::HashMap<String, (bool, i64)>,
}

impl HstsStore {
    /// Load a store from `path`, or an empty store if the file is missing.
    pub fn load(path: &str) -> Self {
        let mut store = Self::default();
        if let Ok(text) = std::fs::read_to_string(path) {
            for line in text.lines() {
                let parts: Vec<_> = line.split('\t').collect();
                if parts.len() >= 3 {
                    if let Ok(exp) = parts[2].parse() {
                        store
                            .entries
                            .insert(parts[0].to_string(), (parts[1] == "1", exp));
                    }
                }
            }
        }
        store
    }

    /// Whether `host` should be upgraded from HTTP to HTTPS.
    pub fn should_upgrade(&self, host: &str) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if let Some((_sub, exp)) = self.entries.get(host) {
            if *exp > now {
                return true;
            }
        }
        for (h, (include_sub, exp)) in &self.entries {
            if *include_sub && *exp > now {
                // Require a multi-label HSTS host so a bare TLD like "com"
                // cannot match every "*.com" name.
                if h.contains('.') && (host == h.as_str() || host.ends_with(&format!(".{h}"))) {
                    return true;
                }
            }
        }
        false
    }

    /// Record a `Strict-Transport-Security` header for `host`.
    ///
    /// `max-age=0` removes the host. Unparseable values are ignored.
    pub fn learn(&mut self, host: &str, header_value: &str) {
        let Some((max_age, include_sub)) = parse_sts_header(header_value) else {
            return;
        };
        if max_age == 0 {
            self.entries.remove(host);
            return;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let exp = now.saturating_add(max_age as i64);
        self.entries.insert(host.to_string(), (include_sub, exp));
    }

    /// Write the store to `path` in tab-separated form.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the file cannot be
    /// created or written.
    pub fn save(&self, path: &str) -> Result<()> {
        use std::io::Write;
        let mut f = std::fs::File::create(path)?;
        for (host, (include_sub, exp)) in &self.entries {
            writeln!(f, "{host}\t{}\t{exp}", if *include_sub { "1" } else { "0" })?;
        }
        Ok(())
    }
}

fn parse_sts_header(value: &str) -> Option<(u64, bool)> {
    let mut max_age: Option<u64> = None;
    let mut include_sub = false;
    for part in value.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (key, val) = match part.split_once('=') {
            Some((k, v)) => (k.trim(), Some(v.trim().trim_matches('"'))),
            None => (part, None),
        };
        if key.eq_ignore_ascii_case("max-age") {
            max_age = val?.parse().ok();
        } else if key.eq_ignore_ascii_case("includesubdomains") {
            include_sub = true;
        }
    }
    Some((max_age?, include_sub))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(host: &str, include_sub: bool, exp: i64) -> HstsStore {
        let mut s = HstsStore::default();
        s.entries.insert(host.to_string(), (include_sub, exp));
        s
    }

    #[test]
    fn hsts_exact_host_match() {
        let far = i64::MAX;
        let s = store_with("example.com", false, far);
        assert!(s.should_upgrade("example.com"));
        assert!(!s.should_upgrade("sub.example.com"));
    }

    #[test]
    fn hsts_subdomain_does_not_match_suffix_com() {
        let far = i64::MAX;
        let s = store_with("com", true, far);
        assert!(!s.should_upgrade("evil.com"));
        assert!(s.should_upgrade("com"));
    }

    #[test]
    fn hsts_include_subdomains() {
        let far = i64::MAX;
        let s = store_with("example.com", true, far);
        assert!(s.should_upgrade("example.com"));
        assert!(s.should_upgrade("sub.example.com"));
        assert!(!s.should_upgrade("evil-example.com"));
    }

    #[test]
    fn hsts_load_missing_expired_and_malformed() {
        let s = HstsStore::load("/no/such/fetchling-hsts-missing");
        assert!(!s.should_upgrade("example.com"));

        let s = store_with("example.com", false, 1);
        assert!(!s.should_upgrade("example.com"));

        let path =
            std::env::temp_dir().join(format!("fetchling-hsts-malformed-{}", std::process::id()));
        std::fs::write(&path, "not-a-valid-line\nexample.com\t0\tnotanumber\n").unwrap();
        let loaded = HstsStore::load(path.to_str().unwrap());
        assert!(!loaded.should_upgrade("example.com"));
        let _ = std::fs::remove_file(&path);

        let mut s = HstsStore::default();
        s.learn("example.com", "includeSubDomains");
        assert!(!s.should_upgrade("example.com"));
    }

    #[test]
    fn hsts_learn_and_save_roundtrip() {
        let mut s = HstsStore::default();
        s.learn("example.com", "max-age=100; includeSubDomains");
        assert!(s.should_upgrade("example.com"));
        assert!(s.should_upgrade("a.example.com"));
        s.learn("example.com", "max-age=0");
        assert!(!s.should_upgrade("example.com"));

        s.learn("foo.test", "max-age=9999");
        let path = std::env::temp_dir().join(format!("fetchling-hsts-{}", std::process::id()));
        s.save(path.to_str().unwrap()).unwrap();
        let loaded = HstsStore::load(path.to_str().unwrap());
        assert!(loaded.should_upgrade("foo.test"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn parse_sts_header_values() {
        assert_eq!(parse_sts_header("max-age=60"), Some((60, false)));
        assert_eq!(
            parse_sts_header("max-age=60; includeSubDomains"),
            Some((60, true))
        );
        assert_eq!(parse_sts_header("includeSubDomains"), None);
        assert_eq!(parse_sts_header("max-age=\"60\""), Some((60, false)));
        assert_eq!(
            parse_sts_header("MAX-AGE=60; IncludeSubDomains"),
            Some((60, true))
        );
    }

    #[test]
    fn protocol_versions_accepts_supported() {
        assert!(protocol_versions("auto").is_ok());
        assert!(protocol_versions("").is_ok());
        assert!(protocol_versions("TLSv1_2").is_ok());
        assert!(protocol_versions("TLS1_2").is_ok());
        assert!(protocol_versions("TLSv1_3").is_ok());
        assert!(protocol_versions("TLS1_3").is_ok());
        assert!(protocol_versions("SSLv3").is_err());
        assert!(protocol_versions("TLSv1").is_err());
        assert!(protocol_versions("PFS").is_err());
    }

    #[test]
    fn load_ca_directory_reads_pem_files() {
        let dir = std::env::temp_dir().join(format!("fetchling-ca-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Empty PEM-looking file with no certs should still be attempted; use webpki isn't needed.
        // Just verify non-.pem files are skipped and missing dir errors.
        std::fs::write(dir.join("skip.txt"), b"not a cert").unwrap();
        let mut store = rustls::RootCertStore::empty();
        load_ca_directory(&mut store, &dir).unwrap();
        assert_eq!(store.len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
        let err = load_ca_directory(&mut store, Path::new("/no/such/ca/dir")).unwrap_err();
        assert!(err.to_string().contains("ca-directory"));
    }

    #[test]
    fn parse_pinnedpubkey_sha256() {
        let zeros = [0u8; 32];
        let b64 = base64_encode(&zeros);
        let pins = parse_pinnedpubkey(&format!("sha256//{b64}")).unwrap();
        assert_eq!(pins.len(), 1);
        assert_eq!(pins[0], zeros);
        let pins = parse_pinnedpubkey(&format!("SHA256//{b64}")).unwrap();
        assert_eq!(pins.len(), 1);
        assert_eq!(pins[0], zeros);
        let pins = parse_pinnedpubkey(&format!("sha256//{b64};sha256//{b64}")).unwrap();
        assert_eq!(pins.len(), 2);
        assert!(parse_pinnedpubkey("not-a-pin").is_err());
        assert!(parse_pinnedpubkey("").is_err());
        assert!(parse_pinnedpubkey(";;;").is_err());
        assert!(parse_pinnedpubkey("sha256//QQ==").is_err());
    }

    #[test]
    fn build_connector_config_branches() {
        let cfg = Config {
            check_certificate: false,
            ..Config::default()
        };
        build_connector(&cfg).unwrap();

        let cfg = Config {
            secure_protocol: "TLSv1_3".into(),
            ..Config::default()
        };
        build_connector(&cfg).unwrap();
        build_connector_resumable(&cfg, false).unwrap();

        let cfg = Config {
            certificate: Some("client.pem".into()),
            private_key: None,
            ..Config::default()
        };
        assert!(matches!(build_connector(&cfg), Err(Error::Tls(_))));
        let cfg = Config {
            certificate: None,
            private_key: Some("key.pem".into()),
            ..Config::default()
        };
        assert!(matches!(build_connector(&cfg), Err(Error::Tls(_))));
    }

    #[test]
    fn load_client_certs_and_key_reject_unsupported_types() {
        let path =
            std::env::temp_dir().join(format!("fetchling-net-keytype-{}", std::process::id()));
        std::fs::write(&path, b"dummy").unwrap();
        let err = load_client_certs(&path, "foo").unwrap_err();
        assert!(matches!(err, Error::Tls(_)));
        let err = load_private_key(&path, "foo").unwrap_err();
        assert!(matches!(err, Error::Tls(_)));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_crl_file_missing_and_empty() {
        let err = load_crl_file(Path::new("/no/such/crl.pem")).unwrap_err();
        assert!(err.to_string().contains("CRL"));
        let path = std::env::temp_dir().join(format!("fetchling-empty-crl-{}", std::process::id()));
        std::fs::write(&path, b"").unwrap();
        let err = load_crl_file(&path).unwrap_err();
        assert!(err.to_string().contains("empty"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_crl_file_pem_and_build_connector() {
        let crls = load_crl_file(Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/testdata/crl.pem"
        )))
        .unwrap();
        assert_eq!(crls.len(), 1);
        let mut cfg = Config {
            crl_file: Some(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/crl.pem").into()),
            ..Config::default()
        };
        build_connector(&cfg).unwrap();
        let zeros = [0u8; 32];
        cfg.pinnedpubkey = Some(format!("sha256//{}", base64_encode(&zeros)));
        build_connector(&cfg).unwrap();
    }
}
