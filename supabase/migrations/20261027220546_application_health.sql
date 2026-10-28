CREATE SCHEMA IF NOT EXISTS private;
REVOKE ALL ON SCHEMA private FROM PUBLIC;
GRANT USAGE ON SCHEMA private TO authenticated;

CREATE FUNCTION private.application_health_session_active() RETURNS boolean
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT EXISTS (SELECT 1 FROM auth.sessions s JOIN auth.users u ON u.id = s.user_id
    WHERE s.id = NULLIF(auth.jwt()->>'session_id','')::uuid AND s.user_id = auth.uid()
      AND (s.not_after IS NULL OR s.not_after > now())
      AND u.email_confirmed_at IS NOT NULL AND NOT COALESCE(u.is_anonymous,false));
$$;
REVOKE ALL ON FUNCTION private.application_health_session_active() FROM PUBLIC, anon;
GRANT EXECUTE ON FUNCTION private.application_health_session_active() TO authenticated;

CREATE TABLE public.application_health_devices (
  user_id uuid NOT NULL REFERENCES auth.users(id) ON DELETE CASCADE,
  device_id uuid NOT NULL,
  first_seen_at timestamptz NOT NULL DEFAULT now(),
  last_seen_at timestamptz NOT NULL DEFAULT now(),
  observed_at timestamptz NOT NULL,
  snapshot jsonb NOT NULL CHECK (octet_length(snapshot::text) <= 4096),
  PRIMARY KEY (user_id, device_id)
);
CREATE INDEX application_health_devices_last_seen ON public.application_health_devices(last_seen_at);
ALTER TABLE public.application_health_devices ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON public.application_health_devices FROM PUBLIC, anon, authenticated;
GRANT SELECT ON public.application_health_devices TO authenticated;
GRANT ALL ON public.application_health_devices TO service_role;
CREATE FUNCTION private.application_health_can_read(p_subject uuid) RETURNS boolean
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT private.application_health_session_active() AND EXISTS (
    SELECT 1 FROM public.workspace_memberships actor
    JOIN public.workspace_memberships subject ON subject.workspace_id = actor.workspace_id
    JOIN public.workspaces workspace ON workspace.id = actor.workspace_id
    WHERE actor.user_id = auth.uid() AND actor.role IN ('owner','admin')
      AND actor.deleted_at IS NULL AND subject.deleted_at IS NULL
      AND subject.user_id = p_subject AND workspace.deleted_at IS NULL AND workspace.kind = 'shared'
  );
$$;
REVOKE ALL ON FUNCTION private.application_health_can_read(uuid) FROM PUBLIC, anon;
GRANT EXECUTE ON FUNCTION private.application_health_can_read(uuid) TO authenticated;
CREATE POLICY application_health_manager_read ON public.application_health_devices
FOR SELECT TO authenticated USING (last_seen_at >= now() - interval '90 days' AND private.application_health_can_read(user_id));

-- The bearer session, not client-supplied identity, owns every snapshot. The
-- definer is needed to check auth.sessions and write a table with no client DML.
CREATE FUNCTION public.application_health_observe(p_snapshot jsonb)
RETURNS void LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
DECLARE
  stamped jsonb;
  actor uuid := auth.uid();
  device uuid;
  observed timestamptz;
BEGIN
  IF actor IS NULL OR NOT EXISTS (
    SELECT 1 FROM auth.sessions s JOIN auth.users u ON u.id = s.user_id
    WHERE s.id = NULLIF(auth.jwt()->>'session_id', '')::uuid AND s.user_id = actor
      AND u.email_confirmed_at IS NOT NULL AND NOT COALESCE(u.is_anonymous, false)
      AND (s.not_after IS NULL OR s.not_after > now())
  ) OR NOT EXISTS (
    SELECT 1 FROM public.workspace_memberships m JOIN public.workspaces w ON w.id = m.workspace_id
    WHERE m.user_id = actor AND m.deleted_at IS NULL AND w.deleted_at IS NULL AND w.kind = 'shared'
  ) THEN RAISE EXCEPTION 'active organization session required' USING ERRCODE = '42501'; END IF;
  device := (p_snapshot->>'deviceId')::uuid;
  observed := (p_snapshot->>'observedAt')::timestamptz;
  IF device IS NULL OR observed IS NULL OR observed > now() + interval '5 minutes'
    OR observed < now() - interval '24 hours' OR octet_length(p_snapshot::text) > 4096
    OR (SELECT count(*) FROM jsonb_object_keys(p_snapshot)) <> 14
    OR NOT p_snapshot ?& ARRAY['deviceId','appVersion','observedAt','permissions','autoRecordDetected','autoRecordScheduled','recording','lastRecordingStartedAt','lastRecordingStoppedAt','lastRecordingDurationSeconds','errorCategory','uploadPendingCount','uploadBlockedCount','uploadError']
    OR jsonb_typeof(p_snapshot->'appVersion') IS DISTINCT FROM 'string'
    OR (p_snapshot->>'appVersion') !~ '^[0-9A-Za-z][0-9A-Za-z.+_-]{0,63}$'
    OR jsonb_typeof(p_snapshot->'permissions') IS DISTINCT FROM 'object'
    OR (SELECT count(*) FROM jsonb_object_keys(p_snapshot->'permissions')) <> 3
    OR NOT (p_snapshot->'permissions') ?& ARRAY['microphone','systemAudio','accessibility']
    OR EXISTS (SELECT 1 FROM jsonb_each_text(p_snapshot->'permissions') p WHERE p.value NOT IN ('authorized','denied','neverRequested','unknown') OR p.value IS NULL)
    OR EXISTS (SELECT 1 FROM jsonb_each(p_snapshot) p WHERE p.key IN ('autoRecordDetected','autoRecordScheduled') AND jsonb_typeof(p.value) <> 'boolean')
    OR jsonb_typeof(p_snapshot->'recording') NOT IN ('boolean','null')
    OR jsonb_typeof(p_snapshot->'uploadError') NOT IN ('boolean','null')
    OR EXISTS (SELECT 1 FROM jsonb_each(p_snapshot) p WHERE p.key IN ('uploadPendingCount','uploadBlockedCount') AND p.value <> 'null'::jsonb AND (jsonb_typeof(p.value) <> 'number' OR (p.value #>> '{}')::numeric NOT BETWEEN 0 AND 4096 OR (p.value #>> '{}')::numeric <> trunc((p.value #>> '{}')::numeric)))
    OR NOT (p_snapshot->'errorCategory' = 'null'::jsonb OR (jsonb_typeof(p_snapshot->'errorCategory') = 'string' AND p_snapshot->>'errorCategory' IN ('audio_capture','transcription')))
    OR NOT (p_snapshot->'lastRecordingDurationSeconds' = 'null'::jsonb OR jsonb_typeof(p_snapshot->'lastRecordingDurationSeconds') = 'number')
    OR EXISTS (SELECT 1 FROM jsonb_each(p_snapshot) p WHERE p.key IN ('lastRecordingStartedAt','lastRecordingStoppedAt') AND p.value <> 'null'::jsonb AND (jsonb_typeof(p.value) <> 'string' OR p.value #>> '{}' !~ '^20[0-9]{2}-[0-9]{2}-[0-9]{2}T[0-9:.]+(Z|[+-][0-9:]+)$'))
    OR (p_snapshot->>'lastRecordingStartedAt')::timestamptz > now() + interval '5 minutes'
    OR (p_snapshot->>'lastRecordingStoppedAt')::timestamptz > now() + interval '5 minutes'
    OR (p_snapshot->>'lastRecordingDurationSeconds')::numeric NOT BETWEEN 0 AND 86400
  THEN RAISE EXCEPTION 'invalid health snapshot' USING ERRCODE = '22023'; END IF;
  -- Serialize per account: bounded device registry and receipt-time throttling
  -- remain valid even when several requests arrive together.
  PERFORM pg_advisory_xact_lock(hashtextextended(actor::text, 0));
  DELETE FROM public.application_health_devices WHERE user_id = actor AND last_seen_at < now() - interval '90 days';
  DELETE FROM public.application_health_devices WHERE (user_id,device_id) IN (
    SELECT d.user_id,d.device_id FROM public.application_health_devices d
    WHERE d.last_seen_at < now() - interval '90 days'
    ORDER BY d.last_seen_at LIMIT 64 FOR UPDATE SKIP LOCKED
  );
  IF NOT EXISTS (SELECT 1 FROM public.application_health_devices WHERE user_id = actor AND device_id = device)
    AND (SELECT count(*) FROM public.application_health_devices WHERE user_id = actor) >= 8
  THEN RAISE EXCEPTION 'device limit reached' USING ERRCODE = '22023'; END IF;
  stamped := p_snapshot || jsonb_build_object('googleConnected', EXISTS (
    SELECT 1 FROM public.nango_connections n WHERE n.user_id = actor
      AND n.integration_id = 'google-meet' AND n.status = 'connected'));
  IF octet_length(stamped::text) > 4096 THEN
    RAISE EXCEPTION 'invalid health snapshot' USING ERRCODE = '22023';
  END IF;
  INSERT INTO public.application_health_devices(user_id,device_id,observed_at,snapshot)
  VALUES (actor,device,observed,stamped)
  ON CONFLICT (user_id,device_id) DO UPDATE SET
    last_seen_at = now(), observed_at = excluded.observed_at, snapshot = excluded.snapshot || jsonb_build_object(
      'lastRecordingStartedAt',COALESCE(NULLIF(excluded.snapshot->'lastRecordingStartedAt','null'::jsonb),application_health_devices.snapshot->'lastRecordingStartedAt'),
      'lastRecordingStoppedAt',COALESCE(NULLIF(excluded.snapshot->'lastRecordingStoppedAt','null'::jsonb),application_health_devices.snapshot->'lastRecordingStoppedAt'),
      'lastRecordingDurationSeconds',COALESCE(NULLIF(excluded.snapshot->'lastRecordingDurationSeconds','null'::jsonb),application_health_devices.snapshot->'lastRecordingDurationSeconds'))
  WHERE application_health_devices.last_seen_at <= now() - interval '15 seconds'
    AND application_health_devices.observed_at < excluded.observed_at;
EXCEPTION WHEN invalid_text_representation OR invalid_datetime_format OR datetime_field_overflow OR numeric_value_out_of_range THEN
  RAISE EXCEPTION 'invalid health snapshot' USING ERRCODE = '22023';
END; $$;
REVOKE ALL ON FUNCTION public.application_health_observe(jsonb) FROM PUBLIC, anon;
GRANT EXECUTE ON FUNCTION public.application_health_observe(jsonb) TO authenticated;

ALTER TABLE public.recording_captures ADD COLUMN transcribed_at timestamptz;
CREATE FUNCTION private.recording_transcribed_at() RETURNS trigger
LANGUAGE plpgsql SET search_path = '' AS $$ BEGIN
  IF NEW.transcript_json IS NOT NULL AND OLD.transcript_json IS DISTINCT FROM NEW.transcript_json THEN
    NEW.transcribed_at := now();
  END IF;
  RETURN NEW;
END; $$;
REVOKE ALL ON FUNCTION private.recording_transcribed_at() FROM PUBLIC, anon, authenticated;
CREATE TRIGGER recording_transcribed_at BEFORE UPDATE OF transcript_json ON public.recording_captures
FOR EACH ROW EXECUTE FUNCTION private.recording_transcribed_at();

