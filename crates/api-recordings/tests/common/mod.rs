use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use api_recordings::shared_recordings::SharedRecordingService;
use serde_json::{Value, json};
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path_regex};

pub async fn service(workspace: &'static str) -> (MockServer, SharedRecordingService) {
    #[derive(Default)]
    struct State {
        captures: Vec<Value>,
        occurrences: HashMap<String, Value>,
        admissions: Vec<(String, String)>,
        objects: HashMap<String, Vec<u8>>,
    }
    let server = MockServer::start().await;
    let state = Arc::new(Mutex::new(State::default()));
    Mock::given(path_regex("^/(rest/v1/(recording_|rpc/recording_)|storage/v1/object/)"))
        .respond_with(move |request: &wiremock::Request| {
            if request.headers.get("authorization").and_then(|value| value.to_str().ok()) != Some("Bearer fixture-service-role") {
                return ResponseTemplate::new(401);
            }
            if request.method.as_str() == "GET" && request.url.path().starts_with("/rest/v1/")
                && !request.url.query_pairs().any(|(key,value)| key == "workspace" && value == format!("eq.{workspace}")) {
                return ResponseTemplate::new(400);
            }
            let mut state = state.lock().unwrap();
            let path = request.url.path();
            match (request.method.as_str(), path) {
                ("GET", "/rest/v1/recording_occurrences") => {
                    let id = request.url.query_pairs().find(|(k,_)| k == "id").map(|(_,v)| v.trim_start_matches("eq.").to_string());
                    ResponseTemplate::new(200).set_body_json(state.occurrences.iter().filter(|(key,_)| id.as_ref().is_none_or(|id| id == *key)).map(|(_,v)| v).collect::<Vec<_>>())
                },
                ("GET", "/rest/v1/recording_captures") => {
                    let id = request.url.query_pairs().find(|(k,_)| k == "occurrence").map(|(_,v)| v.trim_start_matches("eq.").to_string());
                    ResponseTemplate::new(200).set_body_json(state.captures.iter().filter(|v| id.as_deref().is_none_or(|id| v["occurrence"] == id)).collect::<Vec<_>>())
                },
                ("GET", "/rest/v1/recording_admissions") => {
                    let query = request.url.query_pairs().map(|(k,v)| (k.to_string(),v.trim_start_matches("eq.").to_string())).collect::<HashMap<_,_>>();
                    let rows=state.admissions.iter().filter(|(occurrence,actor)| query.get("occurrence") == Some(occurrence) && query.get("actor") == Some(actor)).map(|(_,actor)| json!({"actor":actor})).collect::<Vec<_>>();
                    ResponseTemplate::new(200).set_body_json(rows)
                },
                ("POST", "/rest/v1/rpc/recording_admit") => {
                    let body: Value = serde_json::from_slice(&request.body).unwrap();
                    let id=body["p_id"].as_str().unwrap().to_string();
                    let started=body["p_provider_started_at_ms"].as_i64().unwrap_or(0);
                    state.occurrences.entry(id.clone()).or_insert_with(|| json!({"id":id,"metadata":body["p_metadata"],"sealed":false,"automatic":true,"provider_started_at_ms":started}));
                    let entry=(id,body["p_actor"].as_str().unwrap().to_string());
                    if !state.admissions.contains(&entry) {state.admissions.push(entry);}
                    ResponseTemplate::new(200).set_body_json(started)
                },
                ("POST", "/rest/v1/rpc/recording_upload") => {
                    let body: Value = serde_json::from_slice(&request.body).unwrap();
                    let row = json!({"id":body["p_id"],"occurrence":body["p_occurrence"],"metadata":body["p_metadata"],"object":body["p_object"],"frames":body["p_frames"],"deleted":false,"transcript_json":null});
                    state.captures.retain(|capture| capture["id"] != row["id"] || capture["occurrence"] != row["occurrence"]);
                    state.captures.push(row);
                    state.captures.sort_by(|a,b| a["id"].as_str().cmp(&b["id"].as_str()));
                    ResponseTemplate::new(200)
                },
                ("POST", "/rest/v1/rpc/recording_save_transcript") => {
                    let body: Value = serde_json::from_slice(&request.body).unwrap();
                    let row = state.captures.iter_mut().find(|capture| capture["id"] == body["p_id"]).unwrap();
                    row["transcript_json"] = body["p_transcript"].clone();
                    ResponseTemplate::new(200)
                },
                ("POST", "/rest/v1/rpc/recording_seal") => { for row in state.occurrences.values_mut() {row["sealed"]=json!(true);} ResponseTemplate::new(200) },
                ("POST", "/rest/v1/rpc/recording_delete") => ResponseTemplate::new(200).set_body_json(json!([])),
                ("POST", _) if path.starts_with("/storage/v1/object/recording-audio/") => {
                    state.objects.insert(path.trim_start_matches("/storage/v1/object/recording-audio/").into(),request.body.clone());
                    ResponseTemplate::new(200).set_body_json(json!({}))
                },
                ("GET", _) if path.starts_with("/storage/v1/object/authenticated/recording-audio/") => {
                    match state.objects.get(path.trim_start_matches("/storage/v1/object/authenticated/recording-audio/")) {
                        Some(bytes) => ResponseTemplate::new(200).set_body_bytes(bytes.clone()),
                        None => ResponseTemplate::new(404),
                    }
                },
                ("DELETE", "/storage/v1/object/recording-audio") => ResponseTemplate::new(200).set_body_json(json!([])),
                _ => ResponseTemplate::new(404),
            }
        })
        .with_priority(5)
        .mount(&server)
        .await;
    let service = SharedRecordingService::supabase(&server.uri(), "fixture-service-role").unwrap();
    (server, service)
}
