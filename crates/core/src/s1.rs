//! S1-mini by Superwhisper, run locally through llama.cpp's server. The model
//! and the llama.cpp build for this machine's hardware download on first use.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::{io::AsyncWriteExt, sync::watch};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Styling {
    Casual,
    SemiCasual,
    #[default]
    SemiFormal,
    Formal,
}

impl Styling {
    fn name(self) -> &'static str {
        match self {
            Self::Casual => "casual",
            Self::SemiCasual => "semi-casual",
            Self::SemiFormal => "semi-formal",
            Self::Formal => "formal",
        }
    }
}

// The model was trained on exactly this system prompt and control line; the
// empty think block keeps Qwen3's thinking mode off.
const SYSTEM: &str = "You are a text normalizer for speech-to-text transcripts. The input begins with a control line specifying the styling, structure, and context settings; clean the transcript to match those settings and output only the cleaned text.";

fn prompt(styling: Styling, transcript: &str) -> String {
    // Transcripts are data: they must never open or close a chat turn.
    let transcript = transcript.replace("<|", "<");
    format!(
        "<|im_start|>system\n{SYSTEM}<|im_end|>\n<|im_start|>user\n[Styling: {}] [Structure: prose] [Context: general]\n{transcript}<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n",
        styling.name()
    )
}

// Roughly 700 tokens of English; the model is built for passes under 1,000.
const CHUNK: usize = 3000;

fn chunks(text: &str, limit: usize) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut rest = text.trim();
    while rest.len() > limit {
        let mut end = limit;
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let window = &rest[..end];
        let sentence = window
            .char_indices()
            .filter(|&(i, c)| {
                matches!(c, '.' | '!' | '?') && window[i + 1..].starts_with(char::is_whitespace)
            })
            .map(|(i, _)| i + 1)
            .last();
        let cut = sentence
            .or_else(|| window.rfind(char::is_whitespace))
            .filter(|&i| i > 0)
            .unwrap_or(end);
        chunks.push(rest[..cut].trim());
        rest = rest[cut..].trim_start();
    }
    if !rest.is_empty() {
        chunks.push(rest);
    }
    chunks
}

struct Asset {
    url: &'static str,
    sha256: &'static str,
    size: u64,
}

impl Asset {
    fn file(&self) -> &'static str {
        self.url.rsplit('/').next().unwrap_or(self.url)
    }
}

static MODEL: Asset = Asset {
    url: "https://huggingface.co/superwhisper/s1-mini-GGUF/resolve/34add00a48a2e5d24e5a4ee5405a99620a3a240c/s1-mini-q4_k_m.gguf",
    sha256: "3b41ebe2502cbd03e811d5d16b022f5ab551eda58d62597d152f89535003c634",
    size: 484_219_808,
};

struct Runtime {
    name: &'static str,
    assets: &'static [Asset],
    gpu: bool,
}

macro_rules! llama {
    ($file:literal, $sha256:literal, $size:literal) => {
        Asset {
            url: concat!(
                "https://github.com/ggml-org/llama.cpp/releases/download/b11153/",
                $file
            ),
            sha256: $sha256,
            size: $size,
        }
    };
}

#[cfg(all(windows, target_arch = "x86_64"))]
fn runtime() -> Result<&'static Runtime> {
    static CUDA_13: Runtime = Runtime {
        name: "llama-b11153-cuda-13",
        assets: &[
            llama!(
                "llama-b11153-bin-win-cuda-13.4-x64.zip",
                "7bef2d6a1594aedf61f011bb04051ec0f2037d951b87197ba5f3411544c99a10",
                149_758_635
            ),
            llama!(
                "cudart-llama-bin-win-cuda-13.4-x64.zip",
                "738f8c251ac22b70c3ae6f83a10cf222725df0395246a2cf58f32bdb85fbe668",
                423_535_356
            ),
        ],
        gpu: true,
    };
    static CUDA_12: Runtime = Runtime {
        name: "llama-b11153-cuda-12",
        assets: &[
            llama!(
                "llama-b11153-bin-win-cuda-12.4-x64.zip",
                "0d5c737b6c5ba61f971515e1e008a52073e606914b4ac4ac8d6f27389b84935a",
                253_869_597
            ),
            llama!(
                "cudart-llama-bin-win-cuda-12.4-x64.zip",
                "8c79a9b226de4b3cacfd1f83d24f962d0773be79f1e7b75c6af4ded7e32ae1d6",
                391_443_627
            ),
        ],
        gpu: true,
    };
    // Vulkan covers AMD and Intel GPUs, and runs on the CPU when there is none.
    static VULKAN: Runtime = Runtime {
        name: "llama-b11153-vulkan",
        assets: &[llama!(
            "llama-b11153-bin-win-vulkan-x64.zip",
            "68925200a1a7543adb83c5dabb4611f4e205e6af10f1b13b7f61604a105bc892",
            32_126_804
        )],
        gpu: true,
    };
    static DETECTED: std::sync::OnceLock<&'static Runtime> = std::sync::OnceLock::new();
    Ok(DETECTED.get_or_init(|| match cuda() {
        Some((driver, capability)) if driver >= 13000 && capability >= 75 => &CUDA_13,
        Some((driver, capability)) if driver >= 12000 && capability >= 50 => &CUDA_12,
        _ => &VULKAN,
    }))
}

/// The NVIDIA driver's CUDA version and the first GPU's compute capability.
#[cfg(all(windows, target_arch = "x86_64"))]
fn cuda() -> Option<(i32, i32)> {
    use windows_sys::Win32::System::LibraryLoader::{
        GetProcAddress, LoadLibraryExA, LOAD_LIBRARY_SEARCH_SYSTEM32,
    };
    type Init = unsafe extern "system" fn(u32) -> i32;
    type Version = unsafe extern "system" fn(*mut i32) -> i32;
    type Device = unsafe extern "system" fn(*mut i32, i32) -> i32;
    type Attribute = unsafe extern "system" fn(*mut i32, i32, i32) -> i32;
    const MAJOR: i32 = 75;
    const MINOR: i32 = 76;
    // SAFETY: these are the CUDA driver API signatures; nvcuda.dll ships with
    // every NVIDIA driver and is only loaded from System32.
    unsafe {
        let library = LoadLibraryExA(
            c"nvcuda.dll".as_ptr().cast(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        );
        if library.is_null() {
            return None;
        }
        let symbol = |name: &std::ffi::CStr| GetProcAddress(library, name.as_ptr().cast());
        let init: Init = std::mem::transmute(symbol(c"cuInit")?);
        let version: Version = std::mem::transmute(symbol(c"cuDriverGetVersion")?);
        let device: Device = std::mem::transmute(symbol(c"cuDeviceGet")?);
        let attribute: Attribute = std::mem::transmute(symbol(c"cuDeviceGetAttribute")?);
        let (mut driver, mut ordinal, mut major, mut minor) = (0, 0, 0, 0);
        (init(0) == 0
            && version(&mut driver) == 0
            && device(&mut ordinal, 0) == 0
            && attribute(&mut major, MAJOR, ordinal) == 0
            && attribute(&mut minor, MINOR, ordinal) == 0)
            .then_some((driver, major * 10 + minor))
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn runtime() -> Result<&'static Runtime> {
    static METAL: Runtime = Runtime {
        name: "llama-b11153-metal",
        assets: &[llama!(
            "llama-b11153-bin-macos-arm64.tar.gz",
            "9aa63c493ee501b10d2c51a4f6e7923843e5a75ede176a521df3cc55a1b83db0",
            11_189_602
        )],
        gpu: true,
    };
    Ok(&METAL)
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn runtime() -> Result<&'static Runtime> {
    static VULKAN: Runtime = Runtime {
        name: "llama-b11153-vulkan",
        assets: &[llama!(
            "llama-b11153-bin-ubuntu-vulkan-x64.tar.gz",
            "5cbe209c19456e94d300cc41a0cd4eff1d367404736135754b031f29634a76de",
            30_598_114
        )],
        gpu: true,
    };
    Ok(&VULKAN)
}

#[cfg(not(any(
    all(windows, target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64")
)))]
fn runtime() -> Result<&'static Runtime> {
    bail!("S1-mini unavailable on this platform")
}

#[cfg(windows)]
const SERVER: &str = "llama-server.exe";
#[cfg(not(windows))]
const SERVER: &str = "llama-server";

const DOWNLOAD_FAILED: &str = "Download failed";
const START_FAILED: &str = "S1-mini failed to start";
// Longer than a full-length recording and its transcription.
const WARM: Duration = Duration::from_secs(10 * 60);

/// S1-mini needs two downloads: llama.cpp built for this machine, and the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Part {
    Engine,
    Model,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Download {
    pub ready: bool,
    /// Bytes still to download.
    pub size: u64,
    /// From 0 to 1 while downloading.
    pub progress: Option<f64>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Status {
    pub engine: Download,
    pub model: Download,
}

impl Status {
    fn part(&mut self, part: Part) -> &mut Download {
        match part {
            Part::Engine => &mut self.engine,
            Part::Model => &mut self.model,
        }
    }
}

struct Plan {
    runtime: &'static Runtime,
    directory: PathBuf,
    model: PathBuf,
}

impl Plan {
    fn missing(&self, part: Part) -> Vec<&'static Asset> {
        match part {
            Part::Engine if !self.directory.join(SERVER).exists() => {
                self.runtime.assets.iter().collect()
            }
            Part::Model if !self.model.exists() => vec![&MODEL],
            _ => Vec::new(),
        }
    }
}

struct Server {
    url: String,
    key: String,
    child: Mutex<tokio::process::Child>,
}

impl Server {
    fn alive(&self) -> bool {
        matches!(self.child.lock().unwrap().try_wait(), Ok(None))
    }
}

pub struct Engine {
    dir: PathBuf,
    downloads: reqwest::Client,
    local: reqwest::Client,
    status: watch::Sender<Status>,
    downloading: [tokio::sync::Mutex<()>; 2],
    cancel: [Mutex<CancellationToken>; 2],
    starting: tokio::sync::Mutex<()>,
    server: Mutex<Option<Arc<Server>>>,
    cpu_only: AtomicBool,
    uses: AtomicU64,
    unload: Mutex<Option<Duration>>,
}

impl Engine {
    pub fn new(dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            dir,
            downloads: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .read_timeout(Duration::from_secs(60))
                .build()
                .expect("HTTP client"),
            local: reqwest::Client::builder()
                .no_proxy()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(120))
                .build()
                .expect("HTTP client"),
            status: watch::Sender::new(Status::default()),
            downloading: Default::default(),
            cancel: Default::default(),
            starting: tokio::sync::Mutex::new(()),
            server: Mutex::new(None),
            cpu_only: AtomicBool::new(false),
            uses: AtomicU64::new(0),
            unload: Mutex::new(Some(Duration::from_secs(5 * 60))),
        })
    }

    pub fn subscribe(&self) -> watch::Receiver<Status> {
        self.status.subscribe()
    }

    /// What is downloaded, read from disk except while a part downloads.
    pub fn status(&self) -> Status {
        let plan = self.plan();
        self.status.send_if_modified(|status| {
            let before = status.clone();
            for part in [Part::Engine, Part::Model] {
                let download = status.part(part);
                if download.progress.is_some() {
                    continue;
                }
                match &plan {
                    Ok(plan) => {
                        let missing = plan.missing(part);
                        download.ready = missing.is_empty();
                        download.size = missing.iter().map(|asset| asset.size).sum();
                    }
                    Err(error) => {
                        download.ready = false;
                        download.error = Some(error.to_string());
                    }
                }
            }
            *status != before
        });
        self.status.borrow().clone()
    }

    /// Fails unless both parts are downloaded.
    pub fn check(&self) -> Result<()> {
        let plan = self.plan()?;
        anyhow::ensure!(
            plan.missing(Part::Engine).is_empty(),
            "llama.cpp not downloaded"
        );
        anyhow::ensure!(
            plan.missing(Part::Model).is_empty(),
            "S1-mini not downloaded"
        );
        Ok(())
    }

    /// Downloads one part; a second call while it downloads returns at once.
    pub async fn download(&self, part: Part) -> Result<()> {
        let cancel = self.cancel[part as usize].lock().unwrap().clone();
        let Ok(_downloading) = self.downloading[part as usize].try_lock() else {
            return Ok(());
        };
        let result = self.download_part(part, &cancel).await;
        self.status.send_modify(|status| {
            let download = status.part(part);
            download.progress = None;
            download.error = match &result {
                Err(error) if !cancel.is_cancelled() => Some(error.to_string()),
                _ => None,
            };
        });
        self.status();
        result
    }

    /// A cancelled download resumes where it stopped.
    pub fn cancel_download(&self, part: Part) {
        std::mem::take(&mut *self.cancel[part as usize].lock().unwrap()).cancel();
    }

    /// Shuts the server down.
    pub fn stop(&self) {
        self.server.lock().unwrap().take();
    }

    /// How long an unused model stays loaded; `None` keeps it loaded.
    pub fn set_unload(self: &Arc<Self>, unload: Option<Duration>) {
        *self.unload.lock().unwrap() = unload;
        if self.running().is_some() {
            self.touch(false);
        }
    }

    /// Starts the server ahead of use.
    pub async fn warm(self: &Arc<Self>) -> Result<()> {
        self.server().await?;
        self.touch(true);
        Ok(())
    }

    pub async fn clean(self: &Arc<Self>, transcript: &str, styling: Styling) -> Result<String> {
        let server = self.server().await?;
        let mut cleaned = Vec::new();
        let mut result = Ok(());
        for chunk in chunks(transcript, CHUNK) {
            let text = complete(
                &self.local,
                &server.url,
                &server.key,
                prompt(styling, chunk),
                chunk.len() / 2 + 32,
            )
            .await;
            match text {
                Ok(text) if !text.is_empty() => cleaned.push(text),
                Ok(_) => {}
                Err(error) => {
                    result = Err(error);
                    break;
                }
            }
        }
        self.touch(false);
        result.map(|()| cleaned.join(" "))
    }

    // Unloads the model once it has gone unused for the configured time. A
    // model loaded for a recording outlasts that recording and its
    // transcription, so it is never unloaded before its cleanup.
    fn touch(self: &Arc<Self>, warming: bool) {
        let use_ = self.uses.fetch_add(1, Ordering::SeqCst) + 1;
        let Some(mut after) = *self.unload.lock().unwrap() else {
            return;
        };
        if warming {
            after = after.max(WARM);
        }
        let engine = Arc::downgrade(self);
        tokio::spawn(async move {
            tokio::time::sleep(after).await;
            if let Some(engine) = engine.upgrade() {
                if engine.uses.load(Ordering::SeqCst) == use_ {
                    engine.server.lock().unwrap().take();
                }
            }
        });
    }

    fn plan(&self) -> Result<Plan> {
        let runtime = runtime()?;
        Ok(Plan {
            runtime,
            directory: self.dir.join(runtime.name),
            model: self.dir.join(MODEL.file()),
        })
    }

    fn running(&self) -> Option<Arc<Server>> {
        let mut server = self.server.lock().unwrap();
        if !server.as_ref().is_some_and(|s| s.alive()) {
            server.take();
        }
        server.clone()
    }

    async fn server(self: &Arc<Self>) -> Result<Arc<Server>> {
        if let Some(server) = self.running() {
            return Ok(server);
        }
        let _starting = self.starting.lock().await;
        if let Some(server) = self.running() {
            return Ok(server);
        }
        self.check()?;
        let plan = self.plan()?;
        let gpu = plan.runtime.gpu && !self.cpu_only.load(Ordering::SeqCst);
        let server = match self.spawn(&plan.directory, &plan.model, gpu).await {
            // A GPU backend that cannot start falls back to the CPU.
            Err(_) if gpu => {
                self.cpu_only.store(true, Ordering::SeqCst);
                self.spawn(&plan.directory, &plan.model, false).await?
            }
            server => server?,
        };
        let server = Arc::new(server);
        *self.server.lock().unwrap() = Some(server.clone());
        // Never left loaded if the call that started it is abandoned.
        self.touch(true);
        Ok(server)
    }

    async fn spawn(&self, directory: &Path, model: &Path, gpu: bool) -> Result<Server> {
        let port = std::net::TcpListener::bind("127.0.0.1:0")?
            .local_addr()?
            .port();
        let key = uuid::Uuid::new_v4().to_string();
        let log = std::fs::File::create(self.dir.join("llama-server.log"))?;
        let mut command = tokio::process::Command::new(directory.join(SERVER));
        command
            .current_dir(directory)
            .args(["--host", "127.0.0.1", "--port", &port.to_string()])
            .args(["-c", "4096", "-np", "1", "--no-webui", "-m"])
            .arg(model)
            .args(if gpu {
                ["-ngl", "99", "-sm", "none"].as_slice()
            } else {
                ["--device", "none"].as_slice()
            })
            .env("LLAMA_API_KEY", &key)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        let mut child = command.spawn().context(START_FAILED)?;
        #[cfg(windows)]
        contain(&child);
        let url = format!("http://127.0.0.1:{port}");
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            if !matches!(child.try_wait(), Ok(None)) || Instant::now() > deadline {
                bail!(START_FAILED);
            }
            let health = self.local.get(format!("{url}/health")).send().await;
            if health.is_ok_and(|response| response.status().is_success()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok(Server {
            url,
            key,
            child: Mutex::new(child),
        })
    }

    async fn download_part(&self, part: Part, cancel: &CancellationToken) -> Result<()> {
        let plan = self.plan()?;
        let missing = plan.missing(part);
        let total: u64 = missing.iter().map(|asset| asset.size).sum();
        if total == 0 {
            return Ok(());
        }
        self.status.send_modify(|status| {
            let download = status.part(part);
            download.progress = Some(0.);
            download.error = None;
        });
        tokio::fs::create_dir_all(&self.dir).await?;
        let mut done = 0;
        for asset in &missing {
            let destination = self.dir.join(asset.file());
            self.fetch(asset, &destination, cancel, &mut |bytes| {
                done += bytes;
                let progress = (done * 1000 / total) as f64 / 1000.;
                self.status.send_if_modified(|status| {
                    let download = status.part(part);
                    let changed = download.progress != Some(progress);
                    download.progress = Some(progress);
                    changed
                });
            })
            .await?;
        }
        if part == Part::Engine {
            let dir = self.dir.clone();
            let runtime = plan.runtime;
            let archives: Vec<_> = runtime.assets.iter().map(|a| dir.join(a.file())).collect();
            tokio::task::spawn_blocking(move || -> Result<()> {
                let staging = dir.join(format!("{}.part", runtime.name));
                install_runtime(&archives, &staging, &dir.join(runtime.name))?;
                for archive in archives {
                    std::fs::remove_file(archive)?;
                }
                // Only one llama.cpp build is kept.
                for entry in std::fs::read_dir(&dir)? {
                    let entry = entry?;
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if name.starts_with("llama-")
                        && name != runtime.name
                        && entry.file_type()?.is_dir()
                    {
                        std::fs::remove_dir_all(entry.path())?;
                    }
                }
                Ok(())
            })
            .await??;
        }
        Ok(())
    }

    async fn fetch(
        &self,
        asset: &Asset,
        destination: &Path,
        cancel: &CancellationToken,
        progress: &mut (dyn FnMut(u64) + Send),
    ) -> Result<()> {
        let part = destination.with_file_name(format!("{}.part", asset.file()));
        // An interrupted download resumes after re-hashing what it already has.
        let existing = part.clone();
        let (mut hasher, mut offset) = tokio::task::spawn_blocking(move || {
            let mut hasher = Sha256::new();
            let offset = match std::fs::File::open(&existing) {
                Ok(mut file) => std::io::copy(&mut file, &mut hasher)?,
                Err(_) => 0,
            };
            std::io::Result::Ok((hasher, offset))
        })
        .await??;
        if offset > asset.size {
            (hasher, offset) = (Sha256::new(), 0);
        }
        if offset < asset.size {
            let mut request = self.downloads.get(asset.url);
            if offset > 0 {
                request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
            }
            let mut response = request.send().await.context(DOWNLOAD_FAILED)?;
            let mut file = match response.status().as_u16() {
                206 if offset > 0 => {
                    tokio::fs::OpenOptions::new()
                        .append(true)
                        .open(&part)
                        .await?
                }
                200 => {
                    (hasher, offset) = (Sha256::new(), 0);
                    tokio::fs::File::create(&part).await?
                }
                _ => bail!(DOWNLOAD_FAILED),
            };
            progress(offset);
            loop {
                let chunk = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => bail!("Cancelled"),
                    chunk = response.chunk() => chunk.context(DOWNLOAD_FAILED)?,
                };
                let Some(chunk) = chunk else { break };
                offset += chunk.len() as u64;
                anyhow::ensure!(offset <= asset.size, DOWNLOAD_FAILED);
                file.write_all(&chunk).await?;
                hasher.update(&chunk);
                progress(chunk.len() as u64);
            }
            file.flush().await?;
        } else {
            progress(offset);
        }
        if offset != asset.size || format!("{:x}", hasher.finalize()) != asset.sha256 {
            let _ = tokio::fs::remove_file(&part).await;
            bail!(DOWNLOAD_FAILED);
        }
        tokio::fs::rename(&part, destination).await?;
        Ok(())
    }
}

fn install_runtime(archives: &[PathBuf], staging: &Path, target: &Path) -> Result<()> {
    let _ = std::fs::remove_dir_all(staging);
    for (index, archive) in archives.iter().enumerate() {
        let unpacked = staging.join(format!(".{index}"));
        unpack(archive, &unpacked).context(DOWNLOAD_FAILED)?;
        // Archives hold their files either directly or inside one folder.
        let entries = std::fs::read_dir(&unpacked)?.collect::<std::io::Result<Vec<_>>>()?;
        let root = match entries.as_slice() {
            [entry] if entry.file_type()?.is_dir() => entry.path(),
            _ => unpacked.clone(),
        };
        for entry in std::fs::read_dir(&root)? {
            let entry = entry?;
            std::fs::rename(entry.path(), staging.join(entry.file_name()))?;
        }
        std::fs::remove_dir_all(&unpacked)?;
    }
    anyhow::ensure!(staging.join(SERVER).exists(), DOWNLOAD_FAILED);
    let _ = std::fs::remove_dir_all(target);
    std::fs::rename(staging, target)?;
    Ok(())
}

#[cfg(windows)]
fn unpack(archive: &Path, into: &Path) -> Result<()> {
    zip::ZipArchive::new(std::fs::File::open(archive)?)?.extract(into)?;
    Ok(())
}

#[cfg(not(windows))]
fn unpack(archive: &Path, into: &Path) -> Result<()> {
    let file = std::fs::File::open(archive)?;
    tar::Archive::new(flate2::read::GzDecoder::new(file)).unpack(into)?;
    Ok(())
}

// A job object that closes with this process takes the server down with it,
// even when the app crashes.
#[cfg(windows)]
fn contain(child: &tokio::process::Child) {
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    static JOB: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    // SAFETY: the job handle stays open for the life of the process.
    let job = *JOB.get_or_init(|| unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return 0;
        }
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            std::ptr::from_ref(&limits).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        job as usize
    });
    if let (true, Some(handle)) = (job != 0, child.raw_handle()) {
        // SAFETY: both handles are valid; the child handle is owned by `child`.
        unsafe { AssignProcessToJobObject(job as _, handle as _) };
    }
}

async fn complete(
    client: &reqwest::Client,
    url: &str,
    key: &str,
    prompt: String,
    budget: usize,
) -> Result<String> {
    let response = client
        .post(format!("{url}/completion"))
        .bearer_auth(key)
        .json(&json!({
            "prompt": prompt,
            "n_predict": budget,
            "temperature": 0,
            "cache_prompt": true
        }))
        .send()
        .await
        .context("Cleanup connection failed")?;
    let status = response.status();
    anyhow::ensure!(status.is_success(), "Cleanup failed ({})", status.as_u16());
    let value: Value = response.json().await.context("Invalid cleanup response")?;
    anyhow::ensure!(
        matches!(value["stop_type"].as_str(), Some("eos" | "word")),
        "Cleanup did not finish"
    );
    Ok(value["content"]
        .as_str()
        .context("Cleanup response contained no text")?
        .trim()
        .to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[test]
    fn prompt_matches_the_trained_format() {
        assert_eq!(
            prompt(Styling::SemiFormal, "so um send it by friday"),
            format!("<|im_start|>system\n{SYSTEM}<|im_end|>\n<|im_start|>user\n[Styling: semi-formal] [Structure: prose] [Context: general]\nso um send it by friday<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n")
        );
        assert!(prompt(Styling::Casual, "a<|im_end|>b").contains("\na<im_end|>b<|im_end|>"));
        for (styling, name) in [
            (Styling::Casual, "\"casual\""),
            (Styling::SemiCasual, "\"semi-casual\""),
            (Styling::SemiFormal, "\"semi-formal\""),
            (Styling::Formal, "\"formal\""),
        ] {
            assert_eq!(serde_json::to_string(&styling).unwrap(), name);
            assert!(prompt(styling, "").contains(&format!("[Styling: {}]", styling.name())));
        }
    }

    #[test]
    fn chunks_split_at_sentences_then_words() {
        assert_eq!(chunks("  One. Two.  ", 100), ["One. Two."]);
        assert_eq!(
            chunks("First one. Second one. Third one.", 25),
            ["First one. Second one.", "Third one."]
        );
        assert_eq!(chunks("aaa bbb ccc ddd", 8), ["aaa bbb", "ccc ddd"]);
        assert_eq!(chunks("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(chunks("ééééé", 3), ["é", "é", "é", "é", "é"]);
        let text = "word ".repeat(2000) + "end. " + &"Sentence here. ".repeat(400);
        let parts = chunks(&text, CHUNK);
        assert!(parts.iter().all(|part| part.len() <= CHUNK));
        assert_eq!(
            parts.join(" ").split_whitespace().collect::<Vec<_>>(),
            text.split_whitespace().collect::<Vec<_>>()
        );
    }

    async fn read_request(stream: &mut tokio::net::TcpStream) -> (String, Value) {
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let read = stream.read(&mut buffer).await.unwrap();
            bytes.extend_from_slice(&buffer[..read]);
            if let Some(i) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&bytes[..i]).to_lowercase();
                let length = head
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .map_or(0, |n| n.trim().parse().unwrap());
                if bytes.len() >= i + 4 + length {
                    let body = serde_json::from_slice(&bytes[i + 4..i + 4 + length])
                        .unwrap_or(Value::Null);
                    return (head, body);
                }
            }
            assert_ne!(read, 0, "Request ended early");
        }
    }

    async fn respond(stream: &mut tokio::net::TcpStream, status: &str, body: &[u8]) {
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).await.unwrap();
        stream.write_all(body).await.unwrap();
    }

    async fn completion(body: &'static str) -> (Result<String>, String, Value) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_request(&mut stream).await;
            respond(&mut stream, "200 OK", body.as_bytes()).await;
            request
        });
        let result = complete(
            &reqwest::Client::new(),
            &url,
            "test-only",
            "prompt".into(),
            40,
        )
        .await;
        let (head, body) = server.await.unwrap();
        (result, head, body)
    }

    #[tokio::test]
    async fn completion_is_greedy_and_returns_trimmed_text() {
        let (result, head, body) =
            completion(r#"{"content":"  Hello.  ","stop_type":"eos"}"#).await;
        assert_eq!(result.unwrap(), "Hello.");
        assert!(head.starts_with("post /completion "));
        assert!(head.contains("authorization: bearer test-only"));
        assert_eq!(
            body,
            json!({"prompt":"prompt","n_predict":40,"temperature":0,"cache_prompt":true})
        );
        let (result, _, _) = completion(r#"{"content":"","stop_type":"eos"}"#).await;
        assert_eq!(result.unwrap(), "");
    }

    #[tokio::test]
    async fn truncated_completion_is_never_returned() {
        for body in [
            r#"{"content":"Do","stop_type":"limit"}"#,
            r#"{"content":"Do","stop_type":"none"}"#,
            r#"{"content":"Do"}"#,
        ] {
            let (result, _, _) = completion(body).await;
            assert_eq!(result.unwrap_err().to_string(), "Cleanup did not finish");
        }
    }

    // Serves `content` once, honouring a Range request, and reports the
    // requested range.
    async fn serve_file(content: &'static [u8]) -> (String, tokio::task::JoinHandle<Option<u64>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/file.bin", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (head, _) = read_request(&mut stream).await;
            let start = head
                .lines()
                .find_map(|line| line.strip_prefix("range: bytes="))
                .map(|range| range.trim_end_matches('-').parse::<u64>().unwrap());
            match start {
                Some(start) => {
                    respond(
                        &mut stream,
                        "206 Partial Content",
                        &content[start as usize..],
                    )
                    .await
                }
                None => respond(&mut stream, "200 OK", content).await,
            }
            start
        });
        (url, server)
    }

    fn asset(url: String, content: &[u8]) -> Asset {
        Asset {
            url: Box::leak(url.into_boxed_str()),
            sha256: Box::leak(format!("{:x}", Sha256::digest(content)).into_boxed_str()),
            size: content.len() as u64,
        }
    }

    #[tokio::test]
    async fn download_resumes_and_verifies_the_checksum() {
        const CONTENT: &[u8] = b"0123456789abcdef";
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::new(dir.path().into());
        let (url, server) = serve_file(CONTENT).await;
        let asset = asset(url, CONTENT);
        std::fs::write(dir.path().join("file.bin.part"), &CONTENT[..6]).unwrap();
        let destination = dir.path().join("file.bin");
        let mut done = 0;
        engine
            .fetch(
                &asset,
                &destination,
                &CancellationToken::new(),
                &mut |bytes| done += bytes,
            )
            .await
            .unwrap();
        assert_eq!(server.await.unwrap(), Some(6));
        assert_eq!(std::fs::read(&destination).unwrap(), CONTENT);
        assert_eq!(done, 16);
        assert!(!dir.path().join("file.bin.part").exists());
    }

    #[tokio::test]
    async fn download_with_a_wrong_checksum_is_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::new(dir.path().into());
        let (url, server) = serve_file(b"tampered content").await;
        let asset = asset(url, b"expected content");
        let destination = dir.path().join("file.bin");
        let error = engine
            .fetch(&asset, &destination, &CancellationToken::new(), &mut |_| {})
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), DOWNLOAD_FAILED);
        assert_eq!(server.await.unwrap(), None);
        assert!(!destination.exists());
        assert!(!dir.path().join("file.bin.part").exists());
    }

    #[tokio::test]
    async fn nothing_downloads_or_starts_without_an_explicit_download() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::new(dir.path().into());
        let status = engine.status();
        assert!(!status.engine.ready && !status.model.ready);
        assert_eq!(status.engine.progress, None);
        assert_eq!(
            status.engine.size,
            runtime()
                .unwrap()
                .assets
                .iter()
                .map(|a| a.size)
                .sum::<u64>()
        );
        assert_eq!(status.model.size, MODEL.size);
        let error = engine.clean("um hello", Styling::SemiFormal).await;
        assert_eq!(error.unwrap_err().to_string(), "llama.cpp not downloaded");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn unused_model_unloads_after_the_configured_time() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::new(dir.path().into());
        let mut command = if cfg!(windows) {
            let mut ping = tokio::process::Command::new("ping");
            ping.args(["-n", "30", "127.0.0.1"]);
            ping
        } else {
            let mut sleep = tokio::process::Command::new("sleep");
            sleep.arg("30");
            sleep
        };
        let child = command
            .stdout(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        *engine.server.lock().unwrap() = Some(Arc::new(Server {
            url: String::new(),
            key: String::new(),
            child: Mutex::new(child),
        }));
        let wait = || tokio::time::sleep(Duration::from_millis(150));
        engine.set_unload(None);
        wait().await;
        assert!(engine.running().is_some());
        engine.set_unload(Some(Duration::from_millis(20)));
        // Loaded for a recording: kept for the recording's full length.
        engine.touch(true);
        wait().await;
        assert!(engine.running().is_some());
        // Its cleanup finished: the configured time applies.
        engine.touch(false);
        wait().await;
        assert!(engine.running().is_none());
    }

    #[test]
    fn runtime_installs_from_flat_and_nested_archives() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        std::fs::create_dir_all(source.join("flat")).unwrap();
        std::fs::create_dir_all(source.join("nested/llama-b1")).unwrap();
        std::fs::write(source.join("nested/llama-b1").join(SERVER), "server").unwrap();
        std::fs::write(source.join("flat/cudart.dll"), "runtime").unwrap();
        let archives = [
            archive(&source.join("nested"), &dir.path().join("a")),
            archive(&source.join("flat"), &dir.path().join("b")),
        ];
        let target = dir.path().join("llama");
        install_runtime(&archives, &dir.path().join("llama.part"), &target).unwrap();
        assert_eq!(std::fs::read(target.join(SERVER)).unwrap(), b"server");
        assert_eq!(
            std::fs::read(target.join("cudart.dll")).unwrap(),
            b"runtime"
        );
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 2);
        assert!(!dir.path().join("llama.part").exists());
    }

    #[cfg(windows)]
    fn archive(source: &Path, path: &Path) -> PathBuf {
        let path = path.with_extension("zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        for entry in walk(source) {
            let name = entry
                .strip_prefix(source)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            zip.start_file(name, options).unwrap();
            std::io::Write::write_all(&mut zip, &std::fs::read(&entry).unwrap()).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    #[cfg(not(windows))]
    fn archive(source: &Path, path: &Path) -> PathBuf {
        let path = path.with_extension("tar.gz");
        let file = std::fs::File::create(&path).unwrap();
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
            file,
            flate2::Compression::fast(),
        ));
        tar.append_dir_all(".", source).unwrap();
        tar.into_inner().unwrap().finish().unwrap();
        path
    }

    #[cfg(windows)]
    fn walk(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .flat_map(|entry| {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path)
                } else {
                    vec![path]
                }
            })
            .collect()
    }

    // cargo test -p transcribe-core real_model -- --ignored --nocapture
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "Downloads the model and a llama.cpp build"]
    async fn real_model_cleans_the_model_card_examples() {
        let dir = std::env::temp_dir().join("transcribe-s1-mini");
        let engine = Engine::new(dir);
        let started = Instant::now();
        let (engine_download, model_download) =
            tokio::join!(engine.download(Part::Engine), engine.download(Part::Model));
        engine_download.unwrap();
        model_download.unwrap();
        let status = engine.status();
        assert!(status.engine.ready && status.model.ready);
        engine.warm().await.unwrap();
        println!(
            "{} ready in {:?}",
            runtime().unwrap().name,
            started.elapsed()
        );
        for (input, expected) in [
            (
                "so um i need to like send the the report by uh friday no wait make that thursday",
                "I need to send the report by Thursday.",
            ),
            (
                "i think the answer is forty two no sorry forty three",
                "I think the answer is 43.",
            ),
            (
                "send it to support at superwhisper dot com",
                "Send it to support@superwhisper.com.",
            ),
            ("um", ""),
        ] {
            let started = Instant::now();
            let output = engine.clean(input, Styling::SemiFormal).await.unwrap();
            println!("{:?} {output:?}", started.elapsed());
            assert_eq!(
                output.trim_start_matches("So "),
                expected.trim_start_matches("So ")
            );
        }
        let casual = engine
            .clean(
                "hmm im gonna be late theres a cute dog outside",
                Styling::Casual,
            )
            .await
            .unwrap();
        assert_eq!(casual, casual.to_lowercase());
        engine.stop();
    }
}
