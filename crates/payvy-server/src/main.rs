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
    },
    /// Manage API keys for people and agents.
    Key {
        #[command(subcommand)]
        command: KeyCommand,
    },
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
        Command::Serve { bind, api_key } => {
            anyhow::ensure!(
                api_key.len() >= 32,
                "PAYVY_API_KEY must be at least 32 characters"
            );
            let state = payvy_server::AppState::connect(&database_url, &api_key).await?;
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
