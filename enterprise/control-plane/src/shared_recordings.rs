use std::{
    collections::HashSet,
    path::{Path as FsPath, PathBuf},
    sync::Arc,
};

use anlg_shared_recording::{
    FrameRange, MAX_FRAMES, MAX_SOURCES, Manifest, align_clock_hints, decode_source, render,
};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{
    Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use tokio::sync::Mutex;

use crate::auth::WorkspaceAuthenticator;

const MAX_AUDIO_BYTES: usize = 100 * 1024 * 1024;
const MAX_BODY_BYTES: usize = 140 * 1024 * 1024;

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
    pub audio_sha256: String,
    pub manifest: Manifest,
    pub transcript: Option<Transcript>,
    pub transcript_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureProvenance {
    pub capture_id: String,
    pub metadata: CaptureMetadata,
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
            sqlx::query_scalar::<_, String>("SELECT object FROM revisions")
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

    pub async fn automatic_admit(
        &self,
        workspace: &str,
        id: &str,
        provider: Provider,
        provider_occurrence_id: &str,
        provider_started_at_ms: Option<i64>,
    ) -> Result<OccurrenceManifest> {
        identifier(workspace)?;
        identifier(id)?;
        let occurrence = CreateOccurrence {
            provider,
            provider_occurrence_id: provider_occurrence_id.to_owned(),
            duration_frames: 1,
        };
        validate_occurrence(&occurrence)?;
        let _guard = self.gate.lock().await;
        let key = format!(
            "{:?}:{}",
            occurrence.provider, occurrence.provider_occurrence_id
        );
        if let Some(existing) =
            sqlx::query("SELECT provider_key,deleted FROM occurrences WHERE workspace=? AND id=?")
                .bind(workspace)
                .bind(id)
                .fetch_optional(&self.pool)
                .await?
        {
            if existing.get::<i64, _>("deleted") != 0
                || existing.get::<String, _>("provider_key") != key
            {
                return Err(RecordingError::Conflict);
            }
        } else {
            sqlx::query("INSERT INTO occurrences(workspace,id,provider_key,metadata,automatic,provider_started_at_ms) VALUES(?,?,?,?,1,?)")
                .bind(workspace).bind(id).bind(key).bind(serde_json::to_string(&occurrence)?)
                .bind(provider_started_at_ms.unwrap_or_else(|| chrono::Utc::now().timestamp_millis()))
                .execute(&self.pool).await.map_err(map_constraint)?;
        }
        self.manifest_unlocked(workspace, id).await
    }

    pub async fn occurrence_epoch(&self, workspace: &str, id: &str) -> Result<Option<i64>> {
        sqlx::query_scalar("SELECT provider_started_at_ms FROM occurrences WHERE workspace=? AND id=? AND deleted=0")
            .bind(workspace).bind(id).fetch_optional(&self.pool).await?.ok_or(RecordingError::NotFound)
    }

    pub async fn grant_actor_admission(
        &self,
        workspace: &str,
        id: &str,
        actor: &str,
    ) -> Result<()> {
        identifier(actor)?;
        let _guard = self.gate.lock().await;
        self.occurrence_unlocked(workspace, id).await?;
        sqlx::query("INSERT INTO admissions(workspace,occurrence,actor) VALUES(?,?,?) ON CONFLICT DO NOTHING")
            .bind(workspace).bind(id).bind(actor).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn actor_admitted(&self, workspace: &str, id: &str, actor: &str) -> Result<bool> {
        Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM admissions a JOIN occurrences o ON o.workspace=a.workspace AND o.id=a.occurrence WHERE a.workspace=? AND a.occurrence=? AND a.actor=? AND o.deleted=0)")
            .bind(workspace).bind(id).bind(actor).fetch_one(&self.pool).await?)
    }

    pub async fn revision_audio(
        &self,
        workspace: &str,
        id: &str,
        revision_hash: &str,
    ) -> Result<(Revision, Vec<u8>)> {
        let _guard = self.gate.lock().await;
        self.occurrence_unlocked(workspace, id).await?;
        let row = sqlx::query("SELECT revision_json,object FROM revisions WHERE workspace=? AND occurrence=? AND revision=? ORDER BY number DESC LIMIT 1")
            .bind(workspace).bind(id).bind(revision_hash).fetch_optional(&self.pool).await?.ok_or(RecordingError::NotFound)?;
        let revision: Revision = serde_json::from_str(&row.get::<String, _>("revision_json"))?;
        let wav = std::fs::read(self.directory.join(row.get::<String, _>("object")))?;
        if hex_sha(&wav) != revision.audio_sha256 {
            return Err(RecordingError::Conflict);
        }
        Ok((revision, wav))
    }

    pub async fn create_occurrence(
        &self,
        workspace: &str,
        id: &str,
        request: CreateOccurrence,
    ) -> Result<OccurrenceManifest> {
        identifier(workspace)?;
        identifier(id)?;
        validate_occurrence(&request)?;
        let _guard = self.gate.lock().await;
        let metadata = serde_json::to_string(&request)?;
        let key = format!("{:?}:{}", request.provider, request.provider_occurrence_id);
        if let Some(row) =
            sqlx::query("SELECT metadata,deleted FROM occurrences WHERE workspace=? AND id=?")
                .bind(workspace)
                .bind(id)
                .fetch_optional(&self.pool)
                .await?
        {
            if row.get::<i64, _>("deleted") != 0 || row.get::<String, _>("metadata") != metadata {
                return Err(RecordingError::Conflict);
            }
        } else {
            let result = sqlx::query(
                "INSERT INTO occurrences(workspace,id,provider_key,metadata) VALUES(?,?,?,?)",
            )
            .bind(workspace)
            .bind(id)
            .bind(key)
            .bind(metadata)
            .execute(&self.pool)
            .await;
            if let Err(error) = result {
                return Err(map_constraint(error));
            }
        }
        self.manifest_unlocked(workspace, id).await
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
        let source = decode_source(
            capture_id.to_owned(),
            metadata.rank,
            metadata.offset_frames,
            wav,
            metadata.gaps.clone(),
        )
        .map_err(|error| RecordingError::Invalid(error.to_string()))?;
        if source.sha256 != metadata.sha256 {
            return Err(RecordingError::Conflict);
        }
        let _guard = self.gate.lock().await;
        let (mut occurrence, _, _, _) = self.occurrence_unlocked(workspace, id).await?;
        let automatic: bool =
            sqlx::query_scalar("SELECT automatic FROM occurrences WHERE workspace=? AND id=?")
                .bind(workspace)
                .bind(id)
                .fetch_one(&self.pool)
                .await?;
        if automatic {
            let end = metadata
                .offset_frames
                .saturating_add(source.samples.len() as i64)
                .max(1) as u64;
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
            || limits.get::<i64, _>("frames") as u64 + source.samples.len() as u64 > MAX_FRAMES * 2
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
            .bind(workspace).bind(id).bind(capture_id).bind(identity).bind(json).bind(&object).bind(source.samples.len() as i64).bind(wav.len() as i64).execute(&mut *transaction).await { return Err(map_constraint(error)); }
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
    ) -> Result<(CreateOccurrence, Option<Revision>, bool, u64)> {
        let row = sqlx::query("SELECT metadata,current_json,changed,next_revision FROM occurrences WHERE workspace=? AND id=? AND deleted=0")
            .bind(workspace).bind(id).fetch_optional(&self.pool).await?.ok_or(RecordingError::NotFound)?;
        let metadata = serde_json::from_str(&row.get::<String, _>("metadata"))?;
        let revision = row
            .get::<Option<String>, _>("current_json")
            .map(|json| serde_json::from_str(&json))
            .transpose()?;
        Ok((
            metadata,
            revision,
            row.get::<i64, _>("changed") != 0,
            row.get::<i64, _>("next_revision") as u64,
        ))
    }

    async fn manifest_unlocked(&self, workspace: &str, id: &str) -> Result<OccurrenceManifest> {
        let (occurrence, current, inputs_changed, _) =
            self.occurrence_unlocked(workspace, id).await?;
        let rows = sqlx::query("SELECT id,metadata,deleted FROM captures WHERE workspace=? AND occurrence=? ORDER BY id")
            .bind(workspace).bind(id).fetch_all(&self.pool).await?;
        let captures = rows
            .into_iter()
            .map(|row| {
                Ok(CaptureProvenance {
                    capture_id: row.get("id"),
                    metadata: serde_json::from_str(&row.get::<String, _>("metadata"))?,
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

    pub async fn seal(&self, workspace: &str, id: &str) -> Result<Revision> {
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
        self.finalize_unlocked(workspace, id).await
    }

    pub async fn finalize(&self, workspace: &str, id: &str) -> Result<Revision> {
        let _guard = self.gate.lock().await;
        self.finalize_unlocked(workspace, id).await
    }

    async fn finalize_unlocked(&self, workspace: &str, id: &str) -> Result<Revision> {
        let (occurrence, current, _, number) = self.occurrence_unlocked(workspace, id).await?;
        let rows = sqlx::query("SELECT id,metadata,object FROM captures WHERE workspace=? AND occurrence=? AND deleted=0 ORDER BY id")
            .bind(workspace).bind(id).fetch_all(&self.pool).await?;
        if rows.is_empty() {
            return Err(RecordingError::Conflict);
        }
        let mut sources = Vec::with_capacity(rows.len());
        for row in rows {
            let metadata: CaptureMetadata =
                serde_json::from_str(&row.get::<String, _>("metadata"))?;
            let bytes = std::fs::read(self.directory.join(row.get::<String, _>("object")))?;
            let group = key_hash(&[
                &metadata.uploader_id,
                &metadata.device_id,
                &metadata.local_capture_id,
            ]);
            let trusted = matches!(
                metadata.timing_basis,
                TimingBasis::ProviderTimeline | TimingBasis::OperatorVerified
            );
            let source = decode_source(
                row.get("id"),
                metadata.rank,
                metadata.offset_frames,
                &bytes,
                metadata.gaps,
            )
            .map_err(|error| RecordingError::Invalid(error.to_string()))?;
            if source.sha256 != metadata.sha256 {
                return Err(RecordingError::Conflict);
            }
            sources.push((source, group, trusted));
        }
        let sources = align_clock_hints(sources);
        let duration_frames = sources
            .iter()
            .map(|source| (source.offset_frames + source.samples.len() as i64).max(0) as u64)
            .max()
            .unwrap_or(0)
            .max(occurrence.duration_frames);
        let canonical = render(duration_frames, &sources)
            .map_err(|error| RecordingError::Invalid(error.to_string()))?;
        if let Some(current) = current.filter(|current| current.revision == canonical.revision) {
            let object = format!(
                "canonical-{}-{}.wav",
                key_hash(&[workspace, id]),
                current.revision
            );
            let path = self.directory.join(object);
            if std::fs::read(&path).ok().as_deref() != Some(canonical.wav.as_slice()) {
                write_atomic(&path, &canonical.wav)?;
            }
            sqlx::query("UPDATE occurrences SET changed=0 WHERE workspace=? AND id=?")
                .bind(workspace)
                .bind(id)
                .execute(&self.pool)
                .await?;
            return Ok(current);
        }
        let revision = Revision {
            number,
            revision: canonical.revision,
            audio_sha256: canonical.sha256,
            manifest: canonical.manifest,
            transcript: None,
            transcript_status: "pending".into(),
        };
        let object = format!(
            "canonical-{}-{}.wav",
            key_hash(&[workspace, id]),
            revision.revision
        );
        write_atomic(&self.directory.join(&object), &canonical.wav)?;
        let mut transaction = self.pool.begin().await?;
        sqlx::query("INSERT INTO revisions(workspace,occurrence,number,revision,object,revision_json) VALUES(?,?,?,?,?,?)")
            .bind(workspace).bind(id).bind(number as i64).bind(&revision.revision).bind(object).bind(serde_json::to_string(&revision)?).execute(&mut *transaction).await?;
        sqlx::query("UPDATE occurrences SET current_json=?,next_revision=?,changed=0 WHERE workspace=? AND id=?")
            .bind(serde_json::to_string(&revision)?).bind((number+1) as i64).bind(workspace).bind(id).execute(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(revision)
    }

    pub async fn canonical_audio(&self, workspace: &str, id: &str) -> Result<(Revision, Vec<u8>)> {
        let _guard = self.gate.lock().await;
        let (_, revision, _, _) = self.occurrence_unlocked(workspace, id).await?;
        let revision = revision.ok_or(RecordingError::NotFound)?;
        let object: String = sqlx::query_scalar(
            "SELECT object FROM revisions WHERE workspace=? AND occurrence=? AND number=?",
        )
        .bind(workspace)
        .bind(id)
        .bind(revision.number as i64)
        .fetch_one(&self.pool)
        .await?;
        let wav = std::fs::read(self.directory.join(object))?;
        if hex_sha(&wav) != revision.audio_sha256 {
            return Err(RecordingError::Conflict);
        }
        Ok((revision, wav))
    }

    pub async fn historical_revision(
        &self,
        workspace: &str,
        id: &str,
        number: u64,
    ) -> Result<Revision> {
        let _guard = self.gate.lock().await;
        self.occurrence_unlocked(workspace, id).await?;
        let json: Option<String> = sqlx::query_scalar(
            "SELECT revision_json FROM revisions WHERE workspace=? AND occurrence=? AND number=?",
        )
        .bind(workspace)
        .bind(id)
        .bind(number as i64)
        .fetch_optional(&self.pool)
        .await?;
        Ok(serde_json::from_str(
            &json.ok_or(RecordingError::NotFound)?,
        )?)
    }

    pub async fn historical_audio(
        &self,
        workspace: &str,
        id: &str,
        number: u64,
    ) -> Result<(Revision, Vec<u8>)> {
        let _guard = self.gate.lock().await;
        self.occurrence_unlocked(workspace, id).await?;
        let row=sqlx::query("SELECT revision_json,object FROM revisions WHERE workspace=? AND occurrence=? AND number=?").bind(workspace).bind(id).bind(number as i64).fetch_optional(&self.pool).await?.ok_or(RecordingError::NotFound)?;
        let revision: Revision = serde_json::from_str(&row.get::<String, _>("revision_json"))?;
        let wav = std::fs::read(self.directory.join(row.get::<String, _>("object")))?;
        if hex_sha(&wav) != revision.audio_sha256 {
            return Err(RecordingError::Conflict);
        }
        Ok((revision, wav))
    }

    pub async fn save_transcript(
        &self,
        workspace: &str,
        id: &str,
        expected_revision: &str,
        expected_audio_sha: &str,
        transcript: Transcript,
    ) -> Result<Revision> {
        identifier(&transcript.provider)?;
        identifier(&transcript.model)?;
        if serde_json::to_vec(&transcript.result)?.len() > 16 * 1024 * 1024 {
            return Err(RecordingError::Invalid("transcript limit exceeded".into()));
        }
        let _guard = self.gate.lock().await;
        let (_, revision, _, _) = self.occurrence_unlocked(workspace, id).await?;
        let mut revision = revision.ok_or(RecordingError::Conflict)?;
        if revision.revision != expected_revision || revision.audio_sha256 != expected_audio_sha {
            return Err(RecordingError::Conflict);
        }
        if let Some(existing) = &revision.transcript {
            if existing != &transcript {
                return Err(RecordingError::Conflict);
            }
            return Ok(revision);
        }
        revision.transcript = Some(transcript);
        revision.transcript_status = "ready".into();
        let json = serde_json::to_string(&revision)?;
        let mut transaction = self.pool.begin().await?;
        sqlx::query("UPDATE occurrences SET current_json=? WHERE workspace=? AND id=?")
            .bind(&json)
            .bind(workspace)
            .bind(id)
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            "UPDATE revisions SET revision_json=? WHERE workspace=? AND occurrence=? AND number=?",
        )
        .bind(json)
        .bind(workspace)
        .bind(id)
        .bind(revision.number as i64)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(revision)
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
        sqlx::query("UPDATE captures SET deleted=1 WHERE workspace=? AND occurrence=? AND id=?")
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
        sqlx::query("UPDATE captures SET deleted=1 WHERE workspace=? AND occurrence=?")
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
        let objects: Vec<String> =
            sqlx::query_scalar("SELECT object FROM revisions WHERE workspace=? AND occurrence=?")
                .bind(workspace)
                .bind(id)
                .fetch_all(&self.pool)
                .await?;
        Ok(objects)
    }
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
fn hex_sha(bytes: &[u8]) -> String {
    anlg_shared_recording::sha256(bytes)
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

#[derive(Clone)]
struct HttpState {
    service: Arc<SharedRecordingService>,
    authenticator: Arc<dyn WorkspaceAuthenticator>,
}

pub fn router(
    service: Arc<SharedRecordingService>,
    authenticator: Arc<dyn WorkspaceAuthenticator>,
) -> Router {
    Router::new()
        .route(
            "/v1/workspaces/{workspace}/shared-recordings/{id}",
            post(create).get(manifest).delete(delete_occurrence),
        )
        .route(
            "/v1/workspaces/{workspace}/shared-recordings/{id}/captures/{capture}",
            post(upload).delete(delete_capture),
        )
        .route(
            "/v1/workspaces/{workspace}/shared-recordings/{id}/finalize",
            post(finalize),
        )
        .route(
            "/v1/workspaces/{workspace}/shared-recordings/{id}/seal",
            post(seal),
        )
        .route(
            "/v1/workspaces/{workspace}/shared-recordings/{id}/audio",
            get(audio),
        )
        .route(
            "/v1/workspaces/{workspace}/shared-recordings/{id}/transcript",
            get(transcript),
        )
        .route(
            "/v1/workspaces/{workspace}/shared-recordings/{id}/revisions/{number}",
            get(historical_revision),
        )
        .route(
            "/v1/workspaces/{workspace}/shared-recordings/{id}/revisions/{number}/audio",
            get(historical_audio),
        )
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::from_fn(no_store))
        .with_state(HttpState {
            service,
            authenticator,
        })
}
pub(crate) fn wav_response(wav: Vec<u8>, audio_sha256: &str) -> Response {
    (
        [
            (header::CONTENT_TYPE, "audio/wav".to_owned()),
            (header::CACHE_CONTROL, "no-store".to_owned()),
            (header::ETAG, format!("\"{audio_sha256}\"")),
        ],
        wav,
    )
        .into_response()
}

async fn no_store(request: axum::extract::Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

struct HttpError(StatusCode, String);
impl From<RecordingError> for HttpError {
    fn from(error: RecordingError) -> Self {
        let status = match &error {
            RecordingError::Invalid(_) => StatusCode::BAD_REQUEST,
            RecordingError::Conflict => StatusCode::CONFLICT,
            RecordingError::NotFound => StatusCode::NOT_FOUND,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        if status == StatusCode::INTERNAL_SERVER_ERROR {
            tracing::error!(%error,"shared recording failed");
            Self(status, "recording storage unavailable".into())
        } else {
            Self(status, error.to_string())
        }
    }
}
impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        (
            self.0,
            [(header::CACHE_CONTROL, "no-store")],
            Json(serde_json::json!({"error":self.1})),
        )
            .into_response()
    }
}
fn authorize(
    state: &HttpState,
    headers: &HeaderMap,
    workspace: &str,
) -> std::result::Result<(), HttpError> {
    let auth = state
        .authenticator
        .authenticate(
            headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
        )
        .map_err(|_| {
            HttpError(
                StatusCode::UNAUTHORIZED,
                "workspace service credentials required".into(),
            )
        })?;
    if auth.workspace_id.as_ref() != workspace {
        return Err(HttpError(
            StatusCode::FORBIDDEN,
            "workspace forbidden".into(),
        ));
    }
    Ok(())
}
async fn create(
    State(state): State<HttpState>,
    Path((workspace, id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<CreateOccurrence>,
) -> std::result::Result<Json<OccurrenceManifest>, HttpError> {
    authorize(&state, &headers, &workspace)?;
    Ok(Json(
        state
            .service
            .create_occurrence(&workspace, &id, request)
            .await?,
    ))
}
async fn manifest(
    State(state): State<HttpState>,
    Path((workspace, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> std::result::Result<Json<OccurrenceManifest>, HttpError> {
    authorize(&state, &headers, &workspace)?;
    Ok(Json(state.service.manifest(&workspace, &id).await?))
}
async fn upload(
    State(state): State<HttpState>,
    Path((workspace, id, capture)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> std::result::Result<Json<OccurrenceManifest>, HttpError> {
    authorize(&state, &headers, &workspace)?;
    let request: CaptureUpload = serde_json::from_slice(&body)
        .map_err(|_| HttpError(StatusCode::BAD_REQUEST, "invalid upload metadata".into()))?;
    let wav = STANDARD
        .decode(&request.wav_base64)
        .map_err(|_| HttpError(StatusCode::BAD_REQUEST, "invalid base64 WAV".into()))?;
    Ok(Json(
        state
            .service
            .upload(&workspace, &id, &capture, request.metadata, &wav)
            .await?,
    ))
}
async fn finalize(
    State(state): State<HttpState>,
    Path((workspace, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> std::result::Result<Json<Revision>, HttpError> {
    authorize(&state, &headers, &workspace)?;
    Ok(Json(state.service.finalize(&workspace, &id).await?))
}

async fn seal(
    State(state): State<HttpState>,
    Path((workspace, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> std::result::Result<Json<Revision>, HttpError> {
    authorize(&state, &headers, &workspace)?;
    Ok(Json(state.service.seal(&workspace, &id).await?))
}
async fn audio(
    State(state): State<HttpState>,
    Path((workspace, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> std::result::Result<Response, HttpError> {
    authorize(&state, &headers, &workspace)?;
    let (revision, wav) = state.service.canonical_audio(&workspace, &id).await?;
    Ok(wav_response(wav, &revision.audio_sha256))
}
async fn transcript(
    State(state): State<HttpState>,
    Path((workspace, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> std::result::Result<Json<Revision>, HttpError> {
    authorize(&state, &headers, &workspace)?;
    let current = state
        .service
        .manifest(&workspace, &id)
        .await?
        .current
        .ok_or(RecordingError::NotFound)?;
    Ok(Json(current))
}
async fn delete_capture(
    State(state): State<HttpState>,
    Path((workspace, id, capture)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> std::result::Result<StatusCode, HttpError> {
    authorize(&state, &headers, &workspace)?;
    state
        .service
        .delete_capture(&workspace, &id, &capture)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn delete_occurrence(
    State(state): State<HttpState>,
    Path((workspace, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> std::result::Result<StatusCode, HttpError> {
    authorize(&state, &headers, &workspace)?;
    state.service.delete_occurrence(&workspace, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn historical_revision(
    State(state): State<HttpState>,
    Path((workspace, id, number)): Path<(String, String, u64)>,
    headers: HeaderMap,
) -> std::result::Result<Json<Revision>, HttpError> {
    authorize(&state, &headers, &workspace)?;
    Ok(Json(
        state
            .service
            .historical_revision(&workspace, &id, number)
            .await?,
    ))
}
async fn historical_audio(
    State(state): State<HttpState>,
    Path((workspace, id, number)): Path<(String, String, u64)>,
    headers: HeaderMap,
) -> std::result::Result<Response, HttpError> {
    authorize(&state, &headers, &workspace)?;
    let (revision, wav) = state
        .service
        .historical_audio(&workspace, &id, number)
        .await?;
    Ok(wav_response(wav, &revision.audio_sha256))
}
