//! Cadpad cloud API client — image → CadQuery generation via cadpad-backend.
use crate::credentials::{Credentials, Secret};
use cadpad_core::{Error, Result};
use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

/// Hosted Cadpad API. Override with `CADPAD_API_URL` for local development.
const UPSTREAM_API_URL: &str = "https://api.cadpad.app";
/// Hosted Cadpad site used for browser sign-in. Override with `CADPAD_AUTH_URL`.
const UPSTREAM_AUTH_URL: &str = "https://cadpad.app";

#[derive(Clone, Debug)]
struct Endpoints {
    base_url: String,
    auth_frontend_url: String,
}

impl Endpoints {
    fn resolve() -> Self {
        Self {
            base_url: env_url("CADPAD_API_URL", UPSTREAM_API_URL),
            auth_frontend_url: env_url("CADPAD_AUTH_URL", UPSTREAM_AUTH_URL),
        }
    }
}

fn env_url(name: &str, fallback: &str) -> String {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| value.starts_with("http://") || value.starts_with("https://"))
        .unwrap_or_else(|| fallback.into())
}

#[derive(Clone, Deserialize, Debug)]
struct DeviceStartResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    expires_in: i64,
    #[serde(default)]
    interval: i64,
}

#[derive(Clone, Deserialize, Debug)]
struct DevicePollResponse {
    status: String,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct GenerationAsset {
    pub path: String,
    pub content_type: String,
    pub content_base64: String,
}

/// Runner-up ensemble result offered as a generation variant.
/// Same wrapped shape as the winning result (CadQuery source + assets).
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct GenerationAlternate {
    pub member: String,
    pub format: String,
    pub entry_point: String,
    pub source: String,
    #[serde(default)]
    pub assets: Vec<GenerationAsset>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Clone, Deserialize, Debug)]
pub struct GenerationResult {
    pub format: String,
    pub entry_point: String,
    pub source: String,
    #[serde(default)]
    pub assets: Vec<GenerationAsset>,
    #[serde(default)]
    pub alternates: Vec<GenerationAlternate>,
}

#[derive(Clone, Deserialize, Debug)]
struct JobResponse {
    id: String,
    status: String,
    #[serde(default)]
    result: Option<GenerationResult>,
    #[serde(default)]
    error: Option<String>,
}

pub struct CloudService {
    endpoints: Endpoints,
    credentials: Arc<dyn Credentials>,
    http: reqwest::Client,
}

impl CloudService {
    pub async fn open(credentials: Arc<dyn Credentials>) -> Result<Self> {
        Ok(Self {
            endpoints: Endpoints::resolve(),
            credentials,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .map_err(|error| Error::domain("NETWORK_ERROR", error.to_string()))?,
        })
    }

    pub async fn settings(&self) -> Result<Value> {
        let credentials = self.credentials.clone();
        let configured =
            tokio::task::spawn_blocking(move || credentials.get().map(|key| key.is_some()))
                .await
                .map_err(|_| {
                    Error::domain(
                        "CREDENTIAL_UNAVAILABLE",
                        "Could not access the system keychain",
                    )
                })??;
        Ok(json!({
            "base_url": self.endpoints.base_url,
            "configured": configured,
            "credential_storage": "system_keychain",
        }))
    }

    /// Save a Cadpad API key (`cpd_…`) to the system keychain.
    async fn save_key(&self, api_key: String) -> Result<Value> {
        let secret = Secret::new(api_key)?;
        if !secret.expose().starts_with("cpd_") {
            return Err(Error::domain(
                "INVALID_PARAMETER",
                "Cadpad API keys start with cpd_",
            ));
        }
        let credentials = self.credentials.clone();
        tokio::task::spawn_blocking(move || credentials.set(secret))
            .await
            .map_err(|_| Error::domain("CREDENTIAL_UNAVAILABLE", "Could not save the API key"))??;
        self.settings().await
    }

    /// Start browser login: open the Cadpad site, poll until an API key is minted.
    pub async fn sign_in_with_browser(&self) -> Result<Value> {
        let Endpoints {
            base_url,
            auth_frontend_url,
        } = self.endpoints.clone();
        let response = self
            .http
            .post(format!("{base_url}/v1/auth/device"))
            .send()
            .await
            .map_err(network)?;
        let status = response.status();
        let body = response.bytes().await.map_err(network)?;
        if !status.is_success() {
            return Err(api_error(status.as_u16(), &body));
        }
        let started: DeviceStartResponse = serde_json::from_slice(&body).map_err(|error| {
            Error::domain(
                "NETWORK_ERROR",
                format!("Invalid device login response: {error}"),
            )
        })?;
        let mut verification = started.verification_uri.trim().to_string();
        if verification.is_empty() {
            verification = format!(
                "{}/auth/desktop?code={}",
                auth_frontend_url.trim_end_matches('/'),
                started.user_code
            );
        }
        open_browser(&verification)?;

        let interval = if started.interval > 0 {
            Duration::from_secs(started.interval as u64)
        } else {
            Duration::from_secs(2)
        };
        let deadline = if started.expires_in > 0 {
            tokio::time::Instant::now() + Duration::from_secs(started.expires_in as u64)
        } else {
            tokio::time::Instant::now() + Duration::from_secs(600)
        };
        let poll = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|error| Error::domain("NETWORK_ERROR", error.to_string()))?;

        loop {
            if tokio::time::Instant::now() > deadline {
                return Err(Error::domain(
                    "TIMEOUT",
                    "Timed out waiting for browser sign-in. Try again from Settings.",
                ));
            }
            tokio::time::sleep(interval).await;
            let response = poll
                .get(format!("{base_url}/v1/auth/device/{}", started.device_code))
                .send()
                .await
                .map_err(network)?;
            let status = response.status();
            let body = response.bytes().await.map_err(network)?;
            if status.as_u16() == 404 {
                return Err(Error::domain(
                    "AUTH_FAILED",
                    "Login session expired. Start sign-in again from Settings.",
                ));
            }
            if !status.is_success() {
                return Err(api_error(status.as_u16(), &body));
            }
            let poll_body: DevicePollResponse = serde_json::from_slice(&body).map_err(|error| {
                Error::domain(
                    "NETWORK_ERROR",
                    format!("Invalid device poll response: {error}"),
                )
            })?;
            match poll_body.status.as_str() {
                "pending" => continue,
                "expired" => {
                    return Err(Error::domain(
                        "AUTH_FAILED",
                        "Login code expired. Start sign-in again from Settings.",
                    ));
                }
                "authorized" => {
                    let api_key = poll_body.api_key.ok_or_else(|| {
                        Error::domain("AUTH_FAILED", "Authorized response missing API key")
                    })?;
                    self.save_key(api_key).await?;
                    return Ok(json!({
                        "configured": true,
                        "user_code": started.user_code,
                        "account_id": poll_body.account_id,
                        "verification_uri": verification,
                    }));
                }
                other => {
                    return Err(Error::domain(
                        "NETWORK_ERROR",
                        format!("Unexpected device login status: {other}"),
                    ));
                }
            }
        }
    }

    pub async fn forget_key(&self) -> Result<()> {
        let credentials = self.credentials.clone();
        tokio::task::spawn_blocking(move || credentials.delete())
            .await
            .map_err(|_| {
                Error::domain(
                    "CREDENTIAL_UNAVAILABLE",
                    "Could not remove the API key from the system keychain",
                )
            })?
    }

    /// Signed-in account: profile (email, name) and credit balance from `GET /v1/account`.
    pub async fn account(&self) -> Result<Value> {
        let api_key = self.api_key().await?;
        let response = self
            .http
            .get(format!("{}/v1/account", self.endpoints.base_url))
            .header("Authorization", format!("Bearer {}", api_key.expose()))
            .send()
            .await
            .map_err(network)?;
        let status = response.status();
        let body = response.bytes().await.map_err(network)?;
        if !status.is_success() {
            return Err(api_error(status.as_u16(), &body));
        }
        serde_json::from_slice(&body).map_err(|error| {
            Error::domain(
                "NETWORK_ERROR",
                format!("Invalid account response: {error}"),
            )
        })
    }

    /// Upload an image, poll until the job finishes, return CadQuery source.
    pub async fn generate_from_image(
        &self,
        image: Vec<u8>,
        mime_type: String,
        prompt: Option<String>,
        filename: Option<String>,
    ) -> Result<Value> {
        if image.is_empty() {
            return Err(Error::domain("INVALID_PARAMETER", "Image is empty"));
        }
        if image.len() > 15 << 20 {
            return Err(Error::domain(
                "INVALID_PARAMETER",
                "Image exceeds the 15 MiB upload limit",
            ));
        }
        let api_key = self.api_key().await?;
        let base_url = self.endpoints.base_url.clone();
        let mime = if mime_type.trim().is_empty() {
            "image/png".into()
        } else {
            mime_type
        };
        let name = filename.unwrap_or_else(|| "image.png".into());
        let part = Part::bytes(image)
            .file_name(name)
            .mime_str(&mime)
            .map_err(|error| Error::domain("INVALID_PARAMETER", error.to_string()))?;
        let mut form = Form::new().part("image", part);
        if let Some(prompt) = prompt
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            form = form.text("prompt", prompt);
        }
        let response = self
            .http
            .post(format!("{base_url}/v1/generate"))
            .header("Authorization", format!("Bearer {}", api_key.expose()))
            .multipart(form)
            .send()
            .await
            .map_err(network)?;
        let status = response.status();
        let body = response.bytes().await.map_err(network)?;
        if !status.is_success() {
            return Err(api_error(status.as_u16(), &body));
        }
        let created: Value = serde_json::from_slice(&body).map_err(|error| {
            Error::domain(
                "NETWORK_ERROR",
                format!("Invalid generate response: {error}"),
            )
        })?;
        let job_id = created
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::domain("NETWORK_ERROR", "Generate response missing job id"))?
            .to_string();

        let poll = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|error| Error::domain("NETWORK_ERROR", error.to_string()))?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(200);
        loop {
            if tokio::time::Instant::now() > deadline {
                return Err(Error::domain(
                    "TIMEOUT",
                    "Timed out waiting for image→CAD generation",
                ));
            }
            let response = poll
                .get(format!("{base_url}/v1/jobs/{job_id}"))
                .header("Authorization", format!("Bearer {}", api_key.expose()))
                .send()
                .await
                .map_err(network)?;
            let status = response.status();
            let body = response.bytes().await.map_err(network)?;
            if !status.is_success() {
                return Err(api_error(status.as_u16(), &body));
            }
            let job: JobResponse = serde_json::from_slice(&body).map_err(|error| {
                Error::domain("NETWORK_ERROR", format!("Invalid job response: {error}"))
            })?;
            match job.status.as_str() {
                "succeeded" => {
                    let result = job.result.ok_or_else(|| {
                        Error::domain("NETWORK_ERROR", "Succeeded job missing result")
                    })?;
                    if result.source.trim().is_empty() {
                        return Err(Error::domain(
                            "NETWORK_ERROR",
                            "Generation returned empty CadQuery source",
                        ));
                    }
                    return Ok(json!({
                        "job_id": job.id,
                        "format": result.format,
                        "entry_point": result.entry_point,
                        "source": result.source,
                        "assets": result.assets,
                        "alternates": result.alternates,
                    }));
                }
                "failed" => {
                    return Err(Error::domain(
                        "GENERATION_FAILED",
                        job.error.unwrap_or_else(|| "Generation failed".into()),
                    ));
                }
                "queued" | "running" => {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                }
                other => {
                    return Err(Error::domain(
                        "NETWORK_ERROR",
                        format!("Unexpected job status: {other}"),
                    ));
                }
            }
        }
    }

    async fn api_key(&self) -> Result<Secret> {
        let credentials = self.credentials.clone();
        tokio::task::spawn_blocking(move || credentials.get())
            .await
            .map_err(|_| {
                Error::domain(
                    "CREDENTIAL_UNAVAILABLE",
                    "Could not access the system keychain",
                )
            })?
            .and_then(|key| {
                key.ok_or_else(|| {
                    Error::domain("LLM_NOT_CONFIGURED", "Sign in to Cadpad Cloud in Settings")
                })
            })
    }
}

fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map_err(|error| {
                Error::domain("IO_ERROR", format!("Could not open the browser: {error}"))
            })?;
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map_err(|error| {
                Error::domain("IO_ERROR", format!("Could not open the browser: {error}"))
            })?;
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .map_err(|error| {
                Error::domain("IO_ERROR", format!("Could not open the browser: {error}"))
            })?;
        return Ok(());
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = url;
        Err(Error::domain(
            "UNSUPPORTED_OPERATION",
            "Browser sign-in is not supported on this platform",
        ))
    }
}

fn network(error: reqwest::Error) -> Error {
    Error::domain("NETWORK_ERROR", error.to_string())
}

fn api_error(status: u16, body: &[u8]) -> Error {
    if let Ok(value) = serde_json::from_slice::<Value>(body)
        && let Some(message) = value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .filter(|message| !message.is_empty())
    {
        return Error::domain("API_ERROR", format!("HTTP {status}: {message}"));
    }
    let preview = String::from_utf8_lossy(body);
    let preview = preview.chars().take(240).collect::<String>();
    Error::domain("API_ERROR", format!("HTTP {status}: {preview}"))
}
