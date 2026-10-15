begin;
select plan(17);

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
  tests.get_supabase_uid('cloud_share_owner'),
  'meeting-1',
  '{"id":"meeting-1","title":"E3 call","transcripts":[],"calendar_event_key":"google-event|2026-10-15T15:00:00.000Z"}'::jsonb
), (
  tests.get_supabase_uid('cloud_share_member'),
  'meeting-2',
  '{"id":"meeting-2","title":"Duplicate E3 call","transcripts":[],"calendar_event_key":"google-event|2026-10-15T15:00:00.000Z"}'::jsonb
), (
  tests.get_supabase_uid('cloud_share_owner'),
  'unscheduled',
  '{"id":"unscheduled","title":"Unscheduled","transcripts":[]}'::jsonb
);

select ok(
  (select relrowsecurity from pg_class where oid = 'public.api_shared_session_snapshots'::regclass),
  'shared snapshot mapping has RLS'
);
select ok(
  not has_table_privilege('authenticated', 'public.api_shared_session_snapshots', 'SELECT')
  and not has_function_privilege('authenticated', 'public.read_shared_cloud_api_snapshot(uuid,uuid)', 'EXECUTE'),
  'clients cannot bypass server authorization'
);
select is(
  (select count(*) from public.list_shared_cloud_api_snapshots(
    tests.get_supabase_uid('cloud_share_member'), 20, 0
  )),
  0::bigint,
  'private snapshot is not shared by default'
);
select throws_ok(
  format(
    'select * from public.set_cloud_api_snapshot_workspace(%L::uuid, %L, %L::uuid)',
    tests.get_supabase_uid('cloud_share_outsider'), 'meeting-1',
    (select workspace_id from cloud_share_test_state)
  ),
  'P0002', 'cloud snapshot not found',
  'another user cannot share the owner snapshot'
);
select throws_ok(
  format(
    'select * from public.set_cloud_api_snapshot_workspace(%L::uuid, %L, %L::uuid)',
    tests.get_supabase_uid('cloud_share_owner'), 'unscheduled',
    (select workspace_id from cloud_share_test_state)
  ),
  '22023', 'calendar event key required',
  'a share requires a stable calendar occurrence key'
);

update cloud_share_test_state set share_id = (
  select share_id from public.set_cloud_api_snapshot_workspace(
    tests.get_supabase_uid('cloud_share_owner'),
    'meeting-1', workspace_id
  )
);

select is(
  (select count(*) from public.list_shared_cloud_api_snapshots(
    tests.get_supabase_uid('cloud_share_member'), 20, 0
  )),
  1::bigint,
  'active workspace member sees one shared meeting'
);
select throws_ok(
  format(
    'select * from public.set_cloud_api_snapshot_workspace(%L::uuid, %L, %L::uuid)',
    tests.get_supabase_uid('cloud_share_member'), 'meeting-2',
    (select workspace_id from cloud_share_test_state)
  ),
  '23505', 'calendar event already shared',
  'a second recorder cannot replace the first canonical meeting'
);
select lives_ok(
  format(
    $$update public.api_session_snapshots
      set content_json = jsonb_set(
        content_json || '{"title":"Updated notes"}'::jsonb,
        '{calendar_event_key}', to_jsonb('rescheduled-event'::text))
      where user_id = %L::uuid and session_id = 'meeting-1'$$,
    tests.get_supabase_uid('cloud_share_owner')
  ),
  'rescheduled calendar metadata does not block notes or transcript sync'
);
select is(
  (select content_json ->> 'title'
   from public.read_shared_cloud_api_snapshot(
     tests.get_supabase_uid('cloud_share_member'),
     (select share_id from cloud_share_test_state))),
  'Updated notes',
  'members receive the latest meeting content'
);
select is(
  (select content_json ->> 'calendar_event_key'
   from public.read_shared_cloud_api_snapshot(
     tests.get_supabase_uid('cloud_share_member'),
     (select share_id from cloud_share_test_state))),
  'google-event|2026-10-15T15:00:00.000Z',
  'shared reads retain the original canonical occurrence key'
);
select is(
  (select content_json ->> 'calendar_event_key'
   from public.list_shared_cloud_api_snapshots(
     tests.get_supabase_uid('cloud_share_member'), 20, 0)),
  'google-event|2026-10-15T15:00:00.000Z',
  'shared lists retain the canonical key used for client matching'
);
select is(
  (select share_id from public.set_cloud_api_snapshot_workspace(
    tests.get_supabase_uid('cloud_share_owner'),
    'meeting-1', (select workspace_id from cloud_share_test_state))),
  (select share_id from cloud_share_test_state),
  'repeat sharing after a reschedule preserves the existing share identity'
);
select is(
  (select count(*) from public.list_shared_cloud_api_snapshots(
    tests.get_supabase_uid('cloud_share_outsider'), 20, 0
  )),
  0::bigint,
  'outsider cannot list meeting'
);
select is(
  (select count(*) from public.read_shared_cloud_api_snapshot(
    tests.get_supabase_uid('cloud_share_outsider'),
    (select share_id from cloud_share_test_state)
  )),
  0::bigint,
  'outsider cannot read transcript by knowing share id'
);

update public.workspace_memberships set deleted_at = now()
where workspace_id = (select workspace_id from cloud_share_test_state)
  and user_id = tests.get_supabase_uid('cloud_share_member');
select is(
  (select count(*) from public.read_shared_cloud_api_snapshot(
    tests.get_supabase_uid('cloud_share_member'),
    (select share_id from cloud_share_test_state)
  )),
  0::bigint,
  'membership revocation immediately removes transcript access'
);

select ok(
  public.delete_cloud_api_snapshot_workspace(
    tests.get_supabase_uid('cloud_share_owner'), 'meeting-1'
  ),
  'owner can unshare the meeting'
);
select is(
  (select count(*) from public.api_shared_session_snapshots),
  0::bigint,
  'unshare removes the canonical access mapping'
);

select * from finish();
rollback;
