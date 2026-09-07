use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

pub const MAI: &str = "microsoft/mai-transcribe-2";

#[derive(Clone, Serialize)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub provider: String,
}
#[derive(Clone)]
pub struct Audio {
    pub bytes: Vec<u8>,
    pub format: String,
}
#[derive(Debug, Deserialize)]
pub struct Transcript {
    pub text: String,
}

#[async_trait]
pub trait SttProvider: Send + Sync {
    fn models(&self) -> Vec<Model>;
    async fn transcribe(
        &self,
        model: &str,
        audio: Audio,
        key: &str,
        cancel: CancellationToken,
    ) -> Result<Transcript>;
}

pub struct Registry {
    providers: BTreeMap<String, Arc<dyn SttProvider>>,
}
impl Default for Registry {
    fn default() -> Self {
        let mut registry = Self {
            providers: BTreeMap::new(),
        };
        registry.register("openrouter", Arc::new(OpenRouter::default()));
        registry
    }
}
impl Registry {
    pub fn register(&mut self, id: &str, provider: Arc<dyn SttProvider>) {
        self.providers.insert(id.into(), provider);
    }
    pub fn models(&self) -> Vec<Model> {
        self.providers.values().flat_map(|p| p.models()).collect()
    }
    pub fn resolve(&self, model: &str) -> Result<Arc<dyn SttProvider>> {
        self.providers
            .values()
            .find(|p| p.models().iter().any(|m| m.id == model))
            .cloned()
            .context("Model unavailable")
    }
}

pub struct OpenRouter {
    client: reqwest::Client,
    endpoint: String,
}
impl Default for OpenRouter {
    fn default() -> Self {
        Self {
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(90))
                .build()
                .expect("HTTP client"),
            endpoint: "https://openrouter.ai/api/v1/audio/transcriptions".into(),
        }
    }
}
fn request(model: &str, audio: Audio) -> Value {
    json!({"model":model,"input_audio":{"data":STANDARD.encode(audio.bytes),"format":audio.format},"response_format":"json",
        "provider":{"options":{"azure":{"enhancedMode":{"enabled":true,"model":"MAI-Transcribe-2","modelOptions":{"transcribeStyle":"clean"}}}}}})
}
#[async_trait]
impl SttProvider for OpenRouter {
    fn models(&self) -> Vec<Model> {
        vec![Model {
            id: MAI.into(),
            name: "MAI Transcribe 2".into(),
            provider: "openrouter".into(),
        }]
    }
    async fn transcribe(
        &self,
        model: &str,
        audio: Audio,
        key: &str,
        cancel: CancellationToken,
    ) -> Result<Transcript> {
        anyhow::ensure!(model == MAI, "Model unavailable");
        anyhow::ensure!(
            !audio.bytes.is_empty() && audio.bytes.len() <= 64 * 1024 * 1024,
            "Audio is too large"
        );
        let operation = async {
            // Never blindly retry a billable upload. The caller owns explicit retries.
            let mut response = self
                .client
                .post(&self.endpoint)
                .bearer_auth(key)
                .json(&request(model, audio))
                .send()
                .await
                .context("Connection failed")?;
            match response.status().as_u16() {
                200..=299 => (),
                401 | 403 => bail!("API key rejected"),
                402 => bail!("OpenRouter balance required"),
                429 => bail!("Rate limit reached"),
                408 | 504 => bail!("Transcription timed out"),
                _ => bail!("Transcription failed ({})", response.status().as_u16()),
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                anyhow::ensure!(
                    bytes.len() + chunk.len() <= 4 * 1024 * 1024,
                    "Response is too large"
                );
                bytes.extend_from_slice(&chunk);
            }
            let mut transcript: Transcript =
                serde_json::from_slice(&bytes).context("Invalid transcription response")?;
            transcript.text = transcript.text.trim().to_owned();
            anyhow::ensure!(!transcript.text.is_empty(), "No speech detected");
            Ok(transcript)
        };
        tokio::select! { _ = cancel.cancelled() => bail!("Cancelled"), result = operation => result }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    #[tokio::test]
    async fn sends_clean_mai_request_and_reads_text() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/transcribe", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = vec![0; 16384];
            let mut n = 0;
            loop {
                n += stream.read(&mut bytes[n..]).await.unwrap();
                if let Some(i) = bytes[..n].windows(4).position(|w| w == b"\r\n\r\n") {
                    if let Ok(v) = serde_json::from_slice::<Value>(&bytes[i + 4..n]) {
                        assert_eq!(v["model"], MAI);
                        assert_eq!(
                            v["provider"]["options"]["azure"]["enhancedMode"]["modelOptions"]
                                ["transcribeStyle"],
                            "clean"
                        );
                        assert_eq!(v["input_audio"]["data"], "AQID");
                        break;
                    }
                }
            }
            let body = r#"{"text":"  Hello, world.  "}"#;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let p = OpenRouter {
            endpoint,
            ..Default::default()
        };
        let result = p
            .transcribe(
                MAI,
                Audio {
                    bytes: vec![1, 2, 3],
                    format: "wav".into(),
                },
                "test-only",
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.text, "Hello, world.");
        server.await.unwrap();
    }
    #[tokio::test]
    async fn cancelled_request_never_returns_transcript() {
        let c = CancellationToken::new();
        c.cancel();
        assert!(OpenRouter::default()
            .transcribe(
                MAI,
                Audio {
                    bytes: vec![1],
                    format: "wav".into()
                },
                "test-only",
                c
            )
            .await
            .is_err());
    }
}
