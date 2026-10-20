use std::{env, net::SocketAddr, path::PathBuf, sync::Arc};

use anarlog_enterprise_control_plane::{
    recording_app::{self, RecordingAppState},
    recording_identity::RecordingIdentityResolver,
    recording_members::RecordingMemberAuth,
    recording_transcription::{RecordingProcessor, RecordingTranscriber},
    shared_recordings::SharedRecordingService,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let directory = PathBuf::from(env::var("ANARLOG_SHARED_RECORDINGS_DIR")?);
    anyhow::ensure!(
        directory.is_absolute(),
        "set an absolute isolated recording directory"
    );
    let bind: SocketAddr = env::var("ANARLOG_SHARED_RECORDINGS_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8789".into())
        .parse()?;
    anyhow::ensure!(
        bind.ip().is_loopback(),
        "local app preview binds only to loopback"
    );
    let supabase_url = env::var("SUPABASE_URL")?;
    let anon_key = env::var("SUPABASE_ANON_KEY")?;
    let members = RecordingMemberAuth::new(&supabase_url, &anon_key)?;
    let identities = if let Ok(api_key) = env::var("NANGO_API_KEY") {
        let mut builder = anlg_nango::NangoClient::builder().api_key(api_key);
        if let Ok(base) = env::var("NANGO_API_BASE") {
            builder = builder.api_base(base);
        }
        let connection =
            anlg_api_nango::NangoConnectionState::new(builder.build()?, supabase_url, anon_key);
        Some(RecordingIdentityResolver::new(connection))
    } else {
        tracing::warn!("NANGO_API_KEY missing: provider admission stays unresolved");
        None
    };
    let service = Arc::new(SharedRecordingService::open(directory).await?);
    let processor = if let Ok(api_base) = env::var("ANARLOG_SHARED_RECORDINGS_TRANSCRIPTION_URL") {
        let provider = env::var("ANARLOG_SHARED_RECORDINGS_TRANSCRIPTION_ADAPTER")?;
        let model = env::var("ANARLOG_SHARED_RECORDINGS_TRANSCRIPTION_MODEL")?;
        let api_key = env::var("ANARLOG_SHARED_RECORDINGS_TRANSCRIPTION_KEY").unwrap_or_default();
        let transcriber = RecordingTranscriber::new(&provider, api_base, api_key, model.clone())?;
        Some(Arc::new(RecordingProcessor::new(
            service.clone(),
            transcriber,
            provider,
            model,
        )))
    } else {
        tracing::warn!("transcription endpoint missing: canonical recordings remain pending");
        None
    };
    let state = RecordingAppState {
        service,
        members,
        identities,
        processor,
    };
    let listener = tokio::net::TcpListener::bind(bind).await?;
    println!("shared-recordings-app http://{}", listener.local_addr()?);
    axum::serve(listener, recording_app::router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
