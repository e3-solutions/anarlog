begin;
select plan(10);
select tests.create_supabase_user('health_owner', 'health-owner@example.com');
select tests.create_supabase_user('health_member', 'health-member@example.com');
update auth.users set email_confirmed_at = now() where id in (tests.get_supabase_uid('health_owner'),tests.get_supabase_uid('health_member'));
insert into public.workspaces(id,owner_user_id,kind,name)
values ('00000000-0000-4000-8000-00000000f001',tests.get_supabase_uid('health_owner'),'shared','Health test');
insert into public.workspace_memberships(workspace_id,user_id,role) values
('00000000-0000-4000-8000-00000000f001',tests.get_supabase_uid('health_owner'),'owner'),
('00000000-0000-4000-8000-00000000f001',tests.get_supabase_uid('health_member'),'member');
insert into auth.sessions(id,user_id,created_at,updated_at) values
('00000000-0000-4000-8000-00000000f002',tests.get_supabase_uid('health_owner'),now(),now()),
('00000000-0000-4000-8000-00000000f003',tests.get_supabase_uid('health_member'),now(),now());
create temporary table health_snapshot(value jsonb);
insert into health_snapshot values(jsonb_build_object('deviceId','00000000-0000-4000-8000-00000000f004','appVersion','0.1.21','observedAt',to_char(now() at time zone 'UTC','YYYY-MM-DD"T"HH24:MI:SS"Z"'),'permissions',jsonb_build_object('microphone','denied','systemAudio','unknown','accessibility','neverRequested'),'autoRecordDetected',false,'autoRecordScheduled',false,'recording',null,'uploadPendingCount',null,'uploadBlockedCount',null,'uploadError',null,'lastRecordingStartedAt',null,'lastRecordingStoppedAt',null,'lastRecordingDurationSeconds',null,'errorCategory',null));
grant select on health_snapshot to authenticated;
select tests.authenticate_as('health_member');
select set_config('request.jwt.claims',jsonb_set(current_setting('request.jwt.claims')::jsonb,'{session_id}','"00000000-0000-4000-8000-00000000f003"')::text,true);
select lives_ok('select public.application_health_observe(value) from health_snapshot','A live member session can report partial/unknown permissions');
select lives_ok('select public.application_health_observe(value) from health_snapshot','A duplicate observation is idempotent');
select is((select count(*) from public.application_health_devices),0::bigint,'Ordinary members cannot read device health');
select throws_ok($$select public.application_health_observe(value || '{"transcript":"private"}'::jsonb) from health_snapshot$$,'22023','invalid health snapshot','Ingestion rejects content fields through direct RPC');
select throws_ok(
  format('select public.application_health_observe(value || %L::jsonb) from health_snapshot', invalid.snapshot),
  '22023', 'invalid health snapshot', invalid.description
) from (values
  ('{"deviceId":"not-a-uuid"}', 'Malformed device identifiers return a bounded validation error'),
  ('{"observedAt":"not-a-timestamp"}', 'Malformed observation times return a bounded validation error'),
  ('{"lastRecordingStoppedAt":"2026-02-31T00:00:00Z"}', 'Impossible recording dates return a bounded validation error')
) as invalid(snapshot, description);
select tests.clear_authentication();
select tests.authenticate_as('health_owner');
select set_config('request.jwt.claims',jsonb_set(current_setting('request.jwt.claims')::jsonb,'{session_id}','"00000000-0000-4000-8000-00000000f002"')::text,true);
select is((select count(*) from public.application_health_devices),1::bigint,'Owner RLS reads only the single deduplicated observation');
select ok(not has_table_privilege('authenticated','public.application_health_devices','INSERT'),'Clients cannot bypass authenticated ingestion with direct table writes');
select tests.clear_authentication();
delete from auth.sessions where id = '00000000-0000-4000-8000-00000000f003';
select tests.authenticate_as('health_member');
select set_config('request.jwt.claims',jsonb_set(current_setting('request.jwt.claims')::jsonb,'{session_id}','"00000000-0000-4000-8000-00000000f003"')::text,true);
select throws_ok('select public.application_health_observe(value) from health_snapshot','42501','active organization session required','Revocation blocks health writes even with an unexpired access token');
select * from finish();
rollback;
