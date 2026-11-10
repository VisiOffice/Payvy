use clap::{Parser, Subcommand, ValueEnum};
use payvy_store::ActorKind;

#[derive(Parser)]
#[command(about = "payvy: open-source, agent-first accounts payable")]
struct Cli {
    #[arg(long, env = "DATABASE_URL", hide_env_values = true, global = true)]
    database_url: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server.
    Serve {
        #[arg(long, env = "PAYVY_BIND", default_value = "127.0.0.1:7430")]
        bind: std::net::SocketAddr,
        /// Bootstrap key: authenticates as the person `admin` and signs
        /// confirm tokens. At least 32 characters.
        #[arg(long, env = "PAYVY_API_KEY", hide_env_values = true)]
        api_key: String,
        #[command(flatten)]
        storage: StorageArgs,
        #[command(flatten)]
        inbound: InboundArgs,
    },
    /// Manage API keys for people and agents.
    Key {
        #[command(subcommand)]
        command: KeyCommand,
    },
}

/// Document storage: any S3-compatible store (Cloudflare R2, Backblaze B2,
/// AWS S3, Garage, SeaweedFS). Without it, document commands report that
/// storage isn't configured.
#[derive(clap::Args)]
struct StorageArgs {
    /// e.g. https://<account>.r2.cloudflarestorage.com or
    /// https://s3.us-west-004.backblazeb2.com
    #[arg(long, env = "PAYVY_S3_ENDPOINT", requires_all = ["s3_bucket", "s3_access_key", "s3_secret_key"])]
    s3_endpoint: Option<String>,
    #[arg(long, env = "PAYVY_S3_BUCKET")]
    s3_bucket: Option<String>,
    /// "auto" for R2; the bucket's region for B2 and AWS.
    #[arg(long, env = "PAYVY_S3_REGION", default_value = "auto")]
    s3_region: String,
    #[arg(long, env = "PAYVY_S3_ACCESS_KEY", hide_env_values = true)]
    s3_access_key: Option<String>,
    #[arg(long, env = "PAYVY_S3_SECRET_KEY", hide_env_values = true)]
    s3_secret_key: Option<String>,
    /// Path-style URLs (endpoint/bucket/key): right for R2, B2 and
    /// self-hosted stores. Set false for AWS virtual-hosted buckets.
    #[arg(long, env = "PAYVY_S3_PATH_STYLE", default_value_t = true, action = clap::ArgAction::Set)]
    s3_path_style: bool,
}

impl StorageArgs {
    fn config(self) -> Option<payvy_storage::Config> {
        Some(payvy_storage::Config {
            endpoint: self.s3_endpoint?,
            bucket: self.s3_bucket?,
            region: self.s3_region,
            access_key: self.s3_access_key?,
            secret_key: self.s3_secret_key?,
            path_style: self.s3_path_style,
        })
    }
}

/// Email intake: bills sent to `{token}@{domain}` land in the inbox. Mail
/// reaches the server through a front end that signs each message, such as
/// the Cloudflare Email Worker in `workers/email-inbound` (see
/// docs/email-intake.md).
#[derive(clap::Args)]
struct InboundArgs {
    /// The domain inbound addresses live on, e.g. in.example.com.
    #[arg(long, env = "PAYVY_INBOUND_DOMAIN", requires = "inbound_secret")]
    inbound_domain: Option<String>,
    /// Shared with the front end, which signs each message with it. At least
    /// 32 characters.
    #[arg(long, env = "PAYVY_INBOUND_SECRET", hide_env_values = true)]
    inbound_secret: Option<String>,
}

#[derive(Subcommand)]
enum KeyCommand {
    /// Create a key. It is printed once and never stored.
    Create {
        /// Who acts with this key, as it appears in approvals and the audit
        /// log: "robert", "claude-ap".
        #[arg(long)]
        actor: String,
        /// A person can approve, release and accept workflows; an agent can't.
        #[arg(long, value_enum)]
        kind: Kind,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Kind {
    Person,
    Agent,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let cli = Cli::parse();
    let database_url = cli
        .database_url
        .ok_or_else(|| anyhow::anyhow!("set DATABASE_URL or --database-url"))?;

    match cli.command {
        Command::Serve {
            bind,
            api_key,
            storage,
            inbound,
        } => {
            anyhow::ensure!(
                api_key.len() >= 32,
                "PAYVY_API_KEY must be at least 32 characters"
            );
            let mut state = payvy_server::AppState::connect(&database_url, &api_key).await?;
            match storage.config() {
                Some(config) => {
                    tracing::info!(endpoint = %config.endpoint, bucket = %config.bucket, "document storage");
                    state = state.with_storage(payvy_storage::Storage::new(&config)?);
                }
                None => tracing::warn!("no document storage configured; document commands are off"),
            }
            match (inbound.inbound_domain, inbound.inbound_secret) {
                (Some(domain), Some(secret)) => {
                    tracing::info!(%domain, "email intake");
                    state =
                        state.with_inbound(payvy_server::inbound::Inbound::new(&domain, &secret)?);
                }
                _ => tracing::warn!("no inbound email domain configured; email intake is off"),
            }
            let listener = tokio::net::TcpListener::bind(bind).await?;
            tracing::info!("payvy listening on http://{bind}");
            axum::serve(listener, payvy_server::router(state))
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await?;
        }
        Command::Key {
            command: KeyCommand::Create { actor, kind },
        } => {
            let actor = actor.trim();
            anyhow::ensure!(!actor.is_empty(), "--actor can't be empty");
            let store = payvy_store::Store::connect(&database_url).await?;
            let (key, hash) = payvy_server::new_api_key()?;
            let kind = match kind {
                Kind::Person => ActorKind::Person,
                Kind::Agent => ActorKind::Agent,
            };
            store
                .insert_api_key(payvy_store::DEFAULT_ORG, actor, kind, &hash)
                .await?;
            println!("{key}");
            eprintln!(
                "Key for {} {actor}. Shown once; store it now.",
                kind.as_str()
            );
        }
    }
    Ok(())
}
