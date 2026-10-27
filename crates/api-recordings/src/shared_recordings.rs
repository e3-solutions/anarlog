use reqwest::{Client, Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::Duration;

const MAX_AUDIO_BYTES: usize = 100 * 1024 * 1024;

const MAX_FRAMES: u64 = 16_000 * 60 * 60 * 4;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FrameRange {
    pub start: u64,
    pub end: u64,
}

fn validate_wav(wav: &[u8], gaps: &[FrameRange]) -> Result<u64> {
    let invalid = || {
        RecordingError::Invalid(
            "expected nonempty 16 kHz mono PCM16 WAV and valid source gaps".into(),
        )
    };
    if wav.len() as u64 > MAX_FRAMES * 2 + 4096 || gaps.len() > 4096 {
        return Err(invalid());
    }
    let mut reader = hound::WavReader::new(std::io::Cursor::new(wav)).map_err(|_| invalid())?;
    let spec = reader.spec();
    let frames = u64::from(reader.len());
    if spec.channels != 1
        || spec.sample_rate != 16_000
        || spec.bits_per_sample != 16
        || spec.sample_format != hound::SampleFormat::Int
        || frames == 0
        || frames > MAX_FRAMES
        || gaps
            .iter()
            .any(|gap| gap.start >= gap.end || gap.end > frames)
    {
        return Err(invalid());
    }
    let mut decoded = 0;
    for sample in reader.samples::<i16>() {
        sample.map_err(|_| invalid())?;
        decoded += 1;
    }
    if decoded != frames {
        return Err(invalid());
    }
    Ok(frames)
}

#[derive(Debug, thiserror::Error)]
pub enum RecordingError {
    #[error("invalid recording request: {0}")]
    Invalid(String),
    #[error("recording identity conflict")]
    Conflict,
    #[error("recording not found")]
    NotFound,
    #[error("recording storage unavailable")]
    Upstream,
    #[error("recording metadata unavailable: {0}")]
    Json(#[from] serde_json::Error),
}

type Result<T> = std::result::Result<T, RecordingError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    GoogleMeet,
    SlackHuddle,
    MicrosoftTeams,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateOccurrence {
    pub provider: Provider,
    pub provider_occurrence_id: String,
    pub duration_frames: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TimingBasis {
    ProviderTimeline,
    OperatorVerified,
    DeviceClockHint,
    AudioAligned,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaptureMetadata {
    pub uploader_id: String,
    pub device_id: String,
    pub local_capture_id: String,
    #[serde(default)]
    pub segment_sequence: u32,
    pub local_session_id: String,
    pub local_transcript_id: String,
    pub sha256: String,
    pub offset_frames: i64,
    pub rank: u32,
    pub gaps: Vec<FrameRange>,
    pub timing_basis: TimingBasis,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaptureUpload {
    pub metadata: CaptureMetadata,
    pub wav_base64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Transcript {
    pub provider: String,
    pub model: String,
    pub result: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureProvenance {
    pub capture_id: String,
    pub metadata: CaptureMetadata,
    pub frame_count: u64,
    pub transcript: Option<Transcript>,
    pub transcript_status: String,
    pub deleted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OccurrenceManifest {
    pub occurrence_id: String,
    pub occurrence: CreateOccurrence,
    pub captures: Vec<CaptureProvenance>,
    pub sealed: bool,
}

pub struct SharedRecordingService {
    client: Client,
    url: String,
    service_key: String,
}

impl SharedRecordingService {
    pub fn supabase(url: &str, service_key: &str) -> Result<Self> {
        let parsed = url::Url::parse(url)
            .map_err(|_| RecordingError::Invalid("invalid Supabase URL".into()))?;
        if service_key.is_empty()
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || !(parsed.scheme() == "https"
                || (parsed.scheme() == "http"
                    && matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))))
        {
            return Err(RecordingError::Invalid(
                "invalid Supabase storage configuration".into(),
            ));
        }
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(120))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| RecordingError::Upstream)?,
            url: url.trim_end_matches('/').into(),
            service_key: service_key.into(),
        })
    }

    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.url))
            .header("apikey", &self.service_key)
            .bearer_auth(&self.service_key)
    }

    async fn bytes(&self, request: reqwest::RequestBuilder, limit: usize) -> Result<Vec<u8>> {
        let mut response = request.send().await.map_err(|_| RecordingError::Upstream)?;
        let status = response.status();
        if !status.is_success() {
            #[derive(Deserialize)]
            struct Failure {
                code: Option<String>,
            }
            let code = response
                .json::<Failure>()
                .await
                .ok()
                .and_then(|error| error.code);
            return Err(match (status, code.as_deref()) {
                (_, Some("23505")) => RecordingError::Conflict,
                (_, Some("P0002")) | (StatusCode::NOT_FOUND, _) => RecordingError::NotFound,
                (_, Some("22023")) => {
                    RecordingError::Invalid("source metadata exceeds ingestion limits".into())
                }
                _ => RecordingError::Upstream,
            });
        }
        if response
            .content_length()
            .is_some_and(|length| length > limit as u64)
        {
            return Err(RecordingError::Upstream);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| RecordingError::Upstream)?
        {
            if bytes.len() + chunk.len() > limit {
                return Err(RecordingError::Upstream);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    async fn json<T: serde::de::DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T> {
        Ok(serde_json::from_slice(
            &self.bytes(request, 32 * 1024 * 1024).await?,
        )?)
    }

    async fn rpc<T: serde::de::DeserializeOwned>(&self, name: &str, args: Value) -> Result<T> {
        let bytes = self
            .bytes(
                self.request(Method::POST, &format!("/rest/v1/rpc/{name}"))
                    .json(&args),
                32 * 1024 * 1024,
            )
            .await?;
        Ok(serde_json::from_slice(if bytes.is_empty() {
            b"null"
        } else {
            &bytes
        })?)
    }

    async fn occurrences(
        &self,
        workspace: &str,
        id: Option<&str>,
    ) -> Result<Vec<StoredOccurrence>> {
        identifier(workspace)?;
        let mut request = self
            .request(Method::GET, "/rest/v1/recording_occurrences")
            .query(&[
                ("workspace", format!("eq.{workspace}")),
                ("deleted", "eq.false".into()),
                ("select", "id,metadata,sealed".into()),
                ("order", "created_at.desc,id.asc".into()),
                ("limit", "500".into()),
            ]);
        if let Some(id) = id {
            identifier(id)?;
            request = request.query(&[("id", format!("eq.{id}"))]);
        }
        self.json(request).await
    }

    async fn captures(&self, workspace: &str, id: &str) -> Result<Vec<StoredCapture>> {
        let mut rows = Vec::new();
        loop {
            let page: Vec<StoredCapture> = self
                .json(
                    self.request(Method::GET, "/rest/v1/recording_captures")
                        .query(&[
                            ("workspace", format!("eq.{workspace}")),
                            ("occurrence", format!("eq.{id}")),
                            ("order", "id.asc".into()),
                            ("offset", rows.len().to_string()),
                            ("limit", "1000".into()),
                        ]),
                )
                .await?;
            let finished = page.len() < 1000;
            rows.extend(page);
            if finished {
                return Ok(rows);
            }
        }
    }

    pub async fn list_occurrences(&self, workspace: &str) -> Result<Vec<OccurrenceManifest>> {
        let mut items = Vec::new();
        for row in self.occurrences(workspace, None).await? {
            items.push(self.manifest(workspace, &row.id).await?);
        }
        Ok(items)
    }

    pub async fn admit_actor(
        &self,
        workspace: &str,
        id: &str,
        actor: &str,
        provider: Provider,
        provider_occurrence_id: &str,
        provider_started_at_ms: Option<i64>,
    ) -> Result<Option<i64>> {
        for value in [workspace, id, actor] {
            identifier(value)?;
        }
        let occurrence = CreateOccurrence {
            provider,
            provider_occurrence_id: provider_occurrence_id.into(),
            duration_frames: 1,
        };
        validate_occurrence(&occurrence)?;
        self.rpc("recording_admit", serde_json::json!({
            "p_workspace":workspace,"p_id":id,"p_actor":actor,
            "p_provider_key":format!("{:?}:{}", occurrence.provider, occurrence.provider_occurrence_id),
            "p_metadata":occurrence,"p_provider_started_at_ms":provider_started_at_ms,
        })).await
    }

    pub async fn actor_admitted(&self, workspace: &str, id: &str, actor: &str) -> Result<bool> {
        if self.occurrences(workspace, Some(id)).await?.is_empty() {
            return Ok(false);
        }
        let rows: Vec<Value> = self
            .json(
                self.request(Method::GET, "/rest/v1/recording_admissions")
                    .query(&[
                        ("workspace", format!("eq.{workspace}")),
                        ("occurrence", format!("eq.{id}")),
                        ("actor", format!("eq.{actor}")),
                        ("select", "actor".into()),
                        ("limit", "1".into()),
                    ]),
            )
            .await?;
        Ok(!rows.is_empty())
    }

    pub async fn manifest(&self, workspace: &str, id: &str) -> Result<OccurrenceManifest> {
        let row = self
            .occurrences(workspace, Some(id))
            .await?
            .into_iter()
            .next()
            .ok_or(RecordingError::NotFound)?;
        Ok(OccurrenceManifest {
            occurrence_id: row.id,
            occurrence: row.metadata,
            captures: self
                .captures(workspace, id)
                .await?
                .into_iter()
                .map(StoredCapture::provenance)
                .collect(),
            sealed: row.sealed,
        })
    }

    pub async fn source_audio(
        &self,
        workspace: &str,
        id: &str,
        capture_id: &str,
    ) -> Result<(CaptureProvenance, Vec<u8>)> {
        self.occurrences(workspace, Some(id))
            .await?
            .into_iter()
            .next()
            .ok_or(RecordingError::NotFound)?;
        let row = self
            .captures(workspace, id)
            .await?
            .into_iter()
            .find(|row| row.id == capture_id && !row.deleted)
            .ok_or(RecordingError::NotFound)?;
        if !row.object.starts_with(&format!("{workspace}/{id}/")) || row.object.contains("..") {
            return Err(RecordingError::Conflict);
        }
        let bytes = self
            .bytes(
                self.request(
                    Method::GET,
                    &format!(
                        "/storage/v1/object/authenticated/recording-audio/{}",
                        row.object
                    ),
                ),
                MAX_AUDIO_BYTES,
            )
            .await?;
        if hex_sha(&bytes) != row.metadata.sha256 {
            return Err(RecordingError::Conflict);
        }
        Ok((row.provenance(), bytes))
    }

    pub async fn save_source_transcript(
        &self,
        workspace: &str,
        id: &str,
        capture_id: &str,
        expected_sha: &str,
        transcript: Transcript,
    ) -> Result<()> {
        validate_transcript(&transcript)?;
        let _: Value = self.rpc("recording_save_transcript", serde_json::json!({
            "p_workspace":workspace,"p_occurrence":id,"p_id":capture_id,"p_expected_sha":expected_sha,"p_transcript":transcript,
        })).await?;
        Ok(())
    }

    pub async fn upload(
        &self,
        workspace: &str,
        id: &str,
        capture_id: &str,
        metadata: CaptureMetadata,
        wav: &[u8],
    ) -> Result<OccurrenceManifest> {
        for value in [
            workspace,
            id,
            capture_id,
            &metadata.uploader_id,
            &metadata.device_id,
            &metadata.local_capture_id,
            &metadata.local_session_id,
            &metadata.local_transcript_id,
        ] {
            identifier(value)?;
        }
        if wav.is_empty() || wav.len() > MAX_AUDIO_BYTES || metadata.gaps.len() > 4096 {
            return Err(RecordingError::Invalid(
                "audio or gap limit exceeded".into(),
            ));
        }
        let frames = validate_wav(wav, &metadata.gaps)?;
        if metadata.offset_frames.unsigned_abs() > MAX_FRAMES {
            return Err(RecordingError::Invalid(
                "capture timing hint exceeds supported limits".into(),
            ));
        }
        if hex_sha(wav) != metadata.sha256 {
            return Err(RecordingError::Conflict);
        }
        let manifest = self.manifest(workspace, id).await?;
        if let Some(existing) = manifest
            .captures
            .iter()
            .find(|row| row.capture_id == capture_id)
        {
            if existing.deleted || existing.metadata != metadata {
                return Err(RecordingError::Conflict);
            }
            return Ok(manifest);
        }
        if manifest.sealed {
            return Err(RecordingError::Conflict);
        }
        let object = format!(
            "{workspace}/{id}/{}-{}.wav",
            key_hash(&[workspace, id, capture_id]),
            metadata.sha256
        );
        let request = self
            .request(
                Method::POST,
                &format!("/storage/v1/object/recording-audio/{object}"),
            )
            .header("content-type", "audio/wav")
            .body(wav.to_vec());
        let response = request.send().await.map_err(|_| RecordingError::Upstream)?;
        if !response.status().is_success() {
            // A retry may find the immutable object from an upload whose database commit failed.
            let existing = self
                .bytes(
                    self.request(
                        Method::GET,
                        &format!("/storage/v1/object/authenticated/recording-audio/{object}"),
                    ),
                    MAX_AUDIO_BYTES,
                )
                .await?;
            if hex_sha(&existing) != metadata.sha256 {
                return Err(RecordingError::Conflict);
            }
        }
        let identity = serde_json::to_string(&(
            &metadata.uploader_id,
            &metadata.device_id,
            &metadata.local_capture_id,
            metadata.segment_sequence,
        ))?;
        let _: Value = self.rpc("recording_upload", serde_json::json!({
            "p_workspace":workspace,"p_occurrence":id,"p_id":capture_id,"p_identity":identity,"p_metadata":metadata,
            "p_object":object,"p_frames":frames,"p_size_bytes":wav.len(),
        })).await?;
        self.manifest(workspace, id).await
    }

    pub async fn seal(&self, workspace: &str, id: &str) -> Result<OccurrenceManifest> {
        let _: Value = self
            .rpc(
                "recording_seal",
                serde_json::json!({"p_workspace":workspace,"p_occurrence":id}),
            )
            .await?;
        self.manifest(workspace, id).await
    }

    async fn delete(&self, workspace: &str, id: &str, capture: Option<&str>) -> Result<()> {
        let objects: Vec<String> = self
            .rpc(
                "recording_delete",
                serde_json::json!({"p_workspace":workspace,"p_occurrence":id,"p_capture":capture}),
            )
            .await?;
        if !objects.is_empty() {
            self.bytes(
                self.request(Method::DELETE, "/storage/v1/object/recording-audio")
                    .json(&serde_json::json!({"prefixes":objects})),
                1024 * 1024,
            )
            .await?;
        }
        Ok(())
    }
    pub async fn delete_capture(&self, workspace: &str, id: &str, capture: &str) -> Result<()> {
        self.delete(workspace, id, Some(capture)).await
    }
    pub async fn delete_occurrence(&self, workspace: &str, id: &str) -> Result<()> {
        self.delete(workspace, id, None).await
    }
}

#[derive(Deserialize)]
struct StoredOccurrence {
    id: String,
    metadata: CreateOccurrence,
    sealed: bool,
}
#[derive(Deserialize)]
struct StoredCapture {
    id: String,
    metadata: CaptureMetadata,
    object: String,
    deleted: bool,
    frames: u64,
    transcript_json: Option<Transcript>,
}
impl StoredCapture {
    fn provenance(self) -> CaptureProvenance {
        CaptureProvenance {
            capture_id: self.id,
            metadata: self.metadata,
            frame_count: self.frames,
            transcript_status: if self.transcript_json.is_some() {
                "ready"
            } else {
                "pending"
            }
            .into(),
            transcript: self.transcript_json,
            deleted: self.deleted,
        }
    }
}

fn validate_transcript(transcript: &Transcript) -> Result<()> {
    identifier(&transcript.provider)?;
    if transcript.model.is_empty()
        || transcript.model.len() > 256
        || !transcript
            .model
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/_-.:".contains(&byte))
        || serde_json::to_vec(&transcript.result)?.len() > 16 * 1024 * 1024
    {
        return Err(RecordingError::Invalid(
            "invalid transcript model or size".into(),
        ));
    }
    Ok(())
}

fn identifier(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-.:".contains(&byte))
    {
        return Err(RecordingError::Invalid("invalid identifier".into()));
    }
    Ok(())
}

pub(crate) fn local_capture_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
        && value != "00000000-0000-0000-0000-000000000000"
}

fn validate_occurrence(request: &CreateOccurrence) -> Result<()> {
    if request.duration_frames == 0 || request.duration_frames > MAX_FRAMES {
        return Err(RecordingError::Invalid(
            "invalid fixed timeline length".into(),
        ));
    }
    let valid = match request.provider {
        Provider::GoogleMeet => request
            .provider_occurrence_id
            .strip_prefix("conferenceRecords/")
            .is_some_and(|id| {
                !id.is_empty()
                    && id.len() <= 128
                    && id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
            }),
        Provider::MicrosoftTeams => {
            request
                .provider_occurrence_id
                .split_once('/')
                .is_some_and(|(actor, capture)| {
                    identifier(actor).is_ok() && local_capture_uuid(capture)
                })
        }
        Provider::SlackHuddle => {
            request
                .provider_occurrence_id
                .split_once('/')
                .is_some_and(|(team, call)| {
                    team.starts_with('T')
                        && call.starts_with('R')
                        && team.len() > 1
                        && call.len() > 1
                        && team.len() <= 64
                        && call.len() <= 64
                        && team
                            .bytes()
                            .chain(call.bytes())
                            .all(|byte| byte.is_ascii_alphanumeric())
                })
        }
    };
    if !valid {
        return Err(RecordingError::Invalid("provider occurrence ID or scoped Teams local capture required; room URLs, channels and calendar aliases are not accepted".into()));
    }
    Ok(())
}

fn key_hash(parts: &[&str]) -> String {
    hex_sha(
        serde_json::to_string(parts)
            .expect("string array serializes")
            .as_bytes(),
    )
}
pub(crate) fn hex_sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
