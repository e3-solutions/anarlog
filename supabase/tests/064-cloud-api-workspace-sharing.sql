begin;
select no_plan();

select tests.create_supabase_user('cloud_share_owner', 'cloud-share-owner@example.com');
select tests.create_supabase_user('cloud_share_member', 'cloud-share-member@example.com');
select tests.create_supabase_user('cloud_share_outsider', 'cloud-share-outsider@example.com');
select tests.authenticate_as_service_role();

create temporary table cloud_share_test_state (workspace_id uuid, share_id uuid);
insert into cloud_share_test_state (workspace_id) values (gen_random_uuid());

insert into public.workspaces (id, owner_user_id, kind, name)
select workspace_id, tests.get_supabase_uid('cloud_share_owner'), 'shared', 'E3 test'
from cloud_share_test_state;

insert into public.workspace_memberships (workspace_id, user_id, role)
select workspace_id, tests.get_supabase_uid('cloud_share_owner'), 'owner'
from cloud_share_test_state
union all
select workspace_id, tests.get_supabase_uid('cloud_share_member'), 'member'
from cloud_share_test_state;

insert into public.api_session_snapshots (user_id, session_id, content_json)
values (
  tests.get_supabase_uid('cloud_share_owner'), 'meeting-1',
  '{"id":"meeting-1","title":"Owner recording","transcripts":[{"text":"Owner meeting transcript"}],"calendar_event_key":"google:uid-1","shared_transcript_choice":{"meet_attendance_status":"verified","verified_overlap_seconds":9999,"recorded_seconds":30,"transcript_coverage_seconds":20,"transcript_characters":999999}}'::jsonb
), (
  tests.get_supabase_uid('cloud_share_member'), 'meeting-2',
  '{"id":"meeting-2","title":"Member recording","transcripts":[{"text":"Longer member transcript"}],"calendar_event_key":"google:uid-1","shared_transcript_choice":{"conference_record":"conferenceRecords/abc-123","meet_attendance_status":"verified","recorded_seconds":30,"verified_overlap_seconds":15,"transcript_coverage_seconds":20,"transcript_characters":24}}'::jsonb
), (
  tests.get_supabase_uid('cloud_share_owner'), 'empty',
  '{"id":"empty","title":"Empty","transcripts":[],"calendar_event_key":"google:uid-1"}'::jsonb
), (
  tests.get_supabase_uid('cloud_share_member'), 'adjacent',
  '{"id":"adjacent","title":"Adjacent event","transcripts":[{"text":"Adjacent transcript"}],"calendar_event_key":"google:uid-2","shared_transcript_choice":{"conference_record":"conferenceRecords/abc-123","meet_attendance_status":"verified","recorded_seconds":20,"verified_overlap_seconds":10,"transcript_coverage_seconds":15,"transcript_characters":19}}'::jsonb
);

select ok(
  (select relrowsecurity from pg_class where oid = 'public.api_shared_session_candidates'::regclass),
  'private candidate table has RLS'
);
select ok(
  not has_table_privilege('authenticated', 'public.api_shared_session_candidates', 'SELECT')
  and not has_table_privilege('authenticated', 'public.api_shared_session_snapshots', 'SELECT')
  and not has_function_privilege('authenticated', 'public.read_shared_cloud_api_snapshot(uuid,uuid)', 'EXECUTE'),
  'clients cannot bypass server authorization'
);
select is(
  (select count(*) from public.list_shared_cloud_api_snapshots(
    tests.get_supabase_uid('cloud_share_member'), 20, 0)),
  0::bigint, 'private snapshots are not shared by default'
);
select throws_ok(
  format('select * from public.set_cloud_api_snapshot_workspace(%L::uuid, %L, %L::uuid)',
    tests.get_supabase_uid('cloud_share_outsider'), 'meeting-1',
    (select workspace_id from cloud_share_test_state)),
  'P0002', 'cloud snapshot not found', 'a different user cannot register an owner snapshot'
);
select throws_ok(
  format('select * from public.set_cloud_api_snapshot_workspace(%L::uuid, %L, %L::uuid)',
    tests.get_supabase_uid('cloud_share_owner'), 'empty',
    (select workspace_id from cloud_share_test_state)),
  '22023', 'nonempty transcript required', 'empty transcript cannot become a candidate'
);

update cloud_share_test_state set share_id = (
  select share_id from public.set_cloud_api_snapshot_workspace(
    tests.get_supabase_uid('cloud_share_owner'), 'meeting-1', workspace_id)
);
select is(
  (select count(*) from public.list_shared_cloud_api_snapshots(
    tests.get_supabase_uid('cloud_share_member'), 20, 0)),
  1::bigint, 'first valid candidate creates one shared meeting'
);
select is(
  (select content_json ->> 'title' from public.read_shared_cloud_api_snapshot(
    tests.get_supabase_uid('cloud_share_member'),
    (select share_id from cloud_share_test_state))),
  'Owner recording', 'first valid candidate is selected initially'
);
select is(
  (select verified_overlap_seconds from public.api_shared_session_candidates
    where owner_user_id = tests.get_supabase_uid('cloud_share_owner')
      and session_id = 'meeting-1'),
  null::numeric, 'forged client attendance does not become verified overlap'
);
select is(
  (select transcript_characters from public.api_shared_session_candidates
    where owner_user_id = tests.get_supabase_uid('cloud_share_owner')
      and session_id = 'meeting-1'),
  24, 'stored transcript text count overrides forged client character count'
);

select is(
  (select share_id from public.set_cloud_api_snapshot_workspace(
    tests.get_supabase_uid('cloud_share_member'), 'meeting-2',
    (select workspace_id from cloud_share_test_state), 15,
    'conferenceRecords/abc-123')),
  (select share_id from cloud_share_test_state),
  'second recorder registers under the same stable share ID'
);
select is((select count(*) from public.api_shared_session_candidates),
  2::bigint, 'both private candidates are retained');
select is((select count(*) from public.api_shared_session_snapshots),
  1::bigint, 'the shared meeting still has one group row');
select is(
  (select owner_user_id from public.list_shared_cloud_api_snapshots(
    tests.get_supabase_uid('cloud_share_owner'), 20, 0)),
  tests.get_supabase_uid('cloud_share_member'),
  'verified overlap promotes the better candidate'
);
select is(
  (select content_json ->> 'title' from public.read_shared_cloud_api_snapshot(
    tests.get_supabase_uid('cloud_share_owner'),
    (select share_id from cloud_share_test_state))),
  'Member recording', 'read returns the chosen recording'
);
select is(
  (select share_id from public.set_cloud_api_snapshot_workspace(
    tests.get_supabase_uid('cloud_share_member'), 'meeting-2',
    (select workspace_id from cloud_share_test_state), null,
    'conferenceRecords/abc-123')),
  (select share_id from cloud_share_test_state),
  'an unavailable Meet recheck keeps the same share'
);
select is(
  (select verified_overlap_seconds from public.api_shared_session_candidates
    where owner_user_id = tests.get_supabase_uid('cloud_share_member')
      and session_id = 'meeting-2'),
  15::numeric, 'an unavailable Meet recheck retains verified evidence for unchanged recording'
);
update public.api_session_snapshots
set content_json = jsonb_set(
  jsonb_set(content_json, '{shared_transcript_choice,transcript_coverage_seconds}', '604800'::jsonb),
  '{transcripts}', '[{"text":"x"}]'::jsonb)
where user_id = tests.get_supabase_uid('cloud_share_owner') and session_id = 'meeting-1';
select is(
  (select share_id from public.set_cloud_api_snapshot_workspace(
    tests.get_supabase_uid('cloud_share_owner'), 'meeting-1',
    (select workspace_id from cloud_share_test_state), 1000)),
  (select share_id from cloud_share_test_state),
  'long recording still joins the same group'
);
select is(
  (select owner_user_id from public.list_shared_cloud_api_snapshots(
    tests.get_supabase_uid('cloud_share_owner'), 20, 0)),
  tests.get_supabase_uid('cloud_share_member'),
  'a one-character recording cannot win by claiming a week of speech coverage'
);
select ok(
  (select content_json ? 'shared_transcript_choice' from public.read_shared_cloud_api_snapshot(
    tests.get_supabase_uid('cloud_share_owner'),
    (select share_id from cloud_share_test_state))) = false,
  'private selection evidence is removed from shared reads'
);
select ok(
  (select content_json ? 'shared_transcript_choice' from public.list_shared_cloud_api_snapshots(
    tests.get_supabase_uid('cloud_share_owner'), 20, 0)) = false,
  'private selection evidence is removed from shared lists'
);

update public.api_session_snapshots
set content_json = jsonb_set(
  content_json || '{"title":"Improved owner recording","calendar_event_key":"google:uid-1","shared_transcript_choice":{"conference_record":"conferenceRecords/abc-123","meet_attendance_status":"verified","recorded_seconds":40,"verified_overlap_seconds":30,"transcript_coverage_seconds":35,"transcript_characters":16}}'::jsonb,
  '{transcripts}', '[{"text":"Better owner transcript!"}]'::jsonb)
where user_id = tests.get_supabase_uid('cloud_share_owner') and session_id = 'meeting-1';
select is(
  (select share_id from public.set_cloud_api_snapshot_workspace(
    tests.get_supabase_uid('cloud_share_owner'), 'meeting-1',
    (select workspace_id from cloud_share_test_state), 30)),
  (select share_id from cloud_share_test_state),
  'republishing retains the original share identity and occurrence key'
);
select is(
  (select content_json ->> 'title' from public.read_shared_cloud_api_snapshot(
    tests.get_supabase_uid('cloud_share_member'),
    (select share_id from cloud_share_test_state))),
  'Improved owner recording', 'higher verified overlap promotes the improved recording'
);
select is(
  (select content_json ->> 'calendar_event_key' from public.read_shared_cloud_api_snapshot(
    tests.get_supabase_uid('cloud_share_member'),
    (select share_id from cloud_share_test_state))),
  'google:uid-1', 'shared reads retain the canonical occurrence key'
);
select is(
  (select content_json ->> 'calendar_event_key' from public.list_shared_cloud_api_snapshots(
    tests.get_supabase_uid('cloud_share_member'), 20, 0)),
  'google:uid-1', 'shared lists retain the canonical occurrence key'
);

update public.api_session_snapshots
set content_json = jsonb_set(content_json, '{calendar_event_key}', '"google:uid-3"'::jsonb)
where user_id = tests.get_supabase_uid('cloud_share_owner') and session_id = 'meeting-1';
select isnt(
  (select share_id from public.set_cloud_api_snapshot_workspace(
    tests.get_supabase_uid('cloud_share_owner'), 'meeting-1',
    (select workspace_id from cloud_share_test_state), 30)),
  (select share_id from cloud_share_test_state),
  'reassociated session moves to its new calendar occurrence'
);
select is(
  (select content_json ->> 'title' from public.read_shared_cloud_api_snapshot(
    tests.get_supabase_uid('cloud_share_member'),
    (select share_id from cloud_share_test_state))),
  'Member recording', 'old occurrence no longer exposes reassociated transcript'
);
update public.api_session_snapshots
set content_json = jsonb_set(content_json, '{calendar_event_key}', '"google:uid-1"'::jsonb)
where user_id = tests.get_supabase_uid('cloud_share_owner') and session_id = 'meeting-1';
select is(
  (select share_id from public.set_cloud_api_snapshot_workspace(
    tests.get_supabase_uid('cloud_share_owner'), 'meeting-1',
    (select workspace_id from cloud_share_test_state), 30)),
  (select share_id from cloud_share_test_state),
  'returning to the original occurrence restores its stable share ID'
);

select is(
  (select share_id from public.set_cloud_api_snapshot_workspace(
    tests.get_supabase_uid('cloud_share_member'), 'adjacent',
    (select workspace_id from cloud_share_test_state), 10)) is distinct from
    (select share_id from cloud_share_test_state),
  true, 'the same Meet conference on an adjacent calendar event is a separate share'
);
select is((select count(*) from public.api_shared_session_snapshots),
  2::bigint, 'adjacent calendar occurrences remain separate groups');
select is(
  (select share_id from public.set_cloud_api_snapshot_workspace(
    tests.get_supabase_uid('cloud_share_member'), 'adjacent',
    (select workspace_id from cloud_share_test_state), 0)) is not null,
  true, 'zero overlap does not prevent registration'
);
select is(
  (select verified_overlap_seconds from public.api_shared_session_candidates
    where owner_user_id = tests.get_supabase_uid('cloud_share_member')
      and session_id = 'adjacent'),
  0::numeric, 'a measured zero overlap replaces stale positive evidence'
);
select is(
  (select count(*) from public.read_shared_cloud_api_snapshot(
    tests.get_supabase_uid('cloud_share_outsider'),
    (select share_id from cloud_share_test_state))),
  0::bigint, 'outsider cannot read a transcript by knowing its share ID'
);

update public.workspace_memberships set deleted_at = now()
where workspace_id = (select workspace_id from cloud_share_test_state)
  and user_id = tests.get_supabase_uid('cloud_share_owner');
select is(
  (select content_json ->> 'title' from public.read_shared_cloud_api_snapshot(
    tests.get_supabase_uid('cloud_share_member'),
    (select share_id from cloud_share_test_state))),
  'Member recording', 'owner revocation immediately fails over to another active recorder'
);
select is(
  (select count(*) from public.read_shared_cloud_api_snapshot(
    tests.get_supabase_uid('cloud_share_owner'),
    (select share_id from cloud_share_test_state))),
  0::bigint, 'revoked owner cannot read the shared transcript'
);
select ok(public.delete_cloud_api_snapshot_workspace(
  tests.get_supabase_uid('cloud_share_member'), 'meeting-2'),
  'member can withdraw their own candidate');
select is(
  (select count(*) from public.read_shared_cloud_api_snapshot(
    tests.get_supabase_uid('cloud_share_member'),
    (select share_id from cloud_share_test_state))),
  0::bigint, 'no revoked owner transcript leaks after the remaining candidate withdraws'
);
select is((select count(*) from public.api_shared_session_snapshots
  where id = (select share_id from cloud_share_test_state)),
  1::bigint, 'group identity survives while a revoked owner candidate still exists');

update public.workspace_memberships set deleted_at = null
where workspace_id = (select workspace_id from cloud_share_test_state)
  and user_id = tests.get_supabase_uid('cloud_share_owner');
select is(
  (select content_json ->> 'title' from public.read_shared_cloud_api_snapshot(
    tests.get_supabase_uid('cloud_share_member'),
    (select share_id from cloud_share_test_state))),
  'Improved owner recording', 'restored membership revives the same share ID');
update public.api_session_snapshots
set content_json = jsonb_set(content_json, '{transcripts}', '[]'::jsonb)
where user_id = tests.get_supabase_uid('cloud_share_owner') and session_id = 'meeting-1';
select is(
  (select count(*) from public.read_shared_cloud_api_snapshot(
    tests.get_supabase_uid('cloud_share_member'),
    (select share_id from cloud_share_test_state))),
  0::bigint, 'a stale candidate cannot expose a snapshot whose transcript became empty');
update public.api_session_snapshots
set content_json = jsonb_set(content_json, '{transcripts}',
  '[{"text":"Restored owner transcript"}]'::jsonb)
where user_id = tests.get_supabase_uid('cloud_share_owner') and session_id = 'meeting-1';
select ok(public.delete_cloud_api_snapshot_workspace(
  tests.get_supabase_uid('cloud_share_owner'), 'meeting-1'),
  'owner can withdraw the final candidate');
select is((select count(*) from public.api_shared_session_snapshots
  where id = (select share_id from cloud_share_test_state)),
  0::bigint, 'group is removed after every candidate withdraws');

select * from finish();
rollback;
