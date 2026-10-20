use std::{collections::BTreeMap, env, net::SocketAddr, path::PathBuf, sync::Arc};

use anarlog_enterprise_control_plane::{
    auth::{StaticTokenAuthenticator, WorkspaceAuthenticator},
    recording_transcription::{self, RecordingProcessor, RecordingTranscriber},
    shared_recordings::{self, SharedRecordingService},
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let directory = PathBuf::from(env::var("ANARLOG_SHARED_RECORDINGS_DIR")?);
    anyhow::ensure!(
        directory.is_absolute(),
        "set an absolute, disposable shared recording directory"
    );
    let bind: SocketAddr = env::var("ANARLOG_SHARED_RECORDINGS_BIND")
        .unwrap_or_else(|_| "127.0.0.1:0".into())
        .parse()?;
    anyhow::ensure!(
        bind.ip().is_loopback(),
        "local preview binds only to loopback"
    );
    let tokens: BTreeMap<String, String> =
        serde_json::from_str(&env::var("ANARLOG_ENTERPRISE_WORKSPACE_TOKENS")?)?;
    anyhow::ensure!(
        tokens.values().all(|token| token.len() >= 32),
        "workspace service tokens require at least 32 characters"
    );
    let authenticator: Arc<dyn WorkspaceAuthenticator> =
        Arc::new(StaticTokenAuthenticator::new(tokens)?);
    let service = Arc::new(SharedRecordingService::open(directory).await?);
    let mut router = shared_recordings::router(service.clone(), authenticator.clone());
    if let Ok(api_base) = env::var("ANARLOG_SHARED_RECORDINGS_TRANSCRIPTION_URL") {
        let provider = env::var("ANARLOG_SHARED_RECORDINGS_TRANSCRIPTION_ADAPTER")?;
        let model = env::var("ANARLOG_SHARED_RECORDINGS_TRANSCRIPTION_MODEL")?;
        let api_key = env::var("ANARLOG_SHARED_RECORDINGS_TRANSCRIPTION_KEY").unwrap_or_default();
        let transcriber = RecordingTranscriber::new(&provider, api_base, api_key, model.clone())?;
        let processor = Arc::new(RecordingProcessor::new(
            service,
            transcriber,
            provider,
            model,
        ));
        router = router.merge(recording_transcription::router(processor, authenticator));
    }
    let listener = tokio::net::TcpListener::bind(bind).await?;
    println!("shared-recordings-local http://{}", listener.local_addr()?);
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
