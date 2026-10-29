use std::{sync::Arc, time::Duration};

use reqwest::multipart::{Form, Part};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::shared_recordings::{
    OccurrenceManifest, RecordingError, SharedRecordingService, Transcript,
};

const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const PROVIDER: &str = "openai";

fn endpoint(base: &str, suffix: &str, model: &str) -> anyhow::Result<url::Url> {
    anyhow::ensure!(
        !model.is_empty()
            && model.len() <= 256
            && model
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"/_-.:".contains(&byte)),
        "a supported model identifier is required"
    );
    let mut url: url::Url = base.parse()?;
    anyhow::ensure!(
        url.scheme() == "https"
            || (url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))),
        "provider requires HTTPS or loopback HTTP"
    );
    anyhow::ensure!(
        url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "provider endpoint must not contain credentials, query or fragment"
    );
    let path = url.path().trim_end_matches('/').to_owned();
    url.set_path(&if path.ends_with(suffix) {
        path
    } else {
        format!("{path}{suffix}")
    });
    Ok(url)
}

fn client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .build()?)
}

async fn response_json(mut response: reqwest::Response) -> anyhow::Result<Value> {
    anyhow::ensure!(
        response.status().is_success(),
        "provider returned a non-success status"
    );
    anyhow::ensure!(
        response
            .content_length()
            .is_none_or(|length| length <= MAX_RESPONSE_BYTES as u64),
        "provider response exceeds size limit"
    );
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        anyhow::ensure!(
            chunk.len() <= MAX_RESPONSE_BYTES.saturating_sub(body.len()),
            "provider response exceeds size limit"
        );
        body.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&body)?)
}

pub struct RecordingTranscriber {
    client: reqwest::Client,
    endpoint: url::Url,
    api_key: String,
    model: String,
}

impl RecordingTranscriber {
    pub fn new(api_base: String, api_key: String, model: String) -> anyhow::Result<Self> {
        Ok(Self {
            client: client()?,
            endpoint: endpoint(&api_base, "/audio/transcriptions", &model)?,
            api_key,
            model,
        })
    }

    async fn transcribe(&self, wav: Vec<u8>) -> anyhow::Result<Transcript> {
        let form = Form::new()
            .text("model", self.model.clone())
            .text("response_format", "json")
            .part(
                "file",
                Part::bytes(wav)
                    .file_name("source.wav")
                    .mime_str("audio/wav")?,
            );
        let mut request = self.client.post(self.endpoint.clone()).multipart(form);
        if !self.api_key.is_empty() {
            request = request.bearer_auth(&self.api_key);
        }
        let payload = response_json(request.send().await?).await?;
        let text = payload
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("source transcription requires text"))?;
        Ok(Transcript {
            provider: PROVIDER.into(),
            model: self.model.clone(),
            result: json!({"text":text.trim()}),
        })
    }
}

pub struct RecordingProcessor {
    service: Arc<SharedRecordingService>,
    transcriber: RecordingTranscriber,
    gate: Mutex<()>,
}

impl RecordingProcessor {
    pub fn new(service: Arc<SharedRecordingService>, transcriber: RecordingTranscriber) -> Self {
        Self {
            service,
            transcriber,
            gate: Mutex::new(()),
        }
    }

    pub async fn process(&self, workspace: &str, id: &str) -> anyhow::Result<OccurrenceManifest> {
        let _guard = self.gate.lock().await;
        let manifest = self.service.manifest(workspace, id).await?;
        for capture in manifest.captures.iter().filter(|capture| !capture.deleted) {
            if let Some(transcript) = &capture.transcript {
                if transcript.provider != PROVIDER || transcript.model != self.transcriber.model {
                    return Err(RecordingError::Conflict.into());
                }
                continue;
            }
            let (source, wav) = self
                .service
                .source_audio(workspace, id, &capture.capture_id)
                .await?;
            let transcript = tokio::time::timeout(
                Duration::from_secs(15 * 60),
                self.transcriber.transcribe(wav),
            )
            .await??;
            self.service
                .save_source_transcript(
                    workspace,
                    id,
                    &capture.capture_id,
                    &source.metadata.sha256,
                    transcript,
                )
                .await?;
        }
        Ok(self.service.manifest(workspace, id).await?)
    }
}
