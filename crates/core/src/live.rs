use anyhow::{bail, ensure, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashSet, VecDeque},
    sync::Arc,
    time::Duration,
};
use tokio::{net::TcpStream, sync::mpsc};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{
        protocol::{frame::coding::CloseCode, WebSocketConfig},
        Message,
    },
    MaybeTlsStream, WebSocketStream,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveView {
    pub id: String,
    pub created_at: i64,
    pub phase: LivePhase,
    pub transcript: String,
    pub interim: String,
    pub revision: usize,
    pub summary: String,
    pub summary_revision: usize,
    pub summary_updated_at: Option<i64>,
    pub summarizing: bool,
    pub seconds: f64,
    pub error: Option<String>,
    pub summary_error: Option<String>,
}
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LivePhase {
    #[default]
    Idle,
    Connecting,
    Listening,
    Stopping,
    Done,
    Error,
    Cancelled,
}
impl LivePhase {
    pub fn active(self) -> bool {
        matches!(self, Self::Connecting | Self::Listening | Self::Stopping)
    }
}

#[derive(Default)]
struct Turn {
    id: String,
    partial: String,
    final_text: Option<String>,
}
#[derive(Default)]
pub struct TranscriptState {
    pending: VecDeque<Turn>,
    completed: HashSet<String>,
    active: Option<String>,
    pub text: String,
    pub revision: usize,
    pub speaking: bool,
}
impl TranscriptState {
    pub fn interim(&self) -> String {
        self.pending
            .iter()
            .map(|t| t.final_text.as_deref().unwrap_or(&t.partial))
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    }
    pub fn complete(&self) -> bool {
        self.pending
            .iter()
            .all(|t| t.partial.is_empty() && t.final_text.is_none())
    }
    pub fn apply(&mut self, v: &Value) -> Result<bool> {
        let kind = v["type"].as_str().unwrap_or_default();
        if kind == "error" {
            bail!("Meta streaming error; the completed transcript has been kept");
        }
        if !matches!(
            kind,
            "speechStart" | "speechEnd" | "speechComplete" | "transcript"
        ) {
            return Ok(false);
        }
        let id = match &v["turnId"] {
            Value::String(s) if !s.is_empty() => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Null if kind == "transcript" => {
                if v["transcript"] == "" && self.active.is_none() {
                    return Ok(false);
                }
                self.active
                    .clone()
                    .context("Meta transcript is missing its speech turn")?
            }
            _ => bail!("Invalid Meta speech turn"),
        };
        if self.completed.contains(&id) {
            return Ok(false);
        }
        if !self.pending.iter().any(|t| t.id == id) {
            ensure!(
                self.pending.len() < 100,
                "Meta has too many unfinished speech turns"
            );
            self.pending.push_back(Turn {
                id: id.clone(),
                ..Default::default()
            });
        }
        let turn = self.pending.iter_mut().find(|t| t.id == id).unwrap();
        match kind {
            "speechStart" => {
                self.active = Some(id);
                self.speaking = true;
            }
            "speechEnd" => {
                if self.active.as_ref() == Some(&id) {
                    self.active = None;
                    self.speaking = false;
                }
            }
            "transcript" => {
                let text = v["transcript"]
                    .as_str()
                    .context("Invalid Meta transcript")?;
                if turn.final_text.is_none() {
                    turn.partial = text.to_owned();
                }
            }
            "speechComplete" => {
                if turn.final_text.is_none() {
                    turn.final_text = Some(
                        v["transcript"]
                            .as_str()
                            .context("Invalid completed Meta transcript")?
                            .trim()
                            .to_owned(),
                    );
                }
                if self.active.as_ref() == Some(&id) {
                    self.active = None;
                    self.speaking = false;
                }
            }
            _ => unreachable!(),
        }
        while self.pending.front().is_some_and(|t| t.final_text.is_some()) {
            let turn = self.pending.pop_front().unwrap();
            let text = turn.final_text.unwrap();
            self.completed.insert(turn.id);
            if !text.is_empty() {
                if !self.text.is_empty() {
                    self.text.push_str("\n\n");
                }
                self.text.push_str(&text);
                self.revision += 1;
            }
        }
        Ok(true)
    }
}

pub struct MetaStream(WebSocketStream<MaybeTlsStream<TcpStream>>);
impl MetaStream {
    pub async fn connect(key: &str, cancel: &CancellationToken) -> Result<Self> {
        Self::connect_to("wss://api.meta.ai/v1/asr/realtime", key, cancel).await
    }
    async fn connect_to(endpoint: &str, key: &str, cancel: &CancellationToken) -> Result<Self> {
        let operation =
            async {
                let config = WebSocketConfig::default()
                    .max_message_size(Some(1024 * 1024))
                    .max_frame_size(Some(1024 * 1024));
                let (mut ws, _) = connect_async_with_config(endpoint, Some(config), false)
                    .await
                    .context("Could not connect to Meta")?;
                ws.send(Message::Text(json!({
                "authorization": {"accessToken": format!("Bearer {}", key.trim())},
                "model": "muse-voice-transcribe-1.0", "audioEncoding": "PCM_24KHZ",
                "mode": "ENDPOINTING", "partialMode": "CUMULATIVE", "emitAudioProgress": true,
                "zdrOverride": true
            }).to_string().into())).await.context("Meta handshake failed")?;
                let ack = ws
                    .next()
                    .await
                    .context("Meta closed during connection")?
                    .context("Meta handshake failed")?;
                let value: Value =
                    serde_json::from_str(ack.to_text().context("Invalid Meta handshake")?)
                        .context("Invalid Meta handshake")?;
                ensure!(
                    value["sessionId"].as_str().is_some_and(|s| !s.is_empty()),
                    "Meta rejected the session; check your API key and account access"
                );
                Ok(Self(ws))
            };
        tokio::select! { biased; _ = cancel.cancelled() => bail!("Cancelled"), r = tokio::time::timeout(Duration::from_secs(20), operation) => r.context("Meta connection timed out")? }
    }
    pub async fn run(
        self,
        mut audio: mpsc::Receiver<std::result::Result<Vec<u8>, String>>,
        cancel: CancellationToken,
        on_event: Arc<dyn Fn(Value) + Send + Sync>,
    ) -> Result<()> {
        let (mut writer, mut reader) = self.0.split();
        let ended = CancellationToken::new();
        let input_ended = ended.clone();
        let send = async move {
            while let Some(packet) = audio.recv().await {
                let bytes = packet.map_err(anyhow::Error::msg)?;
                tokio::time::timeout(
                    Duration::from_secs(5),
                    writer.send(Message::Binary(bytes.into())),
                )
                .await
                .context("Meta audio upload stalled")?
                .context("Meta audio connection lost")?;
            }
            input_ended.cancel();
            writer
                .send(Message::Text("{\"type\":\"endStream\"}".into()))
                .await
                .context("Could not finish Meta stream")?;
            Ok::<_, anyhow::Error>(())
        };
        let receive = async move {
            let drain_timeout = async {
                ended.cancelled().await;
                tokio::time::sleep(Duration::from_secs(30)).await;
            };
            tokio::pin!(drain_timeout);
            loop {
                let message = tokio::select! {
                    _ = &mut drain_timeout => bail!("Meta did not finish the last speech turn in time"),
                    m = reader.next() => m.context("Meta connection ended unexpectedly")?.context("Meta connection lost")?,
                };
                match message {
                    Message::Text(text) => {
                        let value: Value =
                            serde_json::from_str(&text).context("Invalid Meta stream response")?;
                        if value["type"] == "error" {
                            bail!("Meta streaming failed; completed text has been kept");
                        }
                        on_event(value);
                    }
                    Message::Close(frame) => {
                        ensure!(
                            ended.is_cancelled()
                                && frame.is_some_and(|f| f.code == CloseCode::Normal),
                            "Meta stream closed before transcription finished"
                        );
                        return Ok(());
                    }
                    _ => (),
                }
            }
        };
        tokio::select! { biased; _ = cancel.cancelled() => bail!("Cancelled"), r = async { tokio::try_join!(send, receive)?; Ok(()) } => r }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cumulative_partials_replace_and_finals_are_ordered_and_deduplicated() {
        let mut s = TranscriptState::default();
        s.apply(&json!({"type":"speechStart","turnId":1})).unwrap();
        s.apply(&json!({"type":"transcript","transcript":"We should"}))
            .unwrap();
        s.apply(&json!({"type":"transcript","transcript":"We should wait","final":true}))
            .unwrap();
        assert_eq!(s.text, "");
        assert_eq!(s.interim(), "We should wait");
        s.apply(&json!({"type":"speechStart","turnId":2})).unwrap();
        s.apply(&json!({"type":"speechComplete","turnId":2,"transcript":"Until Friday."}))
            .unwrap();
        assert!(s.text.is_empty());
        s.apply(&json!({"type":"speechComplete","turnId":1,"transcript":"We should wait."}))
            .unwrap();
        s.apply(&json!({"type":"speechComplete","turnId":1,"transcript":"Duplicate"}))
            .unwrap();
        assert_eq!(s.text, "We should wait.\n\nUntil Friday.");
        assert_eq!(s.revision, 2);
        assert_eq!(s.interim(), "");
        assert!(s.complete());
    }
    #[test]
    fn unfinished_speech_is_never_promoted_to_final() {
        let mut s = TranscriptState::default();
        s.apply(&json!({"type":"transcript","turnId":1,"transcript":"Good results, but"}))
            .unwrap();
        assert!(!s.complete());
        assert!(s.text.is_empty());
        assert_eq!(s.interim(), "Good results, but");
    }

    #[tokio::test]
    async fn streams_binary_audio_receives_partials_before_stop_and_drains_final_turn() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let handshake: Value =
                serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(handshake["model"], "muse-voice-transcribe-1.0");
            assert_eq!(handshake["authorization"]["accessToken"], "Bearer test-key");
            assert_eq!(handshake["audioEncoding"], "PCM_24KHZ");
            assert_eq!(handshake["mode"], "ENDPOINTING");
            assert_eq!(handshake["zdrOverride"], true);
            ws.send(Message::text("{\"sessionId\":\"test-session\"}"))
                .await
                .unwrap();
            assert_eq!(
                ws.next().await.unwrap().unwrap().into_data().as_ref(),
                &[1, 2, 3, 4]
            );
            ws.send(Message::text(
                "{\"type\":\"transcript\",\"turnId\":1,\"transcript\":\"Not finished\"}",
            ))
            .await
            .unwrap();
            let end: Value =
                serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(end["type"], "endStream");
            ws.send(Message::text(
                "{\"type\":\"speechComplete\",\"turnId\":1,\"transcript\":\"Now finished.\"}",
            ))
            .await
            .unwrap();
            ws.close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame {
                code: CloseCode::Normal,
                reason: "".into(),
            }))
            .await
            .unwrap();
        });
        let cancel = CancellationToken::new();
        let stream = MetaStream::connect_to(&endpoint, "test-key", &cancel)
            .await
            .unwrap();
        let (audio, input) = mpsc::channel(2);
        let (out, mut events) = mpsc::unbounded_channel();
        let running = tokio::spawn(stream.run(
            input,
            cancel,
            Arc::new(move |v| {
                out.send(v).unwrap();
            }),
        ));
        audio.send(Ok(vec![1, 2, 3, 4])).await.unwrap();
        let partial = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(partial["transcript"], "Not finished");
        drop(audio);
        let final_turn = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(final_turn["transcript"], "Now finished.");
        tokio::time::timeout(Duration::from_secs(2), running)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn cancel_interrupts_a_stalled_handshake() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let token = CancellationToken::new();
        let cancel = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancel.cancel();
        });
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            MetaStream::connect_to(&endpoint, "secret", &token),
        )
        .await
        .unwrap();
        assert_eq!(result.err().unwrap().to_string(), "Cancelled");
    }
}
