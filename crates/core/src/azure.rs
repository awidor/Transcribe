use anyhow::{anyhow, bail, ensure, Context, Result};
use futures_util::{
    stream::{FuturesUnordered, SplitSink},
    SinkExt, StreamExt,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::{net::TcpStream, sync::watch};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{
        client::IntoClientRequest, http::HeaderValue, protocol::WebSocketConfig, Error, Message,
    },
    MaybeTlsStream, WebSocketStream,
};
use tokio_util::sync::CancellationToken;

use crate::audio::{AudioReceiver, NoSpeech, SAMPLE_RATE};

/// Selected per request, so the resource needs no deployment.
const MODEL: &str = "mai-transcribe-2-streaming";
/// A fifth of a second of audio; anything shorter is treated as silence.
const MIN_BYTES: usize = SAMPLE_RATE * 2 / 5;
/// Packets are batched up to a tenth of a second per message.
const CHUNK_BYTES: usize = SAMPLE_RATE * 2 / 10;

pub struct Config {
    pub region: String,
    pub key: String,
}

/// Text Azure has confirmed, followed by its current guess at what comes next.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Words {
    pub text: String,
    pub interim: String,
}
impl Words {
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.interim.trim().is_empty()
    }
}

// shortcut: a fixed list of public Azure regions; add new ones as Azure opens them.
const REGIONS: &[&str] = &[
    "australiaeast",
    "australiasoutheast",
    "brazilsouth",
    "canadacentral",
    "canadaeast",
    "centralindia",
    "centralus",
    "chilecentral",
    "eastasia",
    "eastus",
    "eastus2",
    "francecentral",
    "germanywestcentral",
    "indonesiacentral",
    "israelcentral",
    "italynorth",
    "japaneast",
    "japanwest",
    "jioindiawest",
    "koreacentral",
    "malaysiawest",
    "mexicocentral",
    "newzealandnorth",
    "northcentralus",
    "northeurope",
    "norwayeast",
    "polandcentral",
    "qatarcentral",
    "southafricanorth",
    "southcentralus",
    "southeastasia",
    "southindia",
    "spaincentral",
    "swedencentral",
    "switzerlandnorth",
    "switzerlandwest",
    "uaenorth",
    "uksouth",
    "ukwest",
    "westcentralus",
    "westeurope",
    "westus",
    "westus2",
    "westus3",
];

/// The region an Azure key belongs to. A key is accepted only by its own
/// resource's region, so no endpoint needs to be entered.
pub async fn region(key: &str) -> Result<String> {
    find_region(
        key,
        REGIONS.iter().map(|region| {
            (
                region.to_string(),
                format!("https://{region}.api.cognitive.microsoft.com/sts/v1.0/issueToken"),
            )
        }),
    )
    .await
}

async fn find_region(
    key: &str,
    probes: impl IntoIterator<Item = (String, String)>,
) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let mut pending: FuturesUnordered<_> = probes
        .into_iter()
        .map(|(region, url)| {
            let request = client
                .post(url)
                .header("Ocp-Apim-Subscription-Key", key.trim())
                .body("")
                .send();
            async move { (region, request.await.map(|r| r.status())) }
        })
        .collect();
    let mut reached = false;
    while let Some((region, status)) = pending.next().await {
        match status {
            Ok(status) if status.is_success() => return Ok(region),
            Ok(_) => reached = true,
            Err(_) => (),
        }
    }
    if reached {
        bail!("Azure API key rejected")
    }
    bail!("Could not connect to Azure")
}

fn id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn text_message(path: &str, request: &str, body: Value) -> Message {
    Message::text(format!(
        "Path: {path}\r\nX-RequestId: {request}\r\nContent-Type: application/json\r\n\r\n{body}"
    ))
}

/// Audio frames carry their headers behind a two-byte length.
fn audio_message(request: &str, body: &[u8], content_type: bool) -> Message {
    let headers = format!(
        "Path: audio\r\nX-RequestId: {request}\r\n{}",
        if content_type {
            "Content-Type: audio/x-wav\r\n"
        } else {
            ""
        }
    );
    let mut bytes = Vec::with_capacity(2 + headers.len() + body.len());
    bytes.extend((headers.len() as u16).to_be_bytes());
    bytes.extend(headers.as_bytes());
    bytes.extend(body);
    Message::binary(bytes)
}

/// A WAV header announcing mono 16-bit PCM at `SAMPLE_RATE`.
fn wave_header() -> Vec<u8> {
    let mut header = Vec::with_capacity(44);
    header.extend(b"RIFF");
    header.extend(0u32.to_le_bytes());
    header.extend(b"WAVEfmt ");
    header.extend(16u32.to_le_bytes());
    header.extend(1u16.to_le_bytes());
    header.extend(1u16.to_le_bytes());
    header.extend((SAMPLE_RATE as u32).to_le_bytes());
    header.extend((SAMPLE_RATE as u32 * 2).to_le_bytes());
    header.extend(2u16.to_le_bytes());
    header.extend(16u16.to_le_bytes());
    header.extend(b"data");
    header.extend(0u32.to_le_bytes());
    header
}

/// The path and JSON body of a service message.
fn parse(text: &str) -> Result<(String, Value)> {
    let (headers, body) = text.split_once("\r\n\r\n").unwrap_or((text, ""));
    let path = headers
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("path"))
        .map(|(_, value)| value.trim().to_ascii_lowercase())
        .context("Invalid Azure response")?;
    let body = if body.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(body).context("Invalid Azure response")?
    };
    Ok((path, body))
}

fn connection_error(error: Error) -> anyhow::Error {
    match error {
        Error::Http(response) => match response.status().as_u16() {
            401 | 403 => anyhow!("Azure API key rejected"),
            429 => anyhow!("Azure rate limit reached"),
            status => anyhow!("Azure connection failed ({status})"),
        },
        _ => anyhow!("Could not connect to Azure"),
    }
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn send(writer: &mut SplitSink<Socket, Message>, message: Message) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(10), writer.send(message))
        .await
        .context("Azure upload stalled")?
        .context("Azure connection lost")
}

pub struct Stream(Socket);
impl Stream {
    pub async fn connect(config: &Config, cancel: &CancellationToken) -> Result<Self> {
        let url = format!(
            "wss://{}.stt.speech.microsoft.com/stt/speech/universal/v2?format=simple",
            config.region
        );
        Self::connect_to(&url, &config.key, cancel).await
    }
    async fn connect_to(url: &str, key: &str, cancel: &CancellationToken) -> Result<Self> {
        let operation = async {
            let mut request = url.into_client_request()?;
            let headers = request.headers_mut();
            headers.insert(
                "Ocp-Apim-Subscription-Key",
                HeaderValue::from_str(key.trim()).context("Invalid Azure API key")?,
            );
            headers.insert("X-ConnectionId", HeaderValue::from_str(&id())?);
            let config = WebSocketConfig::default()
                .max_message_size(Some(1024 * 1024))
                .max_frame_size(Some(1024 * 1024));
            let (ws, _) = connect_async_with_config(request, Some(config), false)
                .await
                .map_err(connection_error)?;
            Ok(Self(ws))
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("Cancelled"),
            r = tokio::time::timeout(Duration::from_secs(20), operation) => r.context("Azure connection timed out")?,
        }
    }

    /// Sends audio until the receiver closes, keeping `words` current, then
    /// returns the final transcript and the seconds of audio it covers.
    pub async fn run(
        self,
        mut audio: AudioReceiver,
        words: &watch::Sender<Words>,
        cancel: &CancellationToken,
    ) -> Result<(String, f64)> {
        let (mut writer, mut reader) = self.0.split();
        let request = id();
        let ended = CancellationToken::new();
        let send = async {
            for message in [
                text_message(
                    "speech.config",
                    &request,
                    json!({"context": {"system": {"name": "Transcribe", "version": env!("CARGO_PKG_VERSION")}}}),
                ),
                text_message(
                    "speech.context",
                    &request,
                    json!({"phraseDetection": {"mode": "Conversation"}, "phraseOutput": {}, "model": {"name": MODEL}}),
                ),
                audio_message(&request, &wave_header(), true),
            ] {
                send(&mut writer, message).await?;
            }
            let mut sent = 0;
            while let Some(packet) = audio.recv().await {
                let mut bytes = packet.map_err(anyhow::Error::msg)?;
                while bytes.len() < CHUNK_BYTES {
                    match audio.try_recv() {
                        Ok(packet) => bytes.extend(packet.map_err(anyhow::Error::msg)?),
                        Err(_) => break,
                    }
                }
                if bytes.is_empty() {
                    continue;
                }
                sent += bytes.len();
                send(&mut writer, audio_message(&request, &bytes, false)).await?;
            }
            ensure!(sent >= MIN_BYTES, NoSpeech);
            // An empty audio frame ends the stream; Azure then finishes the last phrase.
            send(&mut writer, audio_message(&request, &[], false)).await?;
            ended.cancel();
            Ok(sent)
        };
        let receive = async {
            let drain = async {
                ended.cancelled().await;
                tokio::time::sleep(Duration::from_secs(30)).await;
            };
            tokio::pin!(drain);
            loop {
                let message = tokio::select! {
                    _ = &mut drain => bail!("Azure did not finish the transcript in time"),
                    m = reader.next() => m.context("Azure closed the connection")?.context("Azure connection lost")?,
                };
                let text = match message {
                    Message::Text(text) => text,
                    Message::Close(frame) => match frame
                        .map(|f| f.reason.to_string())
                        .filter(|r| !r.trim().is_empty())
                    {
                        Some(reason) => bail!("Azure: {}", reason.trim()),
                        None => bail!("Azure closed the connection"),
                    },
                    _ => continue,
                };
                let (path, body) = parse(&text)?;
                match path.as_str() {
                    "speech.hypothesis" => {
                        let guess = body["Text"].as_str().unwrap_or_default();
                        words.send_if_modified(|w| {
                            let interim = if w.text.is_empty() {
                                guess.to_owned()
                            } else {
                                format!(" {guess}")
                            };
                            let changed = w.interim != interim;
                            w.interim = interim;
                            changed
                        });
                    }
                    "speech.phrase" => match body["RecognitionStatus"].as_str().unwrap_or_default()
                    {
                        "Success" => {
                            let phrase = body["DisplayText"].as_str().unwrap_or_default().trim();
                            words.send_modify(|w| {
                                if !phrase.is_empty() {
                                    if !w.text.is_empty() {
                                        w.text.push(' ');
                                    }
                                    w.text.push_str(phrase);
                                }
                                w.interim.clear();
                            });
                        }
                        "Error" => bail!("Azure transcription failed"),
                        // Silence, and the end of the stream.
                        _ => (),
                    },
                    "turn.end" if ended.is_cancelled() => {
                        return Ok(words.borrow().text.trim().to_owned())
                    }
                    "turn.end" => bail!("Azure stopped transcribing"),
                    _ => (),
                }
            }
        };
        let (sent, text) = tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("Cancelled"),
            r = async { tokio::try_join!(send, receive) } => r?,
        };
        ensure!(!text.is_empty(), NoSpeech);
        Ok((text, sent as f64 / (SAMPLE_RATE * 2) as f64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;
    use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};

    #[tokio::test]
    async fn a_key_finds_the_one_region_that_accepts_it() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut tcp, _) = listener.accept().await.unwrap();
                let mut request = [0; 2048];
                let n = tcp.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..n]).to_lowercase();
                assert!(request.contains("ocp-apim-subscription-key: test-key"));
                let status = if request.starts_with("post /home ") {
                    "200 OK"
                } else {
                    "401 Unauthorized"
                };
                let _ = tcp
                    .write_all(
                        format!(
                            "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await;
            }
        });
        let probe = |region: &str| (region.to_string(), format!("http://{address}/{region}"));
        assert_eq!(
            find_region(
                "test-key",
                [
                    probe("away"),
                    probe("home"),
                    ("offline".into(), "http://127.0.0.1:1/".into())
                ]
            )
            .await
            .unwrap(),
            "home"
        );
        assert_eq!(
            find_region("test-key", [probe("away")])
                .await
                .unwrap_err()
                .to_string(),
            "Azure API key rejected"
        );
        assert_eq!(
            find_region(
                "test-key",
                [("offline".to_string(), "http://127.0.0.1:1/".to_string())]
            )
            .await
            .unwrap_err()
            .to_string(),
            "Could not connect to Azure"
        );
    }

    async fn server(
        script: impl FnOnce(Socket) -> futures_util::future::BoxFuture<'static, ()> + Send + 'static,
    ) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let ws = tokio_tungstenite::accept_hdr_async(
                MaybeTlsStream::Plain(tcp),
                |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
                    assert_eq!(request.headers()["Ocp-Apim-Subscription-Key"], "test-key");
                    Ok(response)
                },
            )
            .await
            .unwrap();
            script(ws).await;
        });
        url
    }

    /// Reads a client frame as its path and body; audio bodies are returned raw.
    async fn receive(ws: &mut Socket) -> (String, Vec<u8>) {
        match ws.next().await.unwrap().unwrap() {
            Message::Text(text) => {
                let (path, body) = parse(&text).unwrap();
                (path, body.to_string().into_bytes())
            }
            Message::Binary(bytes) => {
                let length = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
                let headers = std::str::from_utf8(&bytes[2..2 + length]).unwrap();
                assert!(headers.starts_with("Path: audio\r\n"));
                ("audio".into(), bytes[2 + length..].to_vec())
            }
            other => panic!("Unexpected {other:?}"),
        }
    }

    fn reply(path: &str, body: Value) -> Message {
        Message::text(format!("X-RequestId: r\r\nContent-Type: application/json; charset=utf-8\r\nPath: {path}\r\n\r\n{body}"))
    }

    async fn setup(ws: &mut Socket) {
        assert_eq!(receive(ws).await.0, "speech.config");
        let (path, context) = receive(ws).await;
        assert_eq!(path, "speech.context");
        let context: Value = serde_json::from_slice(&context).unwrap();
        assert_eq!(context["model"]["name"], MODEL);
        let (path, header) = receive(ws).await;
        assert_eq!((path.as_str(), header.len()), ("audio", 44));
        assert_eq!(&header[..4], b"RIFF");
        assert_eq!(
            u32::from_le_bytes(header[24..28].try_into().unwrap()),
            24_000
        );
    }

    #[tokio::test]
    async fn streams_audio_shows_words_and_returns_every_phrase() {
        let url = server(|mut ws| {
            Box::pin(async move {
                setup(&mut ws).await;
                assert_eq!(receive(&mut ws).await, ("audio".into(), vec![0; 9600]));
                for message in [
                    reply("turn.start", json!({})),
                    reply("speech.hypothesis", json!({"Text": "Hello"})),
                    reply(
                        "speech.phrase",
                        json!({"RecognitionStatus": "Success", "DisplayText": "Hello world."}),
                    ),
                    reply("speech.hypothesis", json!({"Text": "How are"})),
                ] {
                    ws.send(message).await.unwrap();
                }
                assert_eq!(receive(&mut ws).await, ("audio".into(), vec![]));
                for message in [
                    reply(
                        "speech.phrase",
                        json!({"RecognitionStatus": "Success", "DisplayText": "How are you?"}),
                    ),
                    reply(
                        "speech.phrase",
                        json!({"RecognitionStatus": "EndOfDictation"}),
                    ),
                    reply("turn.end", json!({})),
                ] {
                    ws.send(message).await.unwrap();
                }
            })
        })
        .await;
        let cancel = CancellationToken::new();
        let stream = Stream::connect_to(&url, "test-key", &cancel).await.unwrap();
        let (audio, input) = mpsc::channel(4);
        let (words, mut shown) = watch::channel(Words::default());
        let running =
            tokio::spawn(async move { stream.run(input, &words, &CancellationToken::new()).await });
        audio.send(Ok(vec![0; 9600])).await.unwrap();
        shown.wait_for(|w| w.interim == " How are").await.unwrap();
        assert_eq!(shown.borrow().text, "Hello world.");
        drop(audio);
        let (text, seconds) = running.await.unwrap().unwrap();
        assert_eq!(text, "Hello world. How are you?");
        assert_eq!(seconds, 0.2);
    }

    #[tokio::test]
    async fn short_audio_is_silence_and_rejections_are_reported() {
        let url = server(|mut ws| {
            Box::pin(async move {
                setup(&mut ws).await;
                let _ = ws.next().await;
            })
        })
        .await;
        let cancel = CancellationToken::new();
        let stream = Stream::connect_to(&url, "test-key", &cancel).await.unwrap();
        let (audio, input) = mpsc::channel(1);
        audio.send(Ok(vec![0; 100])).await.unwrap();
        drop(audio);
        let error = stream
            .run(input, &watch::channel(Words::default()).0, &cancel)
            .await
            .unwrap_err();
        assert!(error.is::<NoSpeech>());

        let url = server(|mut ws| {
            Box::pin(async move {
                setup(&mut ws).await;
                ws.close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame {
                    code:
                        tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Invalid,
                    reason: " Unsupported model. ".into(),
                }))
                .await
                .unwrap();
            })
        })
        .await;
        let stream = Stream::connect_to(&url, "test-key", &cancel).await.unwrap();
        let (_audio, input) = mpsc::channel(1);
        let error = stream
            .run(input, &watch::channel(Words::default()).0, &cancel)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "Azure: Unsupported model.");
    }

    #[tokio::test]
    async fn a_dropped_connection_keeps_the_confirmed_words() {
        let url = server(|mut ws| {
            Box::pin(async move {
                setup(&mut ws).await;
                ws.send(reply(
                    "speech.phrase",
                    json!({"RecognitionStatus": "Success", "DisplayText": "Kept words."}),
                ))
                .await
                .unwrap();
            })
        })
        .await;
        let cancel = CancellationToken::new();
        let stream = Stream::connect_to(&url, "test-key", &cancel).await.unwrap();
        let (_audio, input) = mpsc::channel(1);
        let (words, _) = watch::channel(Words::default());
        assert!(stream.run(input, &words, &cancel).await.is_err());
        assert_eq!(words.borrow().text, "Kept words.");
    }

    #[tokio::test]
    async fn cancel_interrupts_a_stalled_connection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let token = CancellationToken::new();
        let cancel = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancel.cancel();
        });
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            Stream::connect_to(&url, "secret", &token),
        )
        .await
        .unwrap();
        assert_eq!(result.err().unwrap().to_string(), "Cancelled");
    }
}
