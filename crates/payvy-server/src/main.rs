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

// Parsed once at startup; boxing the big variant would buy nothing.
#[allow(clippy::large_enum_variant)]
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
        #[command(flatten)]
        extract: ExtractArgs,
        /// Read new inbox documents in the background as the agent
        /// `extractor`. Off: they are read only when document.extract is
        /// called.
        #[arg(long, env = "PAYVY_EXTRACT_AUTO", default_value_t = true, action = clap::ArgAction::Set)]
        extract_auto: bool,
        /// Seals customers' bank tokens in the database: 64 hex characters
        /// (`openssl rand -hex 32`). Without it, bills are paid outside
        /// Payvy and marked paid. Losing it means reconnecting every bank.
        #[arg(long, env = "PAYVY_SECRETS_KEY", hide_env_values = true)]
        secrets_key: Option<String>,
        /// Where a sandbox Mercury connection talks to, instead of
        /// Mercury's own sandbox. For tests against a fake bank; production
        /// connections always go to Mercury.
        #[arg(long, env = "PAYVY_MERCURY_SANDBOX_URL")]
        mercury_sandbox_url: Option<String>,
        /// The same for a sandbox Rho connection.
        #[arg(long, env = "PAYVY_RHO_SANDBOX_URL")]
        rho_sandbox_url: Option<String>,
        /// Where Mercury can reach this server, e.g. https://dev.payvy.com.
        /// Connecting Mercury then registers a webhook at
        /// {url}/webhooks/mercury/… so payment updates arrive at once.
        #[arg(long, env = "PAYVY_WEBHOOK_URL")]
        webhook_url: Option<String>,
        #[command(flatten)]
        web: WebArgs,
    },
    /// Score the configured extraction model against sample documents with
    /// known answers, e.g. fixtures/invoices. Sends each one to the model.
    ExtractEval {
        dir: std::path::PathBuf,
        #[command(flatten)]
        extract: ExtractArgs,
    },
    /// Print every command with its input schema as JSON. The web app's
    /// typed client is generated from this (web/scripts/gen-api.mjs).
    Commands,
    /// Let someone sign in to the web app (the same as person.add, run as
    /// admin). For the first person; after that, people add each other.
    Person {
        #[command(subcommand)]
        command: PersonCommand,
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
    /// Create the bucket at startup if it doesn't exist: for a fresh local
    /// store (SeaweedFS, Garage) or the end-to-end tests. Off by default;
    /// production keys usually can't create buckets, and shouldn't.
    #[arg(long, env = "PAYVY_S3_CREATE_BUCKET", default_value_t = false, action = clap::ArgAction::Set)]
    s3_create_bucket: bool,
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

/// Reading bills from documents: Claude, or any OpenAI-compatible API
/// (OpenAI, Gemini, Grok, OpenRouter, or a local model through Ollama or
/// vLLM). Without it, documents wait in the inbox to be entered by hand or
/// by an agent that reads them itself.
#[derive(clap::Args)]
struct ExtractArgs {
    /// anthropic or openai-compatible.
    #[arg(long, env = "PAYVY_EXTRACT_PROVIDER")]
    extract_provider: Option<String>,
    /// Defaults to claude-opus-5-5 for anthropic; required otherwise.
    #[arg(long, env = "PAYVY_EXTRACT_MODEL")]
    extract_model: Option<String>,
    /// Anthropic: https://api.anthropic.com. OpenAI-compatible: the URL
    /// `/chat/completions` hangs off; defaults to https://api.openai.com/v1.
    #[arg(long, env = "PAYVY_EXTRACT_BASE_URL")]
    extract_base_url: Option<String>,
    /// Optional for a local server that doesn't check one.
    #[arg(long, env = "PAYVY_EXTRACT_API_KEY", hide_env_values = true)]
    extract_api_key: Option<String>,
}

impl ExtractArgs {
    fn extractor(self) -> anyhow::Result<Option<payvy_extract::Extractor>> {
        use payvy_extract::Provider;
        let Some(name) = self.extract_provider else {
            return Ok(None);
        };
        let provider = Provider::parse(&name).ok_or_else(|| {
            anyhow::anyhow!("PAYVY_EXTRACT_PROVIDER is anthropic or openai-compatible, not {name}")
        })?;
        let model = match (self.extract_model, provider) {
            (Some(m), _) => m,
            (None, Provider::Anthropic) => payvy_extract::DEFAULT_CLAUDE_MODEL.into(),
            (None, Provider::OpenAiCompatible) => {
                anyhow::bail!("set PAYVY_EXTRACT_MODEL for an openai-compatible provider")
            }
        };
        anyhow::ensure!(
            provider != Provider::Anthropic || self.extract_api_key.is_some(),
            "set PAYVY_EXTRACT_API_KEY for anthropic"
        );
        Ok(Some(payvy_extract::Extractor::new(
            payvy_extract::Config {
                provider,
                base_url: self
                    .extract_base_url
                    .unwrap_or_else(|| provider.default_base_url().into()),
                api_key: self.extract_api_key,
                model,
            },
        )?))
    }
}

/// The web app, and signing in to it with links sent by email.
#[derive(clap::Args)]
struct WebArgs {
    /// Where people open the app, e.g. https://beta.payvy.com; sign-in links
    /// point here. Without it, nobody can sign in (API keys still work).
    #[arg(long, env = "PAYVY_PUBLIC_URL")]
    public_url: Option<String>,
    /// The built web app (web/dist) to serve beside the API.
    #[arg(long, env = "PAYVY_WEB_DIR")]
    web_dir: Option<std::path::PathBuf>,
    /// The address mail comes from, e.g. Payvy <no-reply@payvy.com>. Needs
    /// one sender below: Cloudflare Email Service or Amazon SES.
    #[arg(long, env = "PAYVY_MAIL_FROM")]
    mail_from: Option<String>,
    /// Sends mail through Cloudflare Email Service from this account.
    #[arg(long, env = "PAYVY_CLOUDFLARE_ACCOUNT_ID")]
    cloudflare_account_id: Option<String>,
    /// A Cloudflare API token allowed to send email.
    #[arg(long, env = "PAYVY_CLOUDFLARE_EMAIL_TOKEN", hide_env_values = true)]
    cloudflare_email_token: Option<String>,
    /// Sends mail through Amazon SES in this region, e.g. us-west-2.
    #[arg(long, env = "PAYVY_SES_REGION")]
    ses_region: Option<String>,
    /// An IAM access key allowed ses:SendEmail.
    #[arg(long, env = "PAYVY_SES_ACCESS_KEY_ID")]
    ses_access_key_id: Option<String>,
    #[arg(long, env = "PAYVY_SES_SECRET_ACCESS_KEY", hide_env_values = true)]
    ses_secret_access_key: Option<String>,
}

impl WebArgs {
    fn mailer(&self) -> anyhow::Result<payvy_server::mailer::Mailer> {
        use payvy_server::mailer::Mailer;
        let cloudflare = match (&self.cloudflare_account_id, &self.cloudflare_email_token) {
            (Some(a), Some(t)) => Some((a, t)),
            (None, None) => None,
            _ => anyhow::bail!(
                "set both PAYVY_CLOUDFLARE_ACCOUNT_ID and PAYVY_CLOUDFLARE_EMAIL_TOKEN, or neither"
            ),
        };
        let ses = match (
            &self.ses_region,
            &self.ses_access_key_id,
            &self.ses_secret_access_key,
        ) {
            (Some(r), Some(k), Some(s)) => Some((r, k, s)),
            (None, None, None) => None,
            _ => anyhow::bail!(
                "set PAYVY_SES_REGION, PAYVY_SES_ACCESS_KEY_ID and PAYVY_SES_SECRET_ACCESS_KEY together, or none"
            ),
        };
        Ok(match (&self.mail_from, cloudflare, ses) {
            (None, None, None) => Mailer::Log,
            (Some(_), Some(_), Some(_)) => {
                anyhow::bail!("mail is set up for both Cloudflare and SES; choose one")
            }
            (Some(from), Some((account, token)), None) => Mailer::cloudflare(account, token, from)?,
            (Some(from), None, Some((region, key, secret))) => {
                Mailer::ses(region, key, secret, from)?
            }
            (Some(_), None, None) => anyhow::bail!(
                "PAYVY_MAIL_FROM is set but no sender: set the Cloudflare or the SES settings"
            ),
            (None, _, _) => {
                anyhow::bail!("a mail sender is set up but PAYVY_MAIL_FROM isn't")
            }
        })
    }
}

#[derive(Subcommand)]
enum PersonCommand {
    Add {
        #[arg(long)]
        email: String,
        #[arg(long)]
        name: String,
        /// Defaults to the part of the email before @.
        #[arg(long)]
        actor: Option<String>,
        /// Comma-separated roles. The trusted operator CLI defaults to Admin
        /// for initial setup/recovery; adding someone in the app defaults to AP clerk.
        #[arg(long, value_delimiter = ',', default_value = "admin")]
        roles: Vec<String>,
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
    let database_url = || {
        cli.database_url
            .clone()
            .ok_or_else(|| anyhow::anyhow!("set DATABASE_URL or --database-url"))
    };

    match cli.command {
        Command::ExtractEval { ref dir, extract } => {
            let extractor = extract
                .extractor()?
                .ok_or_else(|| anyhow::anyhow!("set PAYVY_EXTRACT_PROVIDER and its key"))?;
            let score = payvy_server::eval::run(&extractor, dir).await?;
            if score.right < score.total {
                std::process::exit(1);
            }
        }
        Command::Serve {
            bind,
            ref api_key,
            storage,
            inbound,
            extract,
            extract_auto,
            ref secrets_key,
            ref mercury_sandbox_url,
            ref rho_sandbox_url,
            ref webhook_url,
            ref web,
        } => {
            let api_key = api_key.clone();
            anyhow::ensure!(
                api_key.len() >= 32,
                "PAYVY_API_KEY must be at least 32 characters"
            );
            // Settings mistakes fail now, before anything connects.
            let mailer = web.mailer()?;
            let mut state = payvy_server::AppState::connect(&database_url()?, &api_key).await?;
            let create_bucket = storage.s3_create_bucket;
            match storage.config() {
                Some(config) => {
                    tracing::info!(endpoint = %config.endpoint, bucket = %config.bucket, "document storage");
                    let store = payvy_storage::Storage::new(&config)?;
                    if create_bucket {
                        store.ensure_bucket().await?;
                    }
                    state = state.with_storage(store);
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
            match extract.extractor()? {
                Some(extractor) => {
                    tracing::info!(model = extractor.model(), provider = %extractor.provider_label(), "document reading");
                    state = state.with_extractor(extractor);
                    if extract_auto && state.storage.is_some() {
                        tokio::spawn(payvy_server::extract_worker::run(state.clone()));
                    }
                    // Bills asked for from a file of several invoices are
                    // read and made whether or not the background reader is on.
                    if state.storage.is_some() {
                        tokio::spawn(payvy_server::bill_batch_worker::run(state.clone()));
                    }
                }
                None => tracing::warn!(
                    "no extraction model configured; documents aren't read automatically"
                ),
            }
            match secrets_key {
                Some(key) => {
                    let sealer = payvy_server::secrets::Sealer::from_hex(key)?;
                    let mut bank = payvy_server::Bank::new(sealer);
                    bank.ramp = payvy_server::ramp::Config::from_env()?;
                    // Browser tests point sandbox connections at a local fake
                    // bank. Only debug builds accept it, and only on loopback;
                    // production connections always reach the real bank.
                    for (name, url, slot) in [
                        (
                            "PAYVY_MERCURY_SANDBOX_URL",
                            mercury_sandbox_url,
                            &mut bank.mercury_sandbox,
                        ),
                        (
                            "PAYVY_RHO_SANDBOX_URL",
                            rho_sandbox_url,
                            &mut bank.rho_sandbox,
                        ),
                    ] {
                        let Some(url) = url else { continue };
                        anyhow::ensure!(
                            cfg!(debug_assertions),
                            "{name} is for tests and only works in debug builds"
                        );
                        let parsed = reqwest::Url::parse(url)?;
                        anyhow::ensure!(
                            parsed.scheme() == "http" && parsed.host_str() == Some("127.0.0.1"),
                            "{name} must be an http://127.0.0.1 URL"
                        );
                        *slot = url.clone();
                    }
                    if let Some(url) = webhook_url {
                        let parsed = reqwest::Url::parse(url)?;
                        anyhow::ensure!(
                            parsed.scheme() == "https",
                            "PAYVY_WEBHOOK_URL must be an https URL Mercury can reach"
                        );
                        bank.webhook_base = Some(url.trim_end_matches('/').to_owned());
                        tracing::info!(%url, "Mercury webhooks");
                    }
                    state = state.with_bank(bank);
                    tokio::spawn(payvy_server::payment_worker::run(state.clone()));
                    tokio::spawn(payvy_server::ramp_worker::run(state.clone()));
                    tracing::info!("paying through connected banks");
                }
                None => tracing::warn!("PAYVY_SECRETS_KEY not set; bills are paid outside Payvy"),
            }
            match &web.public_url {
                Some(url) => {
                    if mailer.is_log() {
                        tracing::warn!(
                            "no mail sender configured; sign-in links are written to this log, not sent"
                        );
                    }
                    tracing::info!(%url, "web sign-in");
                    state = state.with_auth(payvy_server::auth::Auth::new(url, mailer));
                    tokio::spawn(payvy_server::notification_worker::run(state.clone()));
                }
                None => {
                    tracing::warn!("PAYVY_PUBLIC_URL not set; nobody can sign in to the web app")
                }
            }
            if let Some(dir) = &web.web_dir {
                anyhow::ensure!(
                    dir.join("index.html").is_file(),
                    "PAYVY_WEB_DIR {} has no index.html; build it with pnpm --dir web build",
                    dir.display()
                );
                tracing::info!(dir = %dir.display(), "serving the web app");
                state = state.with_web(dir);
            }
            let listener = tokio::net::TcpListener::bind(bind).await?;
            tracing::info!("payvy listening on http://{bind}");
            axum::serve(listener, payvy_server::router(state))
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await?;
        }
        Command::Commands => {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "commands": payvy_domain::commands::COMMANDS
                }))?
            );
        }
        Command::Person {
            command:
                PersonCommand::Add {
                    email,
                    name,
                    actor,
                    roles,
                },
        } => {
            let store = payvy_store::Store::connect(&database_url()?).await?;
            let (unused, _) = payvy_server::new_api_key()?;
            let state = payvy_server::AppState::new(store, &unused);
            let out = payvy_server::commands::run(
                &state,
                &payvy_server::Actor::administrator(payvy_store::DEFAULT_ORG, "admin"),
                "person.add",
                serde_json::json!({ "email": email, "name": name, "actor": actor, "roles": roles }),
                payvy_server::commands::Mode::Commit {
                    confirm_token: None,
                },
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&out["result"])?);
        }
        Command::Key {
            command: KeyCommand::Create { actor, kind },
        } => {
            let actor = actor.trim();
            anyhow::ensure!(!actor.is_empty(), "--actor can't be empty");
            let store = payvy_store::Store::connect(&database_url()?).await?;
            let (key, hash) = payvy_server::new_api_key()?;
            let kind = match kind {
                Kind::Person => ActorKind::Person,
                Kind::Agent => ActorKind::Agent,
            };
            if kind == ActorKind::Person {
                let mut tx = store.begin(payvy_store::DEFAULT_ORG).await?;
                anyhow::ensure!(
                    !payvy_store::actor_roles(&mut tx, payvy_store::DEFAULT_ORG, actor)
                        .await?
                        .is_empty(),
                    "this person has no company roles; add them with person add first"
                );
            }
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
