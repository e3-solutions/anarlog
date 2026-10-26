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
    let bind = preview_bind(
        env::var("ANARLOG_SHARED_RECORDINGS_BIND").ok().as_deref(),
        env::var("ANARLOG_SHARED_RECORDINGS_ALLOW_PUBLIC_BIND").as_deref() == Ok("true"),
    )?;
    let web_origin = env::var("ANARLOG_SHARED_RECORDINGS_WEB_ORIGIN").ok();
    let cors = recording_app::local_preview_cors(web_origin.as_deref())?;
    let supabase_url = env::var("SUPABASE_URL")?;
    let anon_key = env::var("SUPABASE_ANON_KEY")?;
    let members = RecordingMemberAuth::new(&supabase_url, &anon_key)?;
    let mut provider_routes = axum::Router::new();
    let identities = if let Ok(api_key) = env::var("NANGO_API_KEY") {
        let config = anlg_api_nango::NangoConfig {
            nango: serde_json::from_value(serde_json::json!({
                "nango_api_key": api_key,
                "nango_api_base": env::var("NANGO_API_BASE").ok(),
                "nango_webhook_signing_key": env::var("NANGO_WEBHOOK_SIGNING_KEY")?,
            }))?,
            supabase_url: supabase_url.clone(),
            supabase_anon_key: anon_key.clone(),
            supabase_service_role_key: Some(env::var("SUPABASE_SERVICE_ROLE_KEY")?),
        };
        provider_routes = recording_app::provider_router(config.clone());
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
        let model = env::var("ANARLOG_SHARED_RECORDINGS_TRANSCRIPTION_MODEL")?;
        let api_key = env::var("ANARLOG_SHARED_RECORDINGS_TRANSCRIPTION_KEY").unwrap_or_default();
        let transcriber = RecordingTranscriber::new(api_base, api_key, model)?;
        Some(Arc::new(RecordingProcessor::new(
            service.clone(),
            transcriber,
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
    axum::serve(
        listener,
        recording_app::router(state, provider_routes, cors),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    Ok(())
}

fn preview_bind(address: Option<&str>, allow_public: bool) -> anyhow::Result<SocketAddr> {
    let bind: SocketAddr = address.unwrap_or("127.0.0.1:8789").parse()?;
    anyhow::ensure!(
        bind.ip().is_loopback() || allow_public,
        "public binding requires ANARLOG_SHARED_RECORDINGS_ALLOW_PUBLIC_BIND=true"
    );
    Ok(bind)
}

#[cfg(test)]
mod tests {
    use super::preview_bind;

    #[test]
    fn public_binding_requires_explicit_opt_in_for_ipv4_and_ipv6() {
        assert!(preview_bind(None, false).unwrap().ip().is_loopback());
        for address in ["127.0.0.1:8789", "[::1]:8789"] {
            assert!(preview_bind(Some(address), false).is_ok());
        }
        for address in [
            "0.0.0.0:8080",
            "[::]:8080",
            "192.0.2.1:8080",
            "[2001:db8::1]:8080",
        ] {
            assert!(preview_bind(Some(address), false).is_err());
            assert_eq!(
                preview_bind(Some(address), true).unwrap().to_string(),
                address
            );
        }
    }
}
