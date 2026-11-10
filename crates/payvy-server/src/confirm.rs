//! Confirmation tokens for `Confirmed`-tier commands. A commit must quote a
//! token from a preview of the SAME command and input, so an agent cannot
//! move money on a single hallucinated call, and an input edited after the
//! preview is refused rather than silently committed.

use std::time::{SystemTime, UNIX_EPOCH};

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

const TTL_SECS: u64 = 30 * 60;

pub struct Confirmer {
    key: Vec<u8>,
}

impl Confirmer {
    pub fn new(key: &[u8]) -> Self {
        Self { key: key.to_vec() }
    }

    pub fn issue(&self, command: &str, input: &serde_json::Value) -> String {
        let at = now();
        let mac = self.mac(at, command, input).finalize().into_bytes();
        format!("{at}.{}", hex::encode(mac))
    }

    /// Ok when valid, otherwise why the token was refused.
    pub fn check(
        &self,
        token: &str,
        command: &str,
        input: &serde_json::Value,
    ) -> Result<(), String> {
        let (at, mac) = token
            .split_once('.')
            .ok_or_else(|| "Malformed confirm_token.".to_owned())?;
        let at: u64 = at
            .parse()
            .map_err(|_| "Malformed confirm_token.".to_owned())?;
        let mac = hex::decode(mac).map_err(|_| "Malformed confirm_token.".to_owned())?;
        if now().saturating_sub(at) > TTL_SECS {
            return Err("confirm_token has expired (30 minutes). Preview again.".to_owned());
        }
        self.mac(at, command, input).verify_slice(&mac).map_err(|_| {
            "confirm_token does not match: the command or its input changed since the preview. Preview again."
                .to_owned()
        })
    }

    // serde_json objects keep keys sorted, so the same input always signs the same.
    fn mac(&self, at: u64, command: &str, input: &serde_json::Value) -> HmacSha256 {
        let mut mac = HmacSha256::new_from_slice(&self.key).expect("HMAC takes any key length");
        mac.update(format!("{at}\n{command}\n{input}").as_bytes());
        mac
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after 1970")
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_its_own_token_for_the_same_input() {
        let c = Confirmer::new(b"k");
        let input = json!({"amount_cents": 100});
        let t = c.issue("bill.release", &input);
        assert!(c.check(&t, "bill.release", &input).is_ok());
    }

    #[test]
    fn refuses_a_changed_amount_or_command() {
        let c = Confirmer::new(b"k");
        let t = c.issue("bill.release", &json!({"amount_cents": 100}));
        assert!(
            c.check(&t, "bill.release", &json!({"amount_cents": 10_000}))
                .is_err()
        );
        assert!(
            c.check(&t, "bill.void", &json!({"amount_cents": 100}))
                .is_err()
        );
    }

    #[test]
    fn refuses_another_servers_token() {
        let t = Confirmer::new(b"a").issue("bill.release", &json!({}));
        assert!(
            Confirmer::new(b"b")
                .check(&t, "bill.release", &json!({}))
                .is_err()
        );
    }
}
