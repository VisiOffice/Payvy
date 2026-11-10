//! Documents in any S3-compatible object store.
//!
//! One code path for Cloudflare R2, Backblaze B2, AWS S3 and self-hosted
//! stores (Garage, SeaweedFS). Requests are presigned by `rusty-s3` and sent
//! with reqwest; there is no SDK. Objects are written once and never edited:
//! keys are content hashes, so writing the same file twice is harmless.

use std::time::Duration;

use anyhow::{Context, bail};
use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle};

/// How long a signed request (an upload, or a download link handed to a
/// browser) stays valid.
const SIGN_FOR: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone)]
pub struct Config {
    /// e.g. https://<account>.r2.cloudflarestorage.com,
    /// https://s3.us-west-004.backblazeb2.com, http://127.0.0.1:8336
    pub endpoint: String,
    pub bucket: String,
    /// "auto" for R2; the bucket's region for B2 and AWS (us-west-004, us-east-1).
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
    /// Path-style URLs (endpoint/bucket/key). R2, B2 and self-hosted stores
    /// accept them; AWS prefers virtual-hosted, so default off only there.
    pub path_style: bool,
}

pub struct Storage {
    bucket: Bucket,
    credentials: Credentials,
    http: reqwest::Client,
}

impl Storage {
    pub fn new(config: &Config) -> anyhow::Result<Self> {
        let endpoint = config
            .endpoint
            .parse()
            .context("storage endpoint is not a URL")?;
        let style = if config.path_style {
            UrlStyle::Path
        } else {
            UrlStyle::VirtualHost
        };
        let bucket = Bucket::new(
            endpoint,
            style,
            config.bucket.clone(),
            config.region.clone(),
        )
        .context("storage bucket name or endpoint is invalid")?;
        let credentials = Credentials::new(config.access_key.clone(), config.secret_key.clone());
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()?;
        Ok(Self {
            bucket,
            credentials,
            http,
        })
    }

    pub async fn put(&self, key: &str, bytes: Vec<u8>, content_type: &str) -> anyhow::Result<()> {
        let url = self
            .bucket
            .put_object(Some(&self.credentials), key)
            .sign(SIGN_FOR);
        let res = self
            .http
            .put(url)
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(bytes)
            .send()
            .await
            .context("storage unreachable")?;
        check(res, "upload").await
    }

    pub async fn get(&self, key: &str) -> anyhow::Result<Vec<u8>> {
        let url = self
            .bucket
            .get_object(Some(&self.credentials), key)
            .sign(SIGN_FOR);
        let res = self
            .http
            .get(url)
            .send()
            .await
            .context("storage unreachable")?;
        if !res.status().is_success() {
            bail!("storage download failed: {}", res.status());
        }
        Ok(res.bytes().await?.to_vec())
    }

    /// A link that downloads the object for the next 15 minutes, for the web
    /// app's viewer. It carries no other permission.
    pub fn download_url(&self, key: &str) -> String {
        self.bucket
            .get_object(Some(&self.credentials), key)
            .sign(SIGN_FOR)
            .to_string()
    }

    /// Creates the bucket if it isn't there. For development and tests; in
    /// production the bucket is made once in the provider's console.
    pub async fn ensure_bucket(&self) -> anyhow::Result<()> {
        let url = self.bucket.create_bucket(&self.credentials).sign(SIGN_FOR);
        let res = self
            .http
            .put(url)
            .send()
            .await
            .context("storage unreachable")?;
        // 409: it already exists (BucketAlreadyOwnedByYou).
        if res.status() == reqwest::StatusCode::CONFLICT {
            return Ok(());
        }
        check(res, "create bucket").await
    }
}

async fn check(res: reqwest::Response, what: &str) -> anyhow::Result<()> {
    if res.status().is_success() {
        return Ok(());
    }
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    // S3 errors are XML; the <Code> is the useful part and holds no secrets.
    let code = body
        .split("<Code>")
        .nth(1)
        .and_then(|s| s.split("</Code>").next())
        .unwrap_or("");
    bail!("storage {what} failed: {status} {code}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Against a real store when PAYVY_TEST_S3_ENDPOINT is set (docker compose
    /// runs SeaweedFS); skipped otherwise.
    #[tokio::test]
    async fn round_trip() {
        let Some(config) = test_config() else {
            eprintln!("skipping: PAYVY_TEST_S3_ENDPOINT not set");
            return;
        };
        let s = Storage::new(&config).unwrap();
        s.ensure_bucket().await.unwrap();
        s.ensure_bucket().await.unwrap();
        s.put("test/round-trip.txt", b"hello".to_vec(), "text/plain")
            .await
            .unwrap();
        assert_eq!(s.get("test/round-trip.txt").await.unwrap(), b"hello");
        let link = s.download_url("test/round-trip.txt");
        assert_eq!(
            reqwest::get(&link)
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
                .as_ref(),
            b"hello"
        );
        assert!(s.get("test/missing.txt").await.is_err());
    }

    fn test_config() -> Option<Config> {
        Some(Config {
            endpoint: std::env::var("PAYVY_TEST_S3_ENDPOINT").ok()?,
            bucket: "payvy-test".into(),
            region: "us-east-1".into(),
            access_key: std::env::var("PAYVY_TEST_S3_ACCESS_KEY").ok()?,
            secret_key: std::env::var("PAYVY_TEST_S3_SECRET_KEY").ok()?,
            path_style: true,
        })
    }
}
