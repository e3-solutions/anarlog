use std::{sync::Arc, time::Duration};

use reqwest::multipart::{Form, Part};
use tokio::sync::Mutex;

use crate::shared_recordings::{RecordingError, Revision, SharedRecordingService, Transcript};

const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const PROVIDER: &str = "openai";

pub struct RecordingTranscriber {
    client: reqwest::Client,
    endpoint: url::Url,
    api_key: String,
    model: String,
}

impl RecordingTranscriber {
    pub fn new(api_base: String, api_key: String, model: String) -> anyhow::Result<Self> {
        let mut url: url::Url = api_base.parse()?;
        anyhow::ensure!(
            !model.is_empty()
                && model.len() <= 128
                && model
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_-.:".contains(&byte)),
            "a supported model identifier is required"
        );
        anyhow::ensure!(
            url.scheme() == "https"
                || (url.scheme() == "http"
                    && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))),
            "transcription requires HTTPS or a loopback HTTP endpoint"
        );
        anyhow::ensure!(
            url.username().is_empty() && url.password().is_none(),
            "endpoint must not contain credentials"
        );
        let path = url.path().trim_end_matches('/').to_string();
        url.set_path(&if path.ends_with("/audio/transcriptions") {
            path
        } else {
            format!("{path}/audio/transcriptions")
        });
        Ok(Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            endpoint: url,
            api_key,
            model,
        })
    }

    async fn transcribe(&self, wav: Vec<u8>) -> anyhow::Result<serde_json::Value> {
        let form = Form::new()
            .text("model", self.model.clone())
            .text("response_format", "json")
            .part(
                "file",
                Part::bytes(wav)
                    .file_name("canonical.wav")
                    .mime_str("audio/wav")?,
            );
        let mut request = self.client.post(self.endpoint.clone()).multipart(form);
        if !self.api_key.is_empty() {
            request = request.bearer_auth(&self.api_key);
        }
        let mut response = request.send().await?;
        anyhow::ensure!(
            response.status().is_success(),
            "transcription endpoint returned a non-success status"
        );
        anyhow::ensure!(
            response
                .content_length()
                .is_none_or(|length| length <= MAX_RESPONSE_BYTES as u64),
            "transcription response exceeds the size limit"
        );
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            anyhow::ensure!(
                chunk.len() <= MAX_RESPONSE_BYTES.saturating_sub(body.len()),
                "transcription response exceeds the size limit"
            );
            body.extend_from_slice(&chunk);
        }
        let payload: serde_json::Value = serde_json::from_slice(&body)?;
        let text = payload
            .get("text")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("transcription response requires text"))?;
        Ok(serde_json::json!({
            "metadata": {"timing_source": "synthetic_text"},
            "results": {"channels": [{"alternatives": [{
                "transcript": text.trim(), "confidence": 1.0, "words": []
            }]}]}
        }))
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

    pub async fn process(&self, workspace: &str, id: &str) -> anyhow::Result<Revision> {
        // Keep concurrent requests from repeating a completed transcription.
        let _guard = self.gate.lock().await;
        self.service.finalize(workspace, id).await?;
        let (revision, wav) = self.service.canonical_audio(workspace, id).await?;
        if let Some(transcript) = &revision.transcript {
            if transcript.provider != PROVIDER || transcript.model != self.transcriber.model {
                return Err(RecordingError::Conflict.into());
            }
            return Ok(revision);
        }
        let response = tokio::time::timeout(
            Duration::from_secs(15 * 60),
            self.transcriber.transcribe(wav),
        )
        .await??;
        Ok(self
            .service
            .save_transcript(
                workspace,
                id,
                &revision.revision,
                &revision.audio_sha256,
                Transcript {
                    provider: PROVIDER.into(),
                    model: self.transcriber.model.clone(),
                    result: response,
                },
            )
            .await?)
    }
}
