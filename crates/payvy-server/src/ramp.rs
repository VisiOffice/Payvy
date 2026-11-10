//! Server-only Ramp app configuration. Never returned by a command.
use payvy_pay::ramp::{Environment, Error, OAuth, Ramp};

pub struct Config {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub environment: Environment,
    /// Debug loopback fake only. Production always uses Ramp's fixed origin.
    pub sandbox_url: Option<String>,
}
impl Config {
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let id = std::env::var("PAYVY_RAMP_CLIENT_ID").ok();
        let secret = std::env::var("PAYVY_RAMP_CLIENT_SECRET").ok();
        let redirect = std::env::var("PAYVY_RAMP_REDIRECT_URI").ok();
        if id.is_none() && secret.is_none() && redirect.is_none() {
            return Ok(None);
        }
        let config = Self {
            client_id: id.ok_or_else(|| anyhow::anyhow!("PAYVY_RAMP_CLIENT_ID is required"))?,
            client_secret: secret
                .ok_or_else(|| anyhow::anyhow!("PAYVY_RAMP_CLIENT_SECRET is required"))?,
            redirect_uri: redirect
                .ok_or_else(|| anyhow::anyhow!("PAYVY_RAMP_REDIRECT_URI is required"))?,
            environment: match std::env::var("PAYVY_RAMP_ENVIRONMENT")
                .as_deref()
                .unwrap_or("sandbox")
            {
                "sandbox" => Environment::Sandbox,
                "production" => Environment::Production,
                _ => anyhow::bail!("PAYVY_RAMP_ENVIRONMENT must be sandbox or production"),
            },
            sandbox_url: std::env::var("PAYVY_RAMP_SANDBOX_URL").ok(),
        };
        config
            .oauth()?
            .authorize_url(config.environment, &config.redirect_uri, "validate")?;
        Ok(Some(config))
    }
    pub fn environment_name(&self) -> &'static str {
        match self.environment {
            Environment::Sandbox => "sandbox",
            Environment::Production => "production",
        }
    }
    pub fn oauth(&self) -> Result<OAuth, Error> {
        if let Some(url) = &self.sandbox_url {
            if !matches!(self.environment, Environment::Sandbox) {
                return Err(Error::InvalidInput);
            }
            OAuth::sandbox_at(url, &self.client_id, &self.client_secret)
        } else {
            OAuth::new(self.environment, &self.client_id, &self.client_secret)
        }
    }
    pub fn client(&self, token: &str) -> Result<Ramp, Error> {
        if let Some(url) = &self.sandbox_url {
            if !matches!(self.environment, Environment::Sandbox) {
                return Err(Error::InvalidInput);
            }
            Ramp::sandbox_at(url, token)
        } else {
            Ramp::new(self.environment, token)
        }
    }
}
