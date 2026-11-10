//! Read-only sandbox probe. Credentials stay in environment variables; output
//! contains only the connector's typed, masked discovery data.
use payvy_pay::ramp::{Environment, OAuth, Ramp};
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let token = match std::env::var("PAYVY_RAMP_SANDBOX_TOKEN") {
        Ok(token) => token,
        Err(_) => {
            let client_id = std::env::var("PAYVY_RAMP_SANDBOX_CLIENT_ID")
                .map_err(|_| anyhow::anyhow!("set sandbox token or sandbox client credentials"))?;
            let client_secret = std::env::var("PAYVY_RAMP_SANDBOX_CLIENT_SECRET")
                .map_err(|_| anyhow::anyhow!("set PAYVY_RAMP_SANDBOX_CLIENT_SECRET"))?;
            OAuth::new(Environment::Sandbox, &client_id, &client_secret)?
                .client_credentials()
                .await?
                .access_token
        }
    };
    let ramp = Ramp::new(Environment::Sandbox, &token)?;
    let business = ramp.business().await?;
    let entities = ramp.entities().await?;
    let accounts = ramp.accounts().await?;
    let vendors = ramp.vendors().await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "environment": "sandbox",
            "business": business,
            "entities": entities,
            "accounts": accounts,
            "vendor_count": vendors.len(),
            "payments_enabled": false,
        }))?
    );
    Ok(())
}
