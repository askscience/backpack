use std::env;

#[derive(Clone, Debug)]
pub struct Config {
    pub llm_provider: String,
    pub llm_api_key: String,
    pub llm_model: String,
    pub llm_endpoint: String,
    pub embedding_model: String,
    #[allow(dead_code)]
    pub embedding_dim: usize,
    pub max_file_size_mb: u64,
    pub upload_dir: String,
    pub db_path: String,
    pub skill_path: String,
    #[allow(dead_code)]
    pub vosk_model_path: String,
    /// Bearer token for admin API endpoints (spaces CRUD). When `None`,
    /// all `/api/admin/*` routes return 404, revealing no information.
    pub admin_token: Option<String>,
    /// Whether `admin_token` was auto-generated (no `ADMIN_TOKEN` env set).
    /// A generated token is ephemeral, so it is printed once at startup;
    /// an operator-supplied token is never logged in full.
    pub admin_token_generated: bool,
    /// WebAuthn RP ID (e.g. "localhost" or "backpack.example.com")
    pub webauthn_rp_id: String,
    /// WebAuthn RP origin (e.g. "http://localhost:8080")
    pub webauthn_origin: String,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let llm_provider = env::var("LLM_PROVIDER").unwrap_or_else(|_| "openai".into());
        let llm_api_key = env::var("LLM_API_KEY").unwrap_or_default();
        let llm_model = env::var("LLM_MODEL").unwrap_or_else(|_| {
            match llm_provider.as_str() {
                "anthropic" => "claude-3-haiku-20240307".into(),
                _ => "gpt-4o-mini".into(),
            }
        });
        let llm_endpoint = env::var("LLM_ENDPOINT").unwrap_or_else(|_| {
            match llm_provider.as_str() {
                "openai" => "https://api.openai.com/v1".into(),
                "anthropic" => "https://api.anthropic.com/v1".into(),
                "ollama" => "http://localhost:11434".into(),
                _ => "https://api.openai.com/v1".into(),
            }
        });
        let embedding_model = env::var("EMBEDDING_MODEL").unwrap_or_else(|_| {
            match llm_provider.as_str() {
                "ollama" => "nomic-embed-text".into(),
                _ => "text-embedding-3-small".into(),
            }
        });
        let embedding_dim: usize = env::var("EMBEDDING_DIM")
            .unwrap_or_else(|_| "1536".into())
            .parse()
            .unwrap_or(1536);

        // Admin token: when set from env, uses that; otherwise generates a random one.
        // The generated token is printed at startup so admins can use the UI.
        let env_admin_token = env::var("ADMIN_TOKEN").ok().filter(|t| !t.is_empty());
        let admin_token_generated = env_admin_token.is_none();
        let admin_token = Some(env_admin_token.unwrap_or_else(|| {
            use uuid::Uuid;
            format!("bp-admin-{}", Uuid::new_v4())
        }));

        Ok(Config {
            llm_provider,
            llm_api_key,
            llm_model,
            llm_endpoint,
            embedding_model,
            embedding_dim,
            max_file_size_mb: env::var("MAX_FILE_SIZE_MB")
                .unwrap_or_else(|_| "100".into())
                .parse()
                .unwrap_or(100),
            upload_dir: env::var("UPLOAD_DIR").unwrap_or_else(|_| "./uploads".into()),
            db_path: env::var("DB_PATH").unwrap_or_else(|_| "./data/backpack.db".into()),
            skill_path: env::var("SKILL_PATH").unwrap_or_else(|_| "./skill.md".into()),
            vosk_model_path: env::var("VOSK_MODEL_PATH").unwrap_or_else(|_| "/opt/vosk-model".into()),
            admin_token,
            admin_token_generated,
            webauthn_rp_id: env::var("WEBAUTHN_RP_ID").unwrap_or_else(|_| "localhost".into()),
            webauthn_origin: env::var("WEBAUTHN_ORIGIN").unwrap_or_else(|_| "http://localhost:8080".into()),
        })
    }
}
