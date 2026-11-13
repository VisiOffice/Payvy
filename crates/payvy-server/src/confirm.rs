//! Confirmation tokens for `Confirmed`-tier commands. A commit must quote a
//! token from a preview of the SAME command and input, so an agent cannot
//! move money on a single hallucinated call, and an input edited after the
//! preview is refused rather than silently committed.

use std::time::{SystemTime, UNIX_EPOCH};

use hmac::{Hmac, Mac};
use sha2::Sha256;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

const TTL_SECS: u64 = 30 * 60;

pub struct Confirmer {
    key: Vec<u8>,
}

impl Confirmer {
    pub fn new(key: &[u8]) -> Self {
        Self { key: key.to_vec() }
    }

    pub fn issue(&self, org_id: Uuid, command: &str, input: &serde_json::Value) -> String {
        // The nonce makes every preview's token distinct, so a second preview
        // of the same input in the same second isn't refused as already used.
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).expect("system randomness");
        let at = format!("{}-{}", now(), hex::encode(nonce));
        let mac = self
            .mac(&at, org_id, command, input)
            .finalize()
            .into_bytes();
        format!("{at}.{}", hex::encode(mac))
    }

    /// Ok when valid, otherwise why the token was refused.
    pub fn check(
        &self,
        token: &str,
        org_id: Uuid,
        command: &str,
        input: &serde_json::Value,
    ) -> Result<(), String> {
        let (stamp, mac) = token
            .split_once('.')
            .ok_or_else(|| "Malformed confirm_token.".to_owned())?;
        let at: u64 = stamp
            .split('-')
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| "Malformed confirm_token.".to_owned())?;
        let mac = hex::decode(mac).map_err(|_| "Malformed confirm_token.".to_owned())?;
        if now().saturating_sub(at) > TTL_SECS {
            return Err("confirm_token has expired (30 minutes). Preview again.".to_owned());
        }
        self.mac(stamp, org_id, command, input)
            .verify_slice(&mac)
            .map_err(|_| {
                "confirm_token does not match: the command or its input changed since the preview. Preview again."
                    .to_owned()
            })
    }

    // serde_json objects keep keys sorted, so the same input always signs the same.
    fn mac(
        &self,
        stamp: &str,
        org_id: Uuid,
        command: &str,
        input: &serde_json::Value,
    ) -> HmacSha256 {
        let mut mac = HmacSha256::new_from_slice(&self.key).expect("HMAC takes any key length");
        mac.update(format!("{stamp}\n{org_id}\n{command}\n{input}").as_bytes());
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
        let org = Uuid::new_v4();
        let input = json!({"amount_cents": 100});
        let t = c.issue(org, "bill.release", &input);
        assert!(c.check(&t, org, "bill.release", &input).is_ok());
    }

    #[test]
    fn refuses_a_changed_amount_or_command() {
        let c = Confirmer::new(b"k");
        let org = Uuid::new_v4();
        let t = c.issue(org, "bill.release", &json!({"amount_cents": 100}));
        assert!(
            c.check(&t, org, "bill.release", &json!({"amount_cents": 10_000}))
                .is_err()
        );
        assert!(
            c.check(&t, org, "bill.void", &json!({"amount_cents": 100}))
                .is_err()
        );
    }

    #[test]
    fn refuses_another_organizations_token() {
        let c = Confirmer::new(b"k");
        let org_a = Uuid::new_v4();
        let org_b = Uuid::new_v4();
        let input = json!({"limit_cents": 1_000_000});
        let t = c.issue(org_a, "auto_pay.raise_limit", &input);
        assert!(c.check(&t, org_b, "auto_pay.raise_limit", &input).is_err());
    }

    #[test]
    fn two_previews_get_two_tokens() {
        let c = Confirmer::new(b"k");
        let org = Uuid::new_v4();
        let input = json!({"bill_id": "b"});
        assert_ne!(
            c.issue(org, "bill.release", &input),
            c.issue(org, "bill.release", &input)
        );
    }

    #[test]
    fn refuses_another_servers_token() {
        let org = Uuid::new_v4();
        let t = Confirmer::new(b"a").issue(org, "bill.release", &json!({}));
        assert!(
            Confirmer::new(b"b")
                .check(&t, org, "bill.release", &json!({}))
                .is_err()
        );
    }
}
