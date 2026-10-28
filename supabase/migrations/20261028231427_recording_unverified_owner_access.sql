-- Unassigned audio stays private to its uploader until a meeting is verified.
CREATE FUNCTION private.recording_unverified_admitted(p_workspace uuid, p_occurrence text)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path = '' AS $$
  SELECT EXISTS (
    SELECT 1 FROM public.recording_admissions a
    WHERE a.workspace = p_workspace AND a.occurrence = p_occurrence AND a.actor = auth.uid()
  );
$$;
REVOKE ALL ON FUNCTION private.recording_unverified_admitted(uuid,text) FROM PUBLIC,anon;
GRANT EXECUTE ON FUNCTION private.recording_unverified_admitted(uuid,text) TO authenticated;

ALTER POLICY recording_occurrences_read_member ON public.recording_occurrences
USING (
  NOT deleted AND workspace IN (SELECT id FROM public.workspaces WHERE deleted_at IS NULL)
  AND (metadata->>'provider' IS DISTINCT FROM 'unverified'
    OR private.recording_unverified_admitted(workspace,id))
);
