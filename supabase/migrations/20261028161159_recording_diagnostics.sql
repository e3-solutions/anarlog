CREATE TABLE public.recording_diagnostics (
  user_id uuid NOT NULL REFERENCES auth.users(id) ON DELETE CASCADE,
  id uuid NOT NULL,
  device_id uuid NOT NULL,
  boot_id uuid NOT NULL,
  sequence bigint NOT NULL CHECK (sequence >= 0),
  observed_at timestamptz NOT NULL,
  elapsed_ms bigint NOT NULL CHECK (elapsed_ms >= 0),
  received_at timestamptz NOT NULL DEFAULT now(),
  app_version text NOT NULL,
  build_sha text NOT NULL,
  session_id uuid,
  kind text NOT NULL,
  reason text NOT NULL,
  evidence jsonb,
  PRIMARY KEY (user_id,id)
);
CREATE INDEX recording_diagnostics_device_observed ON public.recording_diagnostics(user_id,device_id,observed_at DESC);
CREATE INDEX recording_diagnostics_retention ON public.recording_diagnostics(observed_at);
ALTER TABLE public.recording_diagnostics ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON public.recording_diagnostics FROM PUBLIC,anon,authenticated;
GRANT SELECT ON public.recording_diagnostics TO authenticated;
GRANT ALL ON public.recording_diagnostics TO service_role;
CREATE POLICY recording_diagnostics_read ON public.recording_diagnostics FOR SELECT TO authenticated
USING (observed_at >= now() - interval '7 days' AND (
  (user_id = (SELECT auth.uid()) AND (SELECT private.application_health_session_active()))
  OR private.application_health_can_read(user_id)
));

-- The only client write path derives ownership from a current confirmed session.
-- Definer privileges permit validation and bounded inserts without table DML grants.
CREATE FUNCTION public.recording_diagnostics_ingest(p_events jsonb)
RETURNS jsonb LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
DECLARE
  actor uuid := auth.uid();
  event jsonb;
  field record;
  evidence_value jsonb;
  acknowledgements jsonb := '[]'::jsonb;
  event_id uuid;
  device uuid;
  boot uuid;
  session uuid;
  observed timestamptz;
BEGIN
  IF actor IS NULL OR NOT private.application_health_session_active() OR NOT EXISTS (
    SELECT 1 FROM public.workspace_memberships m JOIN public.workspaces w ON w.id = m.workspace_id
    WHERE m.user_id = actor AND m.deleted_at IS NULL AND w.deleted_at IS NULL AND w.kind = 'shared'
  ) THEN RAISE EXCEPTION 'active organization session required' USING ERRCODE = '42501'; END IF;
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
      OR event->>'kind' NOT IN ('detector_observation','policy_decision','stop_requested','capture_started','capture_finalizing','capture_ended','capture_stopped','recovery_interrupted','capture_error')
      OR event->>'reason' NOT IN ('observed','meeting_active','meeting_ended','ax_unknown','ax_error','ax_probe_timeout','mic_still_active','mic_inactive','network_recovery_hold','user_kept_recording','auto_stop_disabled','auto_start_disabled','identity_unavailable','owner_mismatch','account_changed','manual_stop','sleep','silence_timeout','grace_pending','grace_elapsed','native_lifecycle','native_capture_ended','process_interrupted','probe_error','probe_timeout','probe_started','stop_failed','saved_audio_ready','saving_failed','collector_started','audio_capture_error','transcription_error','sharing_lookup_pending','sharing_lookup_failed','delivery_failed','recording_excluded') THEN
      RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023';
    END IF;
    event_id := (event->>'id')::uuid;
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
        IF field.key IN ('micActive','autoStop','autoStart','identityReady','online','owned','speakerRecent','retentionDropped','leaveControlPresent','leaveControlEnabled','leaveControlVisible') THEN
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
        ELSIF field.key = 'os' THEN
          IF jsonb_typeof(field.value) <> 'string' OR field.value #>> '{}' NOT IN ('macos','linux','windows','unknown') THEN RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023'; END IF;
        ELSIF field.key = 'observation' THEN
          IF jsonb_typeof(field.value) <> 'string' OR field.value #>> '{}' NOT IN ('active','inactive','unknown') THEN RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023'; END IF;
        ELSIF field.key IN ('inspectionStatus','micStatus') THEN
          IF jsonb_typeof(field.value) <> 'string' OR field.value #>> '{}' NOT IN ('ok','error','timeout','pending','unavailable') THEN RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023'; END IF;
        ELSE RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023';
        END IF;
      END LOOP;
    END IF;
    INSERT INTO public.recording_diagnostics(user_id,id,device_id,boot_id,sequence,observed_at,elapsed_ms,app_version,build_sha,session_id,kind,reason,evidence)
    VALUES(actor,event_id,device,boot,(event->>'sequence')::bigint,observed,(event->>'elapsedMs')::bigint,event->>'appVersion',event->>'buildSha',session,event->>'kind',event->>'reason',NULLIF(evidence_value,'null'::jsonb))
    ON CONFLICT (user_id,id) DO NOTHING;
    acknowledgements := acknowledgements || jsonb_build_array(event_id);
  END LOOP;
  DELETE FROM public.recording_diagnostics WHERE user_id = actor AND observed_at < now() - interval '7 days';
  IF (SELECT count(DISTINCT device_id) FROM public.recording_diagnostics WHERE user_id = actor) > 8 THEN
    RAISE EXCEPTION 'recording diagnostics device limit reached' USING ERRCODE = '22023';
  END IF;
  DELETE FROM public.recording_diagnostics WHERE (user_id,id) IN (
    SELECT user_id,id FROM (
      SELECT d.user_id,d.id,row_number() OVER (PARTITION BY d.device_id ORDER BY d.observed_at DESC,d.received_at DESC,d.id) AS position
      FROM public.recording_diagnostics d WHERE d.user_id = actor
    ) ranked WHERE position > 4096
  );
  DELETE FROM public.recording_diagnostics WHERE (user_id,id) IN (
    SELECT d.user_id,d.id FROM public.recording_diagnostics d WHERE d.user_id = actor
    ORDER BY d.observed_at DESC,d.received_at DESC,d.id OFFSET 16384
  );
  RETURN acknowledgements;
EXCEPTION WHEN invalid_text_representation OR invalid_datetime_format OR datetime_field_overflow OR numeric_value_out_of_range THEN
  RAISE EXCEPTION 'invalid recording diagnostics' USING ERRCODE = '22023';
END; $$;
REVOKE ALL ON FUNCTION public.recording_diagnostics_ingest(jsonb) FROM PUBLIC,anon;
GRANT EXECUTE ON FUNCTION public.recording_diagnostics_ingest(jsonb) TO authenticated;

-- Expire dormant devices too; ingestion cleanup alone would retain their data.
CREATE EXTENSION IF NOT EXISTS pg_cron WITH SCHEMA pg_catalog;
GRANT USAGE ON SCHEMA cron TO postgres;
GRANT ALL PRIVILEGES ON ALL TABLES IN SCHEMA cron TO postgres;
SELECT cron.schedule('recording-diagnostics-retention','17 * * * *', $job$
  DELETE FROM public.recording_diagnostics WHERE observed_at < now() - interval '7 days';
  DELETE FROM cron.job_run_details WHERE jobid IN (
    SELECT jobid FROM cron.job WHERE jobname = 'recording-diagnostics-retention'
  ) AND start_time < now() - interval '7 days';
$job$);
