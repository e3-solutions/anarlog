-- Sanitized upload state and bounded audio-quality evidence share existing consent,
-- authentication, acknowledgement and retention rules. No raw errors or audio content.
CREATE OR REPLACE FUNCTION public.recording_diagnostics_ingest(p_events jsonb)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
DECLARE
  actor uuid := auth.uid();
  event jsonb;
  field record;
  evidence_value jsonb;
  acknowledgements jsonb := '[]'::jsonb;
  rejections jsonb := '[]'::jsonb;
  event_id uuid;
  device uuid;
  boot uuid;
  session uuid;
  observed timestamptz;
BEGIN
  IF actor IS NULL OR NOT private.application_health_session_active() THEN
    RAISE EXCEPTION 'active organization session required' USING ERRCODE = '42501';
  END IF;
  IF jsonb_typeof(p_events) IS DISTINCT FROM 'array' THEN
    RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023';
  END IF;
  IF jsonb_array_length(p_events) NOT BETWEEN 1 AND 64 OR octet_length(p_events::text) > 131072 THEN
    RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023';
  END IF;
  PERFORM pg_advisory_xact_lock(hashtextextended('recording_diagnostics:' || actor::text,0));
  FOR event IN SELECT value FROM jsonb_array_elements(p_events) LOOP
    IF jsonb_typeof(event) IS DISTINCT FROM 'object' THEN
      RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023';
    END IF;
    IF jsonb_typeof(event->'id') IS DISTINCT FROM 'string' THEN
      RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023';
    END IF;
    event_id := (event->>'id')::uuid;
    BEGIN
      IF (SELECT count(*) FROM jsonb_object_keys(event)) <> 12
        OR NOT event ?& ARRAY['id','deviceId','bootId','sequence','observedAtMs','elapsedMs','appVersion','buildSha','sessionId','kind','reason','evidence']
        OR octet_length(event::text) > 2048 THEN
        RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023';
      END IF;
      -- All cast inputs are checked before use; malformed values yield one bounded error.
      FOR field IN SELECT key,value FROM jsonb_each(event) LOOP
        IF field.key IN ('id','deviceId','bootId','appVersion','buildSha','kind','reason') AND jsonb_typeof(field.value) <> 'string' THEN
          RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023';
        END IF;
        IF field.key IN ('sequence','observedAtMs','elapsedMs') THEN
          IF jsonb_typeof(field.value) <> 'number' THEN
            RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023';
          END IF;
          IF (field.value #>> '{}')::numeric NOT BETWEEN 0 AND 9223372036854775807
            OR (field.value #>> '{}')::numeric <> trunc((field.value #>> '{}')::numeric) THEN
            RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023';
          END IF;
        END IF;
      END LOOP;
      IF jsonb_typeof(event->'sessionId') NOT IN ('string','null')
        OR event->>'appVersion' !~ '^[0-9A-Za-z][0-9A-Za-z.+_-]{0,63}$'
        OR event->>'buildSha' !~ '^([0-9a-fA-F]{7,64}|unknown)?$'
        OR event->>'kind' NOT IN ('detector_observation','policy_decision','stop_requested','capture_started','capture_finalizing','capture_source_ended','capture_stopped','recovery_interrupted','capture_error','upload_state')
        OR event->>'reason' NOT IN ('observed','meeting_active','meeting_ended','ax_unknown','ax_error','ax_probe_timeout','mic_still_active','mic_inactive','network_recovery_hold','user_kept_recording','auto_stop_disabled','auto_start_disabled','identity_unavailable','owner_mismatch','account_changed','manual_stop','sleep','silence_timeout','grace_pending','grace_elapsed','native_lifecycle','native_capture_ended','process_interrupted','probe_error','probe_timeout','probe_started','stop_failed','stop_prompt_pending','stop_prompt_disabled','saved_audio_ready','saving_failed','collector_started','collector_attached_late','audio_capture_error','transcription_error','sharing_lookup_pending','sharing_lookup_failed','delivery_failed','recording_excluded','start_requested','start_failed','frontend_start_requested','frontend_start_failed') THEN
        RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023';
      END IF;
      device := (event->>'deviceId')::uuid;
      boot := (event->>'bootId')::uuid;
      session := (event->>'sessionId')::uuid;
      observed := to_timestamp((event->>'observedAtMs')::numeric / 1000);
      IF observed < now() - interval '7 days' OR observed > now() + interval '5 minutes' THEN
        RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023';
      END IF;
      evidence_value := event->'evidence';
      IF jsonb_typeof(evidence_value) NOT IN ('object','null') THEN
        RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023';
      END IF;
      IF jsonb_typeof(evidence_value) = 'object' THEN
        FOR field IN SELECT key,value FROM jsonb_each(evidence_value) LOOP
          IF field.key IN ('micActive','autoStop','autoStart','identityReady','online','owned','speakerRecent','retentionDropped','leaveControlPresent','leaveControlEnabled','leaveControlVisible','sourceActorPresent') THEN
            IF jsonb_typeof(field.value) <> 'boolean' THEN RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023'; END IF;
          ELSIF field.key IN ('missingForMs','networkHoldRemainingMs','probeElapsedMs','lastAckAgeMs') THEN
            IF jsonb_typeof(field.value) <> 'number' THEN RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023'; END IF;
            IF (field.value #>> '{}')::numeric NOT BETWEEN 0 AND 86400000
              OR (field.value #>> '{}')::numeric <> trunc((field.value #>> '{}')::numeric) THEN RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023'; END IF;
          ELSIF field.key IN ('queuedCount','droppedCount') THEN
            IF jsonb_typeof(field.value) <> 'number' THEN RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023'; END IF;
            IF (field.value #>> '{}')::numeric NOT BETWEEN 0 AND 1000000000
              OR (field.value #>> '{}')::numeric <> trunc((field.value #>> '{}')::numeric) THEN RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023'; END IF;
          ELSIF field.key = 'matchedBy' THEN
            IF jsonb_typeof(field.value) <> 'string' OR field.value #>> '{}' NOT IN ('context','toolbar','none') THEN RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023'; END IF;
          ELSIF field.key = 'platform' THEN
            IF jsonb_typeof(field.value) <> 'string' OR field.value #>> '{}' NOT IN ('slack','googleMeet','unknown') THEN RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023'; END IF;
          ELSIF field.key = 'osVersion' THEN
            IF jsonb_typeof(field.value) <> 'string' OR field.value #>> '{}' !~ '^[0-9.]{1,32}$' THEN RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023'; END IF;
          ELSIF field.key = 'os' THEN
            IF jsonb_typeof(field.value) <> 'string' OR field.value #>> '{}' NOT IN ('macos','linux','windows','unknown') THEN RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023'; END IF;
          ELSIF field.key = 'observation' THEN
            IF jsonb_typeof(field.value) <> 'string' OR field.value #>> '{}' NOT IN ('active','inactive','unknown') THEN RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023'; END IF;
          ELSIF field.key IN ('inspectionStatus','micStatus') THEN
            IF jsonb_typeof(field.value) <> 'string' OR field.value #>> '{}' NOT IN ('ok','error','timeout','pending','unavailable') THEN RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023'; END IF;
          ELSIF field.key = 'jobId' THEN
            IF jsonb_typeof(field.value) <> 'string' OR field.value #>> '{}' !~ '^[0-9a-fA-F]{64}$' THEN RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023'; END IF;
          ELSIF field.key = 'uploadState' THEN
            IF jsonb_typeof(field.value) <> 'string' OR field.value #>> '{}' NOT IN ('preparing','awaiting_identity','pending','blocked_storage','blocked_identity','blocked_timeline','error','uploaded') THEN RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023'; END IF;
          ELSIF field.key = 'blockReason' THEN
            IF jsonb_typeof(field.value) <> 'string' OR field.value #>> '{}' NOT IN ('identity_unverified','storage_failed','timeline_mismatch','account_changed','recording_excluded','upload_failed','normalize_failed') THEN RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023'; END IF;
          ELSIF field.key = 'identityReason' THEN
            IF jsonb_typeof(field.value) <> 'string' OR field.value #>> '{}' NOT IN ('ax_unavailable','ax_ambiguous','meeting_unverified','inventory_missing','inventory_conflict','meeting_departed','auth_unavailable','observer_stopped','call_changed','occurrence_changed','configuration_changed','unverified','user_excluded') THEN RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023'; END IF;
          ELSIF field.key IN ('capturedFrames','decodedFrames') THEN
            IF jsonb_typeof(field.value) <> 'number' THEN RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023'; END IF;
            IF (field.value #>> '{}')::numeric NOT BETWEEN 0 AND 1382400000 OR (field.value #>> '{}')::numeric <> trunc((field.value #>> '{}')::numeric) THEN RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023'; END IF;
          ELSIF field.key IN ('recordingStartedAtMs','recordingStoppedAtMs') THEN
            IF jsonb_typeof(field.value) <> 'number' THEN RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023'; END IF;
            IF (field.value #>> '{}')::numeric NOT BETWEEN 0 AND 9007199254740991 OR (field.value #>> '{}')::numeric <> trunc((field.value #>> '{}')::numeric) THEN RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023'; END IF;
          ELSIF field.key = 'audioTimelineVerified' THEN
            IF jsonb_typeof(field.value) <> 'boolean' THEN RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023'; END IF;
          ELSIF field.key = 'httpStatus' THEN
            IF jsonb_typeof(field.value) <> 'number' THEN RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023'; END IF;
            IF (field.value #>> '{}')::numeric NOT BETWEEN 100 AND 599 OR (field.value #>> '{}')::numeric <> trunc((field.value #>> '{}')::numeric) THEN RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023'; END IF;
          ELSIF field.key = 'uploadStage' THEN
            IF jsonb_typeof(field.value) <> 'string' OR field.value #>> '{}' NOT IN ('admission','normalize','audio_upload','processing','complete') THEN RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023'; END IF;
          ELSIF field.key = 'segmentCount' THEN
            IF jsonb_typeof(field.value) <> 'number' THEN RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023'; END IF;
            IF (field.value #>> '{}')::numeric NOT BETWEEN 0 AND 4096 OR (field.value #>> '{}')::numeric <> trunc((field.value #>> '{}')::numeric) THEN RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023'; END IF;
          ELSE RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023';
          END IF;
        END LOOP;
      END IF;
      IF event->>'kind' = 'upload_state' AND (session IS NULL OR jsonb_typeof(evidence_value) IS DISTINCT FROM 'object' OR NOT evidence_value ?& ARRAY['jobId','uploadState']) THEN
        RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023';
      END IF;
      INSERT INTO public.recording_diagnostics(user_id,id,device_id,boot_id,sequence,observed_at,elapsed_ms,app_version,build_sha,session_id,kind,reason,evidence)
      VALUES(actor,event_id,device,boot,(event->>'sequence')::bigint,observed,(event->>'elapsedMs')::bigint,event->>'appVersion',event->>'buildSha',session,event->>'kind',event->>'reason',NULLIF(evidence_value,'null'::jsonb))
      ON CONFLICT (user_id,id) DO NOTHING;
      acknowledgements := acknowledgements || jsonb_build_array(event_id);
    EXCEPTION WHEN data_exception THEN
      rejections := rejections || jsonb_build_array(event_id);
    END;
  END LOOP;
  DELETE FROM public.recording_diagnostics WHERE user_id = actor AND observed_at < now() - interval '7 days';
  DELETE FROM public.recording_diagnostics WHERE (user_id,id) IN (
    SELECT user_id,id FROM (
      SELECT d.user_id,d.id,row_number() OVER (PARTITION BY d.device_id ORDER BY CASE WHEN (d.kind = 'detector_observation' AND d.reason IN ('observed','probe_started','native_lifecycle')) OR (d.kind = 'policy_decision' AND d.reason = 'collector_started') THEN 1 ELSE 0 END, d.observed_at DESC,d.received_at DESC,d.id) AS position
      FROM public.recording_diagnostics d WHERE d.user_id = actor
    ) ranked WHERE position > 4096
  );
  DELETE FROM public.recording_diagnostics WHERE (user_id,id) IN (
    SELECT d.user_id,d.id FROM public.recording_diagnostics d WHERE d.user_id = actor
    ORDER BY CASE WHEN (d.kind = 'detector_observation' AND d.reason IN ('observed','probe_started','native_lifecycle')) OR (d.kind = 'policy_decision' AND d.reason = 'collector_started') THEN 1 ELSE 0 END, d.observed_at DESC,d.received_at DESC,d.id OFFSET 16384
  );
  RETURN jsonb_build_object('accepted',acknowledgements,'rejected',rejections);
EXCEPTION WHEN invalid_text_representation OR invalid_datetime_format OR datetime_field_overflow OR numeric_value_out_of_range THEN
  RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023';
END; $$;
