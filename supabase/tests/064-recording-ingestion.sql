begin;
select plan(25);
select tests.create_supabase_user('recording_owner', 'recording-owner@example.com');
select tests.create_supabase_user('recording_member', 'recording-member@example.com');
select tests.create_supabase_user('recording_other', 'recording-other@example.com');

select tests.authenticate_as_service_role();
insert into public.workspace_memberships(workspace_id, user_id, role)
values (tests.get_supabase_uid('recording_owner'), tests.get_supabase_uid('recording_member'), 'member');

select is(public.recording_admit(tests.get_supabase_uid('recording_owner'), 'meet-1',
  tests.get_supabase_uid('recording_owner'), 'GoogleMeet:conference-1', '{"durationFrames":1}', 1000),
  1000::bigint, 'Admission stores the authoritative meeting epoch');
select throws_ok($$select public.recording_admit(tests.get_supabase_uid('recording_owner'), 'meet-1',
  tests.get_supabase_uid('recording_member'), 'GoogleMeet:different', '{}', 1000)$$,
  '23505', 'recording identity conflict', 'A meeting cannot change provider identity');
select is((select count(*)::int from public.recording_admissions), 1,
  'A conflicting admission does not leave an actor grant behind');
select is(public.recording_admit(tests.get_supabase_uid('recording_owner'), 'meet-1',
  tests.get_supabase_uid('recording_member'), 'GoogleMeet:conference-1', '{"durationFrames":1}', 2000),
  1000::bigint, 'A second recorder joins the same meeting without changing its epoch');
select throws_ok($$select public.recording_admit(tests.get_supabase_uid('recording_owner'), 'other-meet',
  tests.get_supabase_uid('recording_other'), 'GoogleMeet:other', '{}', 1000)$$,
  '42501', 'workspace membership required', 'Admission rejects an account outside the workspace');

select public.recording_upload(tests.get_supabase_uid('recording_owner'), 'meet-1', 'capture-1', 'device-source-1',
  jsonb_build_object('uploaderId', tests.get_supabase_uid('recording_owner'), 'sha256', 'sha-1', 'offsetFrames', 0),
  'test-source-1.wav', 16000, 32044);
select public.recording_upload(tests.get_supabase_uid('recording_owner'), 'meet-1', 'capture-2', 'device-source-2',
  jsonb_build_object('uploaderId', tests.get_supabase_uid('recording_member'), 'sha256', 'sha-2', 'offsetFrames', 16000),
  'test-source-2.wav', 16000, 32044);
select lives_ok($$select public.recording_upload(tests.get_supabase_uid('recording_owner'), 'meet-1', 'capture-1', 'device-source-1',
  jsonb_build_object('uploaderId', tests.get_supabase_uid('recording_owner'), 'sha256', 'sha-1', 'offsetFrames', 0),
  'test-source-1.wav', 16000, 32044)$$, 'An identical upload retry is idempotent');
select throws_ok($$select public.recording_upload(tests.get_supabase_uid('recording_owner'), 'meet-1', 'capture-1', 'device-source-1',
  jsonb_build_object('uploaderId', tests.get_supabase_uid('recording_owner'), 'sha256', 'different', 'offsetFrames', 0),
  'test-source-1.wav', 16000, 32044)$$,
  '23505', 'recording identity conflict', 'A capture cannot be replaced by a different recording');
select public.recording_save_transcript(tests.get_supabase_uid('recording_owner'), 'meet-1', 'capture-1', 'sha-1',
  '{"provider":"test","model":"test","result":{"text":"independent source"}}');
select is((select (metadata->>'durationFrames')::bigint from public.recording_occurrences where id = 'meet-1'),
  32000::bigint, 'Independent sources extend the meeting timeline');
select throws_ok($$select public.recording_save_transcript(tests.get_supabase_uid('recording_owner'),
  'meet-1', 'capture-1', 'wrong-sha', '{"result":{"text":"wrong source"}}')$$,
  '23505', 'recording identity conflict', 'A transcript cannot attach to different source audio');
select throws_ok($$select public.recording_save_transcript(tests.get_supabase_uid('recording_owner'),
  'meet-1', 'capture-1', 'sha-1', '{"provider":"test","model":"test","result":{"text":"replacement"}}')$$,
  '23505', 'recording identity conflict', 'A completed source transcript cannot be overwritten');
select lives_ok($$select public.recording_seal(tests.get_supabase_uid('recording_owner'), 'meet-1')$$,
  'An occurrence with sources can be sealed');
select throws_ok($$select public.recording_upload(tests.get_supabase_uid('recording_owner'), 'meet-1', 'capture-3', 'device-source-3',
  jsonb_build_object('uploaderId', tests.get_supabase_uid('recording_owner'), 'sha256', 'sha-3', 'offsetFrames', 0),
  'test-source-3.wav', 16000, 32044)$$,
  '23505', 'recording is sealed', 'Sealing rejects new sources');
insert into storage.objects(bucket_id, name) values
  ('recording-audio', 'test-source-1.wav'), ('recording-audio', 'test-source-2.wav'),
  ('recording-audio', 'uncommitted.wav');

select tests.authenticate_as('recording_member');
select is((select count(*)::int from public.recording_captures), 2, 'Another organization member reads both source captures');
select is((select transcript_json->'result'->>'text' from public.recording_captures where id = 'capture-1'),
  'independent source', 'Source transcript is shared with an organization member');
select is((select count(*)::int from storage.objects where bucket_id = 'recording-audio'), 2,
  'Only audio linked to live authorized captures is visible');
select throws_ok($$update public.recording_captures set transcript_json = '{}'$$,
  '42501', null, 'Clients cannot overwrite source transcripts');
select throws_ok($$select public.recording_seal(tests.get_supabase_uid('recording_owner'), 'meet-1')$$,
  '42501', null, 'Clients cannot invoke privileged ingestion functions');

select tests.authenticate_as('recording_other');
select is((select count(*)::int from public.recording_captures), 0, 'An unrelated account cannot read source transcripts');
select is((select count(*)::int from storage.objects where bucket_id = 'recording-audio'), 0,
  'An unrelated account cannot read source audio');
select tests.authenticate_as_service_role();
select results_eq($$select public.recording_delete(tests.get_supabase_uid('recording_owner'), 'meet-1', 'capture-1')$$,
  array['test-source-1.wav'::text], 'Deletion returns the private object to remove');
select results_eq($$select public.recording_delete(tests.get_supabase_uid('recording_owner'), 'meet-1', 'capture-1')$$,
  array['test-source-1.wav'::text], 'Deletion retries retain the object path for crash recovery');
select throws_ok($$select public.recording_save_transcript(tests.get_supabase_uid('recording_owner'),
  'meet-1', 'capture-1', 'sha-1', '{"provider":"test","model":"test","result":{"text":"late"}}')$$,
  'P0002', null, 'A late ASR response cannot revive a deleted source');
select tests.authenticate_as('recording_member');
select is((select count(*)::int from public.recording_captures), 1, 'Deleted sources disappear from organization reads');
select is((select count(*)::int from storage.objects where bucket_id = 'recording-audio'), 1,
  'Deleted source audio becomes inaccessible before physical removal');
select tests.authenticate_as_service_role();
update public.workspace_memberships set deleted_at = now()
where workspace_id = tests.get_supabase_uid('recording_owner') and user_id = tests.get_supabase_uid('recording_member');
select tests.authenticate_as('recording_member');
select is((select count(*)::int from public.recording_occurrences), 0, 'Revoking membership removes meeting access');
select * from finish();
rollback;
