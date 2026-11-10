//! Outbound email: sign-in links now, notifications later. One small adapter
//! with a backend per sender, like document storage. Without one configured,
//! mail is written to the log, which is enough to sign in on a development
//! machine and never enough in production (the server warns at startup).

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mail {
    pub to: String,
    pub subject: String,
    pub text: String,
}

pub enum Mailer {
    /// Writes each message to the log.
    Log,
    /// Keeps each message, for tests.
    Memory(Arc<Mutex<Vec<Mail>>>),
    /// Cloudflare Email Service: the same Cloudflare account that receives
    /// mail can send it, from an address on a domain it serves.
    Cloudflare {
        account_id: String,
        token: String,
        from: String,
        api_base: String,
        http: reqwest::Client,
    },
}

pub const CLOUDFLARE_API: &str = "https://api.cloudflare.com/client/v4";

impl Mailer {
    pub fn cloudflare(account_id: &str, token: &str, from: &str) -> anyhow::Result<Self> {
        Ok(Self::Cloudflare {
            account_id: account_id.to_owned(),
            token: token.to_owned(),
            from: from.to_owned(),
            api_base: CLOUDFLARE_API.to_owned(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .build()?,
        })
    }

    pub fn is_log(&self) -> bool {
        matches!(self, Self::Log)
    }

    pub async fn send(&self, mail: Mail) -> anyhow::Result<()> {
        match self {
            Self::Log => {
                tracing::warn!(
                    to = %mail.to,
                    subject = %mail.subject,
                    "no mail sender configured; this message was not sent:\n{}",
                    mail.text
                );
                Ok(())
            }
            Self::Memory(sent) => {
                sent.lock().expect("mail log").push(mail);
                Ok(())
            }
            Self::Cloudflare {
                account_id,
                token,
                from,
                api_base,
                http,
            } => {
                let res = http
                    .post(format!(
                        "{api_base}/accounts/{account_id}/email/sending/send"
                    ))
                    .bearer_auth(token)
                    .json(&json!({
                        "from": from, "to": mail.to, "subject": mail.subject, "text": mail.text,
                    }))
                    .send()
                    .await?;
                let status = res.status();
                let body: Value = res.json().await.unwrap_or(Value::Null);
                if !status.is_success() || body["success"] != json!(true) {
                    anyhow::bail!(
                        "Cloudflare refused the message ({status}): {}",
                        body["errors"]
                    );
                }
                let bounced = &body["result"]["permanent_bounces"];
                if bounced.as_array().is_some_and(|b| !b.is_empty()) {
                    anyhow::bail!("the message bounced: {bounced}");
                }
                Ok(())
            }
        }
    }
}
