//! Customer secrets kept in the database (a bank's API token), sealed with
//! XChaCha20-Poly1305 under a key that lives only in the server's
//! environment (PAYVY_SECRETS_KEY). A database dump without that key reveals
//! no token. Each secret is bound to its organization and purpose, so a
//! sealed value copied to another row or organization won't open.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use uuid::Uuid;

pub struct Sealer {
    cipher: XChaCha20Poly1305,
}

impl Sealer {
    /// From 64 hex characters (32 bytes): `openssl rand -hex 32`.
    pub fn from_hex(key: &str) -> anyhow::Result<Self> {
        let bytes = hex::decode(key.trim())
            .map_err(|_| anyhow::anyhow!("PAYVY_SECRETS_KEY must be 64 hex characters"))?;
        anyhow::ensure!(
            bytes.len() == 32,
            "PAYVY_SECRETS_KEY must be 64 hex characters (32 bytes)"
        );
        Ok(Self {
            cipher: XChaCha20Poly1305::new_from_slice(&bytes)
                .map_err(|_| anyhow::anyhow!("bad key length"))?,
        })
    }

    fn aad(org: Uuid, purpose: &str) -> Vec<u8> {
        format!("payvy:{org}:{purpose}").into_bytes()
    }

    /// The nonce followed by the ciphertext.
    pub fn seal(&self, org: Uuid, purpose: &str, secret: &str) -> anyhow::Result<Vec<u8>> {
        let mut nonce = [0u8; 24];
        getrandom::fill(&mut nonce).map_err(|e| anyhow::anyhow!("no randomness: {e}"))?;
        let aad = Self::aad(org, purpose);
        let sealed = self
            .cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: secret.as_bytes(),
                    aad: &aad,
                },
            )
            .map_err(|_| anyhow::anyhow!("couldn't seal the secret"))?;
        Ok([nonce.as_slice(), &sealed].concat())
    }

    pub fn open(&self, org: Uuid, purpose: &str, sealed: &[u8]) -> anyhow::Result<String> {
        anyhow::ensure!(sealed.len() > 24, "sealed secret is too short");
        let (nonce, body) = sealed.split_at(24);
        let aad = Self::aad(org, purpose);
        let plain = self
            .cipher
            .decrypt(XNonce::from_slice(nonce), Payload { msg: body, aad: &aad })
            .map_err(|_| {
                anyhow::anyhow!(
                    "couldn't open a stored secret: PAYVY_SECRETS_KEY changed, or the row was tampered with"
                )
            })?;
        Ok(String::from_utf8(plain)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_secrets_open_only_where_they_were_sealed() {
        let s = Sealer::from_hex(&"ab".repeat(32)).unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let sealed = s
            .seal(a, "bank:x", "secret-token:mercury_production_abc")
            .unwrap();
        assert!(!String::from_utf8_lossy(&sealed).contains("mercury_production"));
        assert_eq!(
            s.open(a, "bank:x", &sealed).unwrap(),
            "secret-token:mercury_production_abc"
        );
        assert!(s.open(b, "bank:x", &sealed).is_err());
        assert!(s.open(a, "bank:y", &sealed).is_err());
        let other = Sealer::from_hex(&"cd".repeat(32)).unwrap();
        assert!(other.open(a, "bank:x", &sealed).is_err());
        assert!(Sealer::from_hex("short").is_err());
    }
}
