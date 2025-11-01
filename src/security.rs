//! Security utilities covering HTTPS enforcement, domain allowlists, and SPKI pinning.
//!
//! # Threat Model
//! These helpers assume upstream TLS validation has already succeeded and focus on defending the
//! cache pipeline against downgrade attempts (HTTP redirects), host header confusion, and
//! certificate substitution by validating SPKI fingerprints.

// std
use std::{
	collections::HashSet,
	fmt::{Debug, Formatter, Result as FmtResult},
};
// crates.io
use base64::prelude::*;
use serde::{Deserialize, Serialize, de::Deserializer};
use sha2::{Digest, Sha256};
use url::Url;
// self
use crate::_prelude::*;

/// SHA-256 fingerprint of a Subject Public Key Info (SPKI) structure.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SpkiFingerprint {
	bytes: Arc<[u8; 32]>,
}
impl SpkiFingerprint {
	/// Decode a base64(fp) value into a fingerprint.
	pub fn from_b64(value: &str) -> Result<Self> {
		let cleaned = value.trim();
		let decoded = BASE64_STANDARD
			.decode(cleaned)
			.or_else(|_| BASE64_URL_SAFE_NO_PAD.decode(cleaned))
			.map_err(|err| Error::Validation {
				field: "pinned_spki",
				reason: format!("Invalid base64 fingerprint: {err}."),
			})?;

		if decoded.len() != 32 {
			return Err(Error::Validation {
				field: "pinned_spki",
				reason: "Fingerprint must decode to 32 bytes (SHA-256).".into(),
			});
		}

		let mut bytes = [0u8; 32];

		bytes.copy_from_slice(&decoded);

		Ok(Self { bytes: Arc::new(bytes) })
	}

	/// Raw fingerprint bytes.
	pub fn as_bytes(&self) -> &[u8; 32] {
		self.bytes.as_ref()
	}
}
impl Debug for SpkiFingerprint {
	fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
		write!(f, "SpkiFingerprint({})", BASE64_STANDARD.encode(self.bytes.as_ref()))
	}
}
impl TryFrom<String> for SpkiFingerprint {
	type Error = Error;

	fn try_from(value: String) -> Result<Self> {
		Self::from_b64(&value)
	}
}
impl From<SpkiFingerprint> for String {
	fn from(value: SpkiFingerprint) -> Self {
		BASE64_STANDARD.encode(value.bytes.as_ref())
	}
}

/// Canonicalise a DNS name by trimming whitespace, removing any trailing dot, and lowercasing.
pub fn canonicalize_dns_name(value: &str) -> Option<String> {
	let trimmed = value.trim();
	if trimmed.is_empty() {
		return None;
	}

	let without_trailing_dot = trimmed.trim_end_matches('.');
	if without_trailing_dot.is_empty() {
		return None;
	}

	Some(without_trailing_dot.to_ascii_lowercase())
}

/// Normalise an allowlist by canonicalising entries and removing duplicates/empties.
pub fn normalize_allowlist(domains: Vec<String>) -> Vec<String> {
	let mut seen = HashSet::new();
	let mut normalized = Vec::with_capacity(domains.len());

	for domain in domains {
		if let Some(canonical) = canonicalize_dns_name(&domain)
			&& seen.insert(canonical.clone())
		{
			normalized.push(canonical);
		}
	}

	normalized
}

/// `serde` helper to normalise allowlist domains during deserialisation.
pub fn deserialize_allowed_domains<'de, D>(
	deserializer: D,
) -> std::result::Result<Vec<String>, D::Error>
where
	D: Deserializer<'de>,
{
	let raw = Vec::<String>::deserialize(deserializer)?;
	Ok(normalize_allowlist(raw))
}

/// Ensure the provided URL uses HTTPS.
pub fn enforce_https(url: &Url) -> Result<()> {
	if url.scheme() == "https" {
		Ok(())
	} else {
		Err(Error::Security(format!("Upstream URL {url} must use HTTPS.")))
	}
}

#[inline]
fn matches_allowlist(host: &str, domain: &str) -> bool {
	if host == domain {
		return true;
	}

	host.strip_suffix(domain).and_then(|prefix| prefix.strip_suffix('.')).is_some()
}

fn is_canonical_allowlist_entry(domain: &str) -> bool {
	!domain.is_empty()
		&& !domain.ends_with('.')
		&& domain.trim().len() == domain.len()
		&& !domain.chars().any(|c| c.is_ascii_uppercase())
}

/// Evaluate whether the given hostname is allowed by the provided suffix allowlist.
///
/// When the list is empty, all hosts are considered valid.
pub fn host_is_allowed(host: &str, allowed_domains: &[String]) -> bool {
	if allowed_domains.is_empty() {
		return true;
	}

	let Some(host) = canonicalize_dns_name(host) else {
		return false;
	};

	allowed_domains.iter().any(|domain| {
		if is_canonical_allowlist_entry(domain) {
			matches_allowlist(&host, domain)
		} else if let Some(canonical) = canonicalize_dns_name(domain) {
			matches_allowlist(&host, &canonical)
		} else {
			false
		}
	})
}

/// Compute the SHA-256 fingerprint of a DER-encoded SPKI payload.
pub fn fingerprint_spki(spki_der: &[u8]) -> [u8; 32] {
	let digest = Sha256::digest(spki_der);
	let mut bytes = [0u8; 32];

	bytes.copy_from_slice(&digest);

	bytes
}

/// Validate that at least one configured SPKI fingerprint matches the presented SPKI set.
///
/// The iterator should provide DER-encoded SPKI payloads extracted from the TLS peer certificates.
pub fn verify_spki_pins<'a, I>(present_spki: I, pins: &[SpkiFingerprint]) -> Result<()>
where
	I: IntoIterator<Item = &'a [u8]>,
{
	if pins.is_empty() {
		return Ok(());
	}

	let mut presented_fingerprints = Vec::new();

	for spki in present_spki {
		let fingerprint = fingerprint_spki(spki);
		if pins.iter().any(|pin| pin.as_bytes() == &fingerprint) {
			return Ok(());
		}
		if tracing::enabled!(tracing::Level::WARN) {
			presented_fingerprints.push(BASE64_STANDARD.encode(fingerprint));
		}
	}

	if tracing::enabled!(tracing::Level::WARN) {
		let expected: Vec<String> =
			pins.iter().map(|pin| BASE64_STANDARD.encode(pin.as_bytes())).collect();
		tracing::warn!(
			expected = ?expected,
			presented = ?presented_fingerprints,
			"SPKI pin verification failed — no fingerprints matched",
		);
	}

	Err(Error::Security(
		"Presented certificate chain does not match any configured SPKI pins.".into(),
	))
}

#[cfg(test)]
mod tests {
	use super::*;
	use url::Url;

	#[test]
	fn base64_variants_are_accepted() {
		let bytes = [42u8; 32];
		let standard = BASE64_STANDARD.encode(bytes);
		let url_safe = BASE64_URL_SAFE_NO_PAD.encode(bytes);

		for encoded in [standard, url_safe] {
			let fingerprint = SpkiFingerprint::from_b64(&encoded).expect("valid fingerprint");
			assert_eq!(fingerprint.as_bytes(), &bytes);
		}
	}

	#[test]
	fn base64_length_error_is_reported() {
		let err = SpkiFingerprint::from_b64("AQID");
		assert!(err.is_err());
	}

	#[test]
	fn host_allowlist_handles_case_and_trailing_dot() {
		let domains = normalize_allowlist(vec!["Example.COM.".into()]);
		assert!(host_is_allowed("api.EXAMPLE.com.", &domains));
		assert!(host_is_allowed("example.com.", &domains));
		assert!(!host_is_allowed("other.org", &domains));
		let empty_allowlist: Vec<String> = Vec::new();
		assert!(host_is_allowed("anything.example", &empty_allowlist));
	}

	#[test]
	fn verify_spki_pins_success_and_failure() {
		let spki_primary = b"primary";
		let spki_other = b"other";
		let pin_value = BASE64_STANDARD.encode(fingerprint_spki(spki_primary));
		let pins = vec![SpkiFingerprint::from_b64(&pin_value).unwrap()];

		assert!(verify_spki_pins([spki_primary.as_slice()], &pins).is_ok());
		assert!(verify_spki_pins([spki_other.as_slice()], &pins).is_err());
	}

	#[test]
	fn enforce_https_rejects_insecure_scheme() {
		let http = Url::parse("http://example.com/jwks").unwrap();
		assert!(enforce_https(&http).is_err());
	}
}
