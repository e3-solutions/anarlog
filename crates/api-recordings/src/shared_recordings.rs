use std::{
    collections::HashSet,
    path::{Path as FsPath, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{
    Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use tokio::sync::Mutex;

const MAX_AUDIO_BYTES: usize = 100 * 1024 * 1024;

const MAX_FRAMES: u64 = 16_000 * 60 * 60 * 4;
const MAX_SOURCES: usize = 32;

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
    #[error("recording identity or revision conflict")]
    Conflict,
    #[error("recording not found")]
    NotFound,
    #[error("recording storage unavailable: {0}")]
    Storage(#[from] sqlx::Error),
    #[error("recording files unavailable: {0}")]
    Io(#[from] std::io::Error),
    #[error("recording metadata unavailable: {0}")]
    Json(#[from] serde_json::Error),
}

type Result<T> = std::result::Result<T, RecordingError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    GoogleMeet,
    SlackHuddle,
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
pub struct Revision {
    pub number: u64,
    pub revision: String,
    #[serde(default = "legacy_kind")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<Value>,
    pub transcript: Option<Transcript>,
    pub transcript_status: String,
}

fn legacy_kind() -> String {
    "legacy_audio".into()
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
    pub current: Option<Revision>,
    pub inputs_changed: bool,
    pub sealed: bool,
}

pub struct SharedRecordingService {
    pool: SqlitePool,
    directory: PathBuf,
    gate: Mutex<()>,
    _lock: std::fs::File,
}

impl SharedRecordingService {
    pub async fn open(directory: impl AsRef<FsPath>) -> Result<Self> {
        let directory = directory.as_ref().to_path_buf();
        std::fs::create_dir_all(&directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
        }
        let mut lock_options = std::fs::OpenOptions::new();
        lock_options
            .create(true)
            .read(true)
            .write(true)
            .truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            lock_options.mode(0o600);
        }
        let lock = lock_options.open(directory.join("service.lock"))?;
        lock.try_lock().map_err(|error| {
            RecordingError::Invalid(format!("storage directory is already in use: {error}"))
        })?;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(directory.join("recordings.sqlite"))
                    .create_if_missing(true)
                    .foreign_keys(true)
                    .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal),
            )
            .await?;
        sqlx::raw_sql("CREATE TABLE IF NOT EXISTS occurrences (
            workspace TEXT NOT NULL, id TEXT NOT NULL, provider_key TEXT NOT NULL,
            metadata TEXT NOT NULL, deleted INTEGER NOT NULL DEFAULT 0,
            changed INTEGER NOT NULL DEFAULT 1, sealed INTEGER NOT NULL DEFAULT 0, next_revision INTEGER NOT NULL DEFAULT 1,
            current_json TEXT, PRIMARY KEY(workspace,id), UNIQUE(workspace,provider_key));
            CREATE TABLE IF NOT EXISTS captures (
            workspace TEXT NOT NULL, occurrence TEXT NOT NULL, id TEXT NOT NULL,
            identity TEXT NOT NULL, metadata TEXT NOT NULL, object TEXT NOT NULL,
            deleted INTEGER NOT NULL DEFAULT 0, frames INTEGER NOT NULL, size_bytes INTEGER NOT NULL, PRIMARY KEY(workspace,occurrence,id),
            UNIQUE(workspace,identity), FOREIGN KEY(workspace,occurrence) REFERENCES occurrences(workspace,id));
            CREATE TABLE IF NOT EXISTS revisions (
            workspace TEXT NOT NULL, occurrence TEXT NOT NULL, number INTEGER NOT NULL,
            revision TEXT NOT NULL, object TEXT NOT NULL, revision_json TEXT NOT NULL, PRIMARY KEY(workspace,occurrence,number));").execute(&pool).await?;
        let columns = sqlx::query("PRAGMA table_info(occurrences)")
            .fetch_all(&pool)
            .await?;
        if !columns
            .iter()
            .any(|column| column.get::<String, _>("name") == "sealed")
        {
            sqlx::query("ALTER TABLE occurrences ADD COLUMN sealed INTEGER NOT NULL DEFAULT 0")
                .execute(&pool)
                .await?;
        }
        if !columns
            .iter()
            .any(|column| column.get::<String, _>("name") == "automatic")
        {
            sqlx::query("ALTER TABLE occurrences ADD COLUMN automatic INTEGER NOT NULL DEFAULT 0")
                .execute(&pool)
                .await?;
            sqlx::query("ALTER TABLE occurrences ADD COLUMN provider_started_at_ms INTEGER")
                .execute(&pool)
                .await?;
        }
        sqlx::raw_sql(
            "CREATE TABLE IF NOT EXISTS admissions (
            workspace TEXT NOT NULL, occurrence TEXT NOT NULL, actor TEXT NOT NULL,
            PRIMARY KEY(workspace,occurrence,actor),
            FOREIGN KEY(workspace,occurrence) REFERENCES occurrences(workspace,id));",
        )
        .execute(&pool)
        .await?;
        let capture_columns = sqlx::query("PRAGMA table_info(captures)")
            .fetch_all(&pool)
            .await?;
        if !capture_columns
            .iter()
            .any(|column| column.get::<String, _>("name") == "transcript_json")
        {
            sqlx::query("ALTER TABLE captures ADD COLUMN transcript_json TEXT")
                .execute(&pool)
                .await?;
        }
        let service = Self {
            pool,
            directory,
            gate: Mutex::new(()),
            _lock: lock,
        };
        service.recover_files().await?;
        Ok(service)
    }

    async fn recover_files(&self) -> Result<()> {
        sqlx::query("DELETE FROM revisions WHERE EXISTS (SELECT 1 FROM occurrences WHERE occurrences.workspace=revisions.workspace AND occurrences.id=revisions.occurrence AND (occurrences.deleted=1 OR occurrences.current_json IS NULL))").execute(&self.pool).await?;
        let mut retained: HashSet<String> =
            sqlx::query_scalar("SELECT object FROM captures WHERE deleted=0")
                .fetch_all(&self.pool)
                .await?
                .into_iter()
                .collect();
        retained.extend(
            sqlx::query_scalar::<_, String>("SELECT object FROM revisions WHERE object<>''")
                .fetch_all(&self.pool)
                .await?,
        );
        for entry in std::fs::read_dir(&self.directory)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type()?.is_file()
                && (name.ends_with(".pending")
                    || ((name.starts_with("source-") || name.starts_with("canonical-"))
                        && name.ends_with(".wav")
                        && !retained.contains(&name)))
            {
                remove_if_exists(&entry.path())?;
            }
        }
        Ok(())
    }

    pub async fn list_occurrences(&self, workspace: &str) -> Result<Vec<OccurrenceManifest>> {
        let _guard = self.gate.lock().await;
        let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM occurrences WHERE workspace=? AND deleted=0 ORDER BY rowid DESC LIMIT 500")
            .bind(workspace).fetch_all(&self.pool).await?;
        let mut items = Vec::with_capacity(ids.len());
        for id in ids {
            items.push(self.manifest_unlocked(workspace, &id).await?);
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
        identifier(workspace)?;
        identifier(id)?;
        identifier(actor)?;
        let occurrence = CreateOccurrence {
            provider,
            provider_occurrence_id: provider_occurrence_id.to_owned(),
            duration_frames: 1,
        };
        validate_occurrence(&occurrence)?;
        let _guard = self.gate.lock().await;
        let mut transaction = self.pool.begin().await?;
        let key = format!(
            "{:?}:{}",
            occurrence.provider, occurrence.provider_occurrence_id
        );
        let epoch = if let Some(existing) = sqlx::query(
            "SELECT provider_key,deleted,provider_started_at_ms FROM occurrences WHERE workspace=? AND id=?",
        )
        .bind(workspace)
        .bind(id)
        .fetch_optional(&mut *transaction)
        .await?
        {
            if existing.get::<i64, _>("deleted") != 0
                || existing.get::<String, _>("provider_key") != key
            {
                return Err(RecordingError::Conflict);
            }
            existing.get::<Option<i64>, _>("provider_started_at_ms")
        } else {
            let epoch = provider_started_at_ms.unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
            sqlx::query("INSERT INTO occurrences(workspace,id,provider_key,metadata,automatic,provider_started_at_ms) VALUES(?,?,?,?,1,?)")
                .bind(workspace).bind(id).bind(key).bind(serde_json::to_string(&occurrence)?)
                .bind(epoch).execute(&mut *transaction).await.map_err(map_constraint)?;
            Some(epoch)
        };
        sqlx::query("INSERT INTO admissions(workspace,occurrence,actor) VALUES(?,?,?) ON CONFLICT DO NOTHING")
            .bind(workspace).bind(id).bind(actor).execute(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(epoch)
    }

    pub async fn actor_admitted(&self, workspace: &str, id: &str, actor: &str) -> Result<bool> {
        Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM admissions a JOIN occurrences o ON o.workspace=a.workspace AND o.id=a.occurrence WHERE a.workspace=? AND a.occurrence=? AND a.actor=? AND o.deleted=0)")
            .bind(workspace).bind(id).bind(actor).fetch_one(&self.pool).await?)
    }

    pub async fn source_audio(
        &self,
        workspace: &str,
        id: &str,
        capture_id: &str,
    ) -> Result<(CaptureProvenance, Vec<u8>)> {
        let _guard = self.gate.lock().await;
        let capture = self
            .manifest_unlocked(workspace, id)
            .await?
            .captures
            .into_iter()
            .find(|capture| capture.capture_id == capture_id && !capture.deleted)
            .ok_or(RecordingError::NotFound)?;
        let object: String = sqlx::query_scalar(
            "SELECT object FROM captures WHERE workspace=? AND occurrence=? AND id=? AND deleted=0",
        )
        .bind(workspace)
        .bind(id)
        .bind(capture_id)
        .fetch_one(&self.pool)
        .await?;
        let wav = std::fs::read(self.directory.join(object))?;
        if hex_sha(&wav) != capture.metadata.sha256 {
            return Err(RecordingError::Conflict);
        }
        Ok((capture, wav))
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
        let _guard = self.gate.lock().await;
        self.occurrence_unlocked(workspace, id).await?;
        let row = sqlx::query("SELECT metadata,transcript_json FROM captures WHERE workspace=? AND occurrence=? AND id=? AND deleted=0")
            .bind(workspace).bind(id).bind(capture_id).fetch_optional(&self.pool).await?.ok_or(RecordingError::NotFound)?;
        let metadata: CaptureMetadata = serde_json::from_str(&row.get::<String, _>("metadata"))?;
        if metadata.sha256 != expected_sha {
            return Err(RecordingError::Conflict);
        }
        if let Some(existing) = row.get::<Option<String>, _>("transcript_json") {
            if serde_json::from_str::<Transcript>(&existing)? != transcript {
                return Err(RecordingError::Conflict);
            }
            return Ok(());
        }
        let mut transaction = self.pool.begin().await?;
        sqlx::query("UPDATE captures SET transcript_json=? WHERE workspace=? AND occurrence=? AND id=? AND deleted=0")
            .bind(serde_json::to_string(&transcript)?).bind(workspace).bind(id).bind(capture_id).execute(&mut *transaction).await?;
        sqlx::query("UPDATE occurrences SET changed=1 WHERE workspace=? AND id=?")
            .bind(workspace)
            .bind(id)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
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
        identifier(capture_id)?;
        for value in [
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
        let _guard = self.gate.lock().await;
        let (mut occurrence, _, _) = self.occurrence_unlocked(workspace, id).await?;
        let automatic: bool =
            sqlx::query_scalar("SELECT automatic FROM occurrences WHERE workspace=? AND id=?")
                .bind(workspace)
                .bind(id)
                .fetch_one(&self.pool)
                .await?;
        if automatic {
            let end = metadata.offset_frames.saturating_add(frames as i64).max(1) as u64;
            if end > MAX_FRAMES {
                return Err(RecordingError::Invalid(
                    "automatic occurrence exceeds supported timeline".into(),
                ));
            }
            occurrence.duration_frames = occurrence.duration_frames.max(end);
        }
        let json = serde_json::to_string(&metadata)?;
        let identity = serde_json::to_string(&(
            &metadata.uploader_id,
            &metadata.device_id,
            &metadata.local_capture_id,
            metadata.segment_sequence,
        ))?;
        if let Some(row) = sqlx::query(
            "SELECT metadata,deleted FROM captures WHERE workspace=? AND occurrence=? AND id=?",
        )
        .bind(workspace)
        .bind(id)
        .bind(capture_id)
        .fetch_optional(&self.pool)
        .await?
        {
            if row.get::<i64, _>("deleted") != 0 || row.get::<String, _>("metadata") != json {
                return Err(RecordingError::Conflict);
            }
            return self.manifest_unlocked(workspace, id).await;
        }
        if self.sealed_unlocked(workspace, id).await? {
            return Err(RecordingError::Conflict);
        }
        let limits=sqlx::query("SELECT count(*) AS count,coalesce(sum(frames),0) AS frames FROM captures WHERE workspace=? AND occurrence=? AND deleted=0")
            .bind(workspace).bind(id).fetch_one(&self.pool).await?;
        if limits.get::<i64, _>("count") >= MAX_SOURCES as i64
            || limits.get::<i64, _>("frames") as u64 + frames > MAX_FRAMES * 2
        {
            return Err(RecordingError::Invalid(
                "occurrence source limit exceeded".into(),
            ));
        }
        let object = format!(
            "source-{}-{}.wav",
            key_hash(&[workspace, id, capture_id]),
            metadata.sha256
        );
        let mut transaction = self.pool.begin().await?;
        if let Err(error) = sqlx::query("INSERT INTO captures(workspace,occurrence,id,identity,metadata,object,frames,size_bytes) VALUES(?,?,?,?,?,?,?,?)")
            .bind(workspace).bind(id).bind(capture_id).bind(identity).bind(json).bind(&object).bind(frames as i64).bind(wav.len() as i64).execute(&mut *transaction).await { return Err(map_constraint(error)); }
        write_atomic(&self.directory.join(&object), wav)?;
        if automatic {
            sqlx::query("UPDATE occurrences SET metadata=? WHERE workspace=? AND id=?")
                .bind(serde_json::to_string(&occurrence)?)
                .bind(workspace)
                .bind(id)
                .execute(&mut *transaction)
                .await?;
        }
        sqlx::query("UPDATE occurrences SET changed=1 WHERE workspace=? AND id=?")
            .bind(workspace)
            .bind(id)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        self.manifest_unlocked(workspace, id).await
    }

    pub async fn manifest(&self, workspace: &str, id: &str) -> Result<OccurrenceManifest> {
        let _guard = self.gate.lock().await;
        self.manifest_unlocked(workspace, id).await
    }

    async fn occurrence_unlocked(
        &self,
        workspace: &str,
        id: &str,
    ) -> Result<(CreateOccurrence, Option<Revision>, bool)> {
        let row = sqlx::query("SELECT metadata,current_json,changed FROM occurrences WHERE workspace=? AND id=? AND deleted=0")
            .bind(workspace).bind(id).fetch_optional(&self.pool).await?.ok_or(RecordingError::NotFound)?;
        let metadata = serde_json::from_str(&row.get::<String, _>("metadata"))?;
        let revision = row
            .get::<Option<String>, _>("current_json")
            .map(|json| serde_json::from_str(&json))
            .transpose()?;
        Ok((metadata, revision, row.get::<i64, _>("changed") != 0))
    }

    async fn manifest_unlocked(&self, workspace: &str, id: &str) -> Result<OccurrenceManifest> {
        let (occurrence, current, inputs_changed) = self.occurrence_unlocked(workspace, id).await?;
        let rows = sqlx::query("SELECT id,metadata,deleted,frames,transcript_json FROM captures WHERE workspace=? AND occurrence=? ORDER BY id")
            .bind(workspace).bind(id).fetch_all(&self.pool).await?;
        let captures = rows
            .into_iter()
            .map(|row| {
                Ok(CaptureProvenance {
                    capture_id: row.get("id"),
                    metadata: serde_json::from_str(&row.get::<String, _>("metadata"))?,
                    frame_count: row.get::<i64, _>("frames") as u64,
                    transcript: row
                        .get::<Option<String>, _>("transcript_json")
                        .map(|json| serde_json::from_str(&json))
                        .transpose()?,
                    transcript_status: if row.get::<Option<String>, _>("transcript_json").is_some()
                    {
                        "ready"
                    } else {
                        "pending"
                    }
                    .into(),
                    deleted: row.get::<i64, _>("deleted") != 0,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(OccurrenceManifest {
            occurrence_id: id.into(),
            occurrence,
            captures,
            current,
            inputs_changed,
            sealed: self.sealed_unlocked(workspace, id).await?,
        })
    }

    async fn sealed_unlocked(&self, workspace: &str, id: &str) -> Result<bool> {
        sqlx::query_scalar(
            "SELECT sealed FROM occurrences WHERE workspace=? AND id=? AND deleted=0",
        )
        .bind(workspace)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(RecordingError::NotFound)
    }

    pub async fn seal(&self, workspace: &str, id: &str) -> Result<OccurrenceManifest> {
        let _guard = self.gate.lock().await;
        self.occurrence_unlocked(workspace, id).await?;
        let has_capture:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM captures WHERE workspace=? AND occurrence=? AND deleted=0)").bind(workspace).bind(id).fetch_one(&self.pool).await?;
        if !has_capture {
            return Err(RecordingError::Conflict);
        }
        sqlx::query("UPDATE occurrences SET sealed=1 WHERE workspace=? AND id=?")
            .bind(workspace)
            .bind(id)
            .execute(&self.pool)
            .await?;
        self.manifest_unlocked(workspace, id).await
    }

    pub async fn delete_capture(&self, workspace: &str, id: &str, capture_id: &str) -> Result<()> {
        let _guard = self.gate.lock().await;
        self.occurrence_unlocked(workspace, id).await?;
        let row = sqlx::query(
            "SELECT object,deleted FROM captures WHERE workspace=? AND occurrence=? AND id=?",
        )
        .bind(workspace)
        .bind(id)
        .bind(capture_id)
        .fetch_optional(&self.pool)
        .await?;
        let row = row.ok_or(RecordingError::NotFound)?;
        let object: String = row.get("object");
        if row.get::<i64, _>("deleted") != 0 {
            remove_if_exists(&self.directory.join(object))?;
            return Ok(());
        }
        let derived = self.derived_objects_unlocked(workspace, id).await?;
        let mut transaction = self.pool.begin().await?;
        sqlx::query("UPDATE captures SET deleted=1,transcript_json=NULL WHERE workspace=? AND occurrence=? AND id=?")
            .bind(workspace)
            .bind(id)
            .bind(capture_id)
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            "UPDATE occurrences SET current_json=NULL,changed=1 WHERE workspace=? AND id=?",
        )
        .bind(workspace)
        .bind(id)
        .execute(&mut *transaction)
        .await?;
        sqlx::query("DELETE FROM revisions WHERE workspace=? AND occurrence=?")
            .bind(workspace)
            .bind(id)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        for object in derived {
            remove_if_exists(&self.directory.join(object))?;
        }
        remove_if_exists(&self.directory.join(object))?;
        Ok(())
    }

    pub async fn delete_occurrence(&self, workspace: &str, id: &str) -> Result<()> {
        let _guard = self.gate.lock().await;
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM occurrences WHERE workspace=? AND id=?)",
        )
        .bind(workspace)
        .bind(id)
        .fetch_one(&self.pool)
        .await?;
        if !exists {
            return Err(RecordingError::NotFound);
        }
        let objects: Vec<String> =
            sqlx::query_scalar("SELECT object FROM captures WHERE workspace=? AND occurrence=?")
                .bind(workspace)
                .bind(id)
                .fetch_all(&self.pool)
                .await?;
        let derived = self.derived_objects_unlocked(workspace, id).await?;
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            "UPDATE occurrences SET deleted=1,current_json=NULL WHERE workspace=? AND id=?",
        )
        .bind(workspace)
        .bind(id)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "UPDATE captures SET deleted=1,transcript_json=NULL WHERE workspace=? AND occurrence=?",
        )
        .bind(workspace)
        .bind(id)
        .execute(&mut *transaction)
        .await?;
        sqlx::query("DELETE FROM revisions WHERE workspace=? AND occurrence=?")
            .bind(workspace)
            .bind(id)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        for object in derived {
            remove_if_exists(&self.directory.join(object))?;
        }
        for object in objects {
            remove_if_exists(&self.directory.join(object))?;
        }
        Ok(())
    }

    async fn derived_objects_unlocked(&self, workspace: &str, id: &str) -> Result<Vec<String>> {
        let objects: Vec<String> = sqlx::query_scalar(
            "SELECT object FROM revisions WHERE workspace=? AND occurrence=? AND object<>''",
        )
        .bind(workspace)
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        Ok(objects)
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
        return Err(RecordingError::Invalid("actual provider occurrence ID required; room URLs, channels and calendar aliases are not accepted".into()));
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
    format!("{:x}", Sha256::digest(bytes))
}
fn map_constraint(error: sqlx::Error) -> RecordingError {
    if error
        .as_database_error()
        .is_some_and(|error| error.is_unique_violation())
    {
        RecordingError::Conflict
    } else {
        RecordingError::Storage(error)
    }
}
fn write_atomic(path: &FsPath, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let temporary = path.with_extension("pending");
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&temporary, path)?;
    std::fs::File::open(
        path.parent()
            .ok_or_else(|| RecordingError::Invalid("invalid storage path".into()))?,
    )?
    .sync_all()?;
    Ok(())
}
fn remove_if_exists(path: &FsPath) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
