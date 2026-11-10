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
    /// Amazon SES (API v2), from an address or domain verified in SES.
    Ses {
        region: String,
        access_key_id: String,
        secret_access_key: String,
        from: String,
        /// https://email.{region}.amazonaws.com unless a test points it elsewhere.
        endpoint: String,
        http: reqwest::Client,
    },
}

pub const CLOUDFLARE_API: &str = "https://api.cloudflare.com/client/v4";
const SES_PATH: &str = "/v2/email/outbound-emails";

fn http() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()?)
}

impl Mailer {
    pub fn cloudflare(account_id: &str, token: &str, from: &str) -> anyhow::Result<Self> {
        Ok(Self::Cloudflare {
            account_id: account_id.to_owned(),
            token: token.to_owned(),
            from: from.to_owned(),
            api_base: CLOUDFLARE_API.to_owned(),
            http: http()?,
        })
    }

    pub fn ses(
        region: &str,
        access_key_id: &str,
        secret_access_key: &str,
        from: &str,
    ) -> anyhow::Result<Self> {
        Ok(Self::Ses {
            region: region.to_owned(),
            access_key_id: access_key_id.to_owned(),
            secret_access_key: secret_access_key.to_owned(),
            from: from.to_owned(),
            endpoint: format!("https://email.{region}.amazonaws.com"),
            http: http()?,
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
            Self::Ses {
                region,
                access_key_id,
                secret_access_key,
                from,
                endpoint,
                http,
            } => {
                let body = serde_json::to_vec(&json!({
                    "FromEmailAddress": from,
                    "Destination": { "ToAddresses": [mail.to] },
                    "Content": { "Simple": {
                        "Subject": { "Data": mail.subject, "Charset": "UTF-8" },
                        "Body": { "Text": { "Data": mail.text, "Charset": "UTF-8" } },
                    } },
                }))?;
                let url = reqwest::Url::parse(&format!("{endpoint}{SES_PATH}"))?;
                let host = match (url.host_str(), url.port()) {
                    (Some(h), Some(p)) => format!("{h}:{p}"),
                    (Some(h), None) => h.to_owned(),
                    _ => anyhow::bail!("SES endpoint has no host: {endpoint}"),
                };
                let signed = crate::sigv4::sign(
                    &crate::sigv4::Credentials {
                        access_key_id,
                        secret_access_key,
                        region,
                        service: "ses",
                    },
                    &crate::sigv4::Request {
                        method: "POST",
                        host: &host,
                        path: SES_PATH,
                        headers: &[("content-type", "application/json")],
                        body: &body,
                    },
                    chrono::Utc::now(),
                );
                let mut req = http
                    .post(url)
                    .header("content-type", "application/json")
                    .body(body);
                for (k, v) in signed {
                    req = req.header(k, v);
                }
                let res = req.send().await?;
                let status = res.status();
                if !status.is_success() {
                    let body: Value = res.json().await.unwrap_or(Value::Null);
                    anyhow::bail!(
                        "SES refused the message ({status}): {}",
                        body["message"].as_str().unwrap_or("no reason given")
                    );
                }
                Ok(())
            }
        }
    }
}
