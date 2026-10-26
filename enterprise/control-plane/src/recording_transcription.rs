use std::{io::Write, sync::Arc, time::Duration};

use owhisper_client::{BatchClient, DeepgramAdapter, OpenAIAdapter};
use owhisper_interface::{ListenParams, batch};
use tokio::sync::Mutex;

use crate::shared_recordings::{RecordingError, Revision, SharedRecordingService, Transcript};

pub enum RecordingTranscriber {
    Deepgram(BatchClient<DeepgramAdapter>),
    OpenAi(BatchClient<OpenAIAdapter>),
}

impl RecordingTranscriber {
    pub fn new(
        provider: &str,
        api_base: String,
        api_key: String,
        model: String,
    ) -> anyhow::Result<Self> {
        let url: url::Url = api_base.parse()?;
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
        let params = ListenParams {
            model: Some(model),
            ..Default::default()
        };
        match provider {
            "deepgram" => Ok(Self::Deepgram(BatchClient::<DeepgramAdapter>::new(
                api_base, api_key, params,
            ))),
            "openai" => Ok(Self::OpenAi(BatchClient::<OpenAIAdapter>::new(
                api_base, api_key, params,
            ))),
            _ => anyhow::bail!("supported transcription adapters are deepgram and openai"),
        }
    }

    async fn transcribe(&self, wav: &[u8]) -> anyhow::Result<batch::Response> {
        let mut file = tempfile::Builder::new().suffix(".wav").tempfile()?;
        file.write_all(wav)?;
        file.flush()?;
        let response = match self {
            Self::Deepgram(client) => client.transcribe_file(file.path()).await?,
            Self::OpenAi(client) => client.transcribe_file(file.path()).await?,
        };
        Ok(response)
    }
}

pub struct RecordingProcessor {
    service: Arc<SharedRecordingService>,
    transcriber: RecordingTranscriber,
    provider: String,
    model: String,
    gate: Mutex<()>,
}

impl RecordingProcessor {
    pub fn new(
        service: Arc<SharedRecordingService>,
        transcriber: RecordingTranscriber,
        provider: String,
        model: String,
    ) -> Self {
        Self {
            service,
            transcriber,
            provider,
            model,
            gate: Mutex::new(()),
        }
    }

    pub async fn process(&self, workspace: &str, id: &str) -> anyhow::Result<Revision> {
        // Keep duplicate requests from buying the same successful transcription twice.
        let _guard = self.gate.lock().await;
        self.service.finalize(workspace, id).await?;
        let (revision, wav) = self.service.canonical_audio(workspace, id).await?;
        if let Some(transcript) = &revision.transcript {
            if transcript.provider != self.provider || transcript.model != self.model {
                return Err(RecordingError::Conflict.into());
            }
            return Ok(revision);
        }
        let response = tokio::time::timeout(
            Duration::from_secs(15 * 60),
            self.transcriber.transcribe(&wav),
        )
        .await??;
        validate_response(&response, revision.manifest.duration_frames)?;
        Ok(self
            .service
            .save_transcript(
                workspace,
                id,
                &revision.revision,
                &revision.audio_sha256,
                Transcript {
                    provider: self.provider.clone(),
                    model: self.model.clone(),
                    result: serde_json::to_value(response)?,
                },
            )
            .await?)
    }
}

fn validate_response(response: &batch::Response, duration_frames: u64) -> anyhow::Result<()> {
    anyhow::ensure!(
        response.results.channels.len() == 1,
        "expected one mono transcript channel"
    );
    let alternatives = &response.results.channels[0].alternatives;
    anyhow::ensure!(
        !alternatives.is_empty(),
        "transcription returned no alternatives"
    );
    let duration = duration_frames as f64 / anlg_shared_recording::SAMPLE_RATE as f64;
    for alternative in alternatives {
        let mut previous = 0.0;
        for word in &alternative.words {
            anyhow::ensure!(
                word.start.is_finite()
                    && word.end.is_finite()
                    && word.start >= previous
                    && word.end >= word.start
                    && word.end <= duration + 0.05,
                "transcription timestamps do not match canonical timeline"
            );
            previous = word.start;
        }
    }
    Ok(())
}
