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

        // Admin token: when set, enables /api/admin/* endpoints.
        // When absent (default), admin routes return 404 for every request.
        let admin_token = env::var("ADMIN_TOKEN")
            .ok()
            .filter(|t| !t.is_empty());

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
        })
    }
}
