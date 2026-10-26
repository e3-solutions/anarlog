use std::{
    collections::HashSet,
    path::{Path as FsPath, PathBuf},
};

use anlg_shared_recording::{
    FrameRange, MAX_FRAMES, MAX_SOURCES, Manifest, align_clock_hints, decode_source, render,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{
    Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use tokio::sync::Mutex;

const MAX_AUDIO_BYTES: usize = 100 * 1024 * 1024;

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
