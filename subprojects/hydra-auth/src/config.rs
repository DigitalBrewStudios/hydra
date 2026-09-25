use clap::Parser;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Parser, Deserialize)]
#[command(name = "hydra-auth", about = "Hydra Authentification Backend.")]
pub struct Cli {
    /// Config path
    #[clap(short, long, default_value = "auth.toml")]
    pub config_path: String,
}

impl Default for Cli {
    fn default() -> Self {
        Self::new()
    }
}

impl Cli {
    #[must_use]
    pub fn new() -> Self {
        Self::parse()
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderType {
    #[serde(rename = "Github", alias = "GitHub")]
    Github,
    Oidc,
    Ldap,
    Saml,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    #[serde(rename = "type")]
    r_type: ProviderType,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    hydra_login_text: Option<String>,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    client_secret: Option<secrecy::SecretString>,
    #[serde(default)]
    client_secret_path: Option<std::path::PathBuf>,
    #[serde(default)]
    token_path: Option<std::path::PathBuf>,
}

#[derive(Debug)]
pub struct Provider {
    pub name: String,
    pub login_text: String,
    pub kind: ProviderKind,
}

#[derive(Debug)]
pub enum ProviderKind {
    Github(Github),
}

#[derive(Debug)]
pub struct Github {
    pub client_id: String,
    pub client_secret: Option<secrecy::SecretString>,
    pub client_secret_path: Option<std::path::PathBuf>,
    pub token_path: Option<std::path::PathBuf>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    db_url: secrecy::SecretString,
    #[serde(default = "default_max_db_connections")]
    max_db_connections: u32,
    providers: HashMap<String, ProviderConfig>,
}

fn default_max_db_connections() -> u32 {
    4
}

impl From<AppConfig> for App {
    fn from(val: AppConfig) -> Self {
        let providers = val
            .providers
            .into_iter()
            .map(|(key, value)| {
                let name = value.name.unwrap_or_else(|| key.clone());
                let login_text =
                    value.hydra_login_text.unwrap_or_else(|| format!("Login with {name}"));
                let kind = match value.r_type {
                    ProviderType::Github => ProviderKind::Github(Github {
                        client_id: value.client_id.unwrap_or_default(),
                        client_secret: value.client_secret,
                        client_secret_path: value.client_secret_path,
                        token_path: value.token_path,
                    }),
                };
                (key, Provider { name, login_text, kind })
            })
            .collect();

        Self {
            db_url: std::env::var("HYDRA_DATABASE_URL")
                .map(secrecy::SecretString::from)
                .unwrap_or(val.db_url),
            max_db_connections: val.max_db_connections,
            providers,
        }
    }
}

#[derive(Debug)]
pub struct App {
    pub db_url: secrecy::SecretString,
    pub max_db_connections: u32,
    pub providers: HashMap<String, Provider>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Failed to parse TOML from '{path}': {source}")]
    ParseToml {
        path: String,
        source: toml::de::Error,
    },

    #[error("Failed to parse default config: {0}")]
    ParseDefault(toml::de::Error),

    #[error("Failed to read config from '{path}': {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
}

impl App {
    #[tracing::instrument(err)]
    pub fn init(filepath: &str) -> Result<Self, ConfigError> {
        tracing::info!("Trying to load file: {filepath}");
        let toml: AppConfig = match fs_err::read_to_string(filepath) {
            Ok(content) => toml::from_str(&content).map_err(|e| ConfigError::ParseToml {
                path: filepath.to_string(),
                source: e,
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!("no config file found at '{filepath}'! Using default config");
                toml::from_str("").map_err(ConfigError::ParseDefault)?
            }
            Err(e) => {
                return Err(ConfigError::Read {
                    path: filepath.to_string(),
                    source: e,
                });
            }
        };
        tracing::info!("Loaded config: {toml:?}");
        Ok(toml.into())
    }
}

#[cfg(test)]
mod tests {
    use super::App;

    fn parse(toml: &str) -> App {
        let dir = tempfile::tempdir().expect("failed to create tempdir");
        let path = dir.path().join("auth.toml");
        std::fs::write(&path, toml).expect("failed to write temp config");
        App::init(path.to_str().expect("temp path is utf-8")).expect("failed to parse config")
    }

    #[test]
    fn parses_repo_config() {
        let app = parse(
            r#"
dbUrl = ""

[providers.github]
name = "Github"
type = "Github"
clientId = ""
clientSecret = ""
"#,
        );
        assert_eq!(app.max_db_connections, 4);
        let gh = app.providers.get("github").unwrap();
        assert_eq!(gh.name, "Github");
        assert_eq!(gh.login_text, "Login with Github");
        let super::ProviderKind::Github(g) = &gh.kind;
        assert_eq!(g.client_id, "");
        assert!(g.client_secret.is_some());
    }

    #[test]
    fn parses_module_style_config() {
        let app = parse(
            r#"
dbUrl = "postgres://localhost/hydra"
maxDbConnections = 8

[providers.github]
type = "GitHub"
clientId = "abc"
clientSecretPath = "/run/secrets/gh"
"#,
        );
        assert_eq!(app.max_db_connections, 8);
        let gh = app.providers.get("github").unwrap();
        assert_eq!(gh.name, "github");
        assert_eq!(gh.login_text, "Login with github");
        let super::ProviderKind::Github(g) = &gh.kind;
        assert_eq!(g.client_id, "abc");
        assert!(g.client_secret.is_none());
    }
}
