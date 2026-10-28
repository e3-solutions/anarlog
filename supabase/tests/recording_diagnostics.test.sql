begin;
select plan(27);
select tests.create_supabase_user('diagnostics_owner','diagnostics-owner@example.com');
select tests.create_supabase_user('diagnostics_member','diagnostics-member@example.com');
select tests.create_supabase_user('diagnostics_outsider','diagnostics-outsider@example.com');
update auth.users set email_confirmed_at = now() where id in (tests.get_supabase_uid('diagnostics_owner'),tests.get_supabase_uid('diagnostics_member'),tests.get_supabase_uid('diagnostics_outsider'));
insert into public.workspaces(id,owner_user_id,kind,name) values ('00000000-0000-4000-8000-00000000d001',tests.get_supabase_uid('diagnostics_owner'),'shared','Diagnostics test');
insert into public.workspace_memberships(workspace_id,user_id,role) values
('00000000-0000-4000-8000-00000000d001',tests.get_supabase_uid('diagnostics_owner'),'owner'),
('00000000-0000-4000-8000-00000000d001',tests.get_supabase_uid('diagnostics_member'),'member');
insert into auth.sessions(id,user_id,created_at,updated_at) values
('00000000-0000-4000-8000-00000000d002',tests.get_supabase_uid('diagnostics_owner'),now(),now()),
('00000000-0000-4000-8000-00000000d003',tests.get_supabase_uid('diagnostics_member'),now(),now()),
('00000000-0000-4000-8000-00000000d004',tests.get_supabase_uid('diagnostics_outsider'),now(),now());
create temporary table diagnostic_event(value jsonb);
insert into diagnostic_event values(jsonb_build_object(
'id','00000000-0000-4000-8000-00000000d005','deviceId','00000000-0000-4000-8000-00000000d006',
'bootId','00000000-0000-4000-8000-00000000d007','sequence',1,'observedAtMs',floor(extract(epoch from now() - interval '6 days') * 1000),
'elapsedMs',200,'appVersion','0.1.22','buildSha','1234567','sessionId',null,'kind','policy_decision','reason','ax_unknown',
'evidence',jsonb_build_object('observation','unknown','inspectionStatus','timeout','autoStop',true,'micActive',false,'missingForMs',12000,'queuedCount',64,'droppedCount',2,'retentionDropped',true,'lastAckAgeMs',60000,'platform','slack','os','macos')));
grant select on diagnostic_event to authenticated;
select tests.authenticate_as('diagnostics_member');
select set_config('request.jwt.claims',jsonb_set(current_setting('request.jwt.claims')::jsonb,'{session_id}','"00000000-0000-4000-8000-00000000d003"')::text,true);
select is((select public.recording_diagnostics_ingest(jsonb_build_array(value)) from diagnostic_event),'["00000000-0000-4000-8000-00000000d005"]'::jsonb,'Offline events collected six days ago are acknowledged');
select lives_ok('select public.recording_diagnostics_ingest(jsonb_build_array(value)) from diagnostic_event','Retry after lost response is idempotent');
select is((select count(*) from public.recording_diagnostics),1::bigint,'Member reads their own single deduplicated event');
select lives_ok($$select public.recording_diagnostics_ingest(jsonb_build_array(value || '{"reason":"meeting_ended"}'::jsonb)) from diagnostic_event$$,'Replayed identifiers cannot mutate the stored evidence');
select is((select reason from public.recording_diagnostics),'ax_unknown','First accepted event remains immutable');
select throws_ok(
  format('select public.recording_diagnostics_ingest(jsonb_build_array(value || %L::jsonb)) from diagnostic_event',invalid.patch),
  '22023','invalid recording diagnostics',invalid.description
) from (values
('{"transcript":"private"}','Reject arbitrary content fields'),
('{"evidence":{"windowTitle":"private"}}','Reject content hidden inside evidence'),
('{"evidence":{"inspectionStatus":"private"}}','Reject free-form status strings'),
('{"evidence":{"autoStop":"true"}}','Reject mistyped boolean evidence'),
('{"evidence":{"missingForMs":86400001}}','Reject unbounded numeric evidence'),
('{"reason":"arbitrary-content"}','Reject unbounded reason strings'),
('{"observedAtMs":1}','Reject expired offline events'),
('{"deviceId":"invalid"}','Malformed UUIDs return a bounded error'),
('{"sequence":1.5}','Reject fractional sequence numbers'),
('{"sessionId":42}','Reject mistyped optional UUIDs')
) invalid(patch,description);
select throws_ok('select public.recording_diagnostics_ingest(jsonb_agg(value)) from diagnostic_event cross join generate_series(1,65)','22023','invalid recording diagnostics','Batch limit bounds processing and payload size');
select throws_ok($$select public.recording_diagnostics_ingest(jsonb_build_array(value || '{"id":"00000000-0000-4000-8000-00000000d008"}'::jsonb,value || '{"reason":"private"}'::jsonb)) from diagnostic_event$$,'22023','invalid recording diagnostics','Invalid batch rolls back earlier events');
select is((select count(*) from public.recording_diagnostics),1::bigint,'Partial invalid batches leave no inserted rows');
select ok(not has_table_privilege('authenticated','public.recording_diagnostics','INSERT'),'Clients cannot bypass the sanitized RPC');
select throws_ok($$select public.recording_diagnostics_ingest(jsonb_agg(value || jsonb_build_object('id',md5('diagnostic-event-' || n)::uuid,'deviceId',md5('diagnostic-device-' || n)::uuid))) from diagnostic_event cross join generate_series(1,8) n$$,'22023','recording diagnostics device limit reached','Per-account device quota bounds persistent storage');
select is((select count(*) from public.recording_diagnostics),1::bigint,'Rejected device batches leave no partial rows');
select tests.clear_authentication();
select tests.authenticate_as('diagnostics_owner');
select set_config('request.jwt.claims',jsonb_set(current_setting('request.jwt.claims')::jsonb,'{session_id}','"00000000-0000-4000-8000-00000000d002"')::text,true);
select is((select count(*) from public.recording_diagnostics),1::bigint,'Workspace owners can investigate member events');
select lives_ok('select public.recording_diagnostics_ingest(jsonb_build_array(value)) from diagnostic_event','A colliding event UUID from a different account stays independently owned');
select is((select count(*) from public.recording_diagnostics),2::bigint,'Cross-account UUID collision never overwrites another account');
select tests.clear_authentication();
select tests.authenticate_as('diagnostics_outsider');
select set_config('request.jwt.claims',jsonb_set(current_setting('request.jwt.claims')::jsonb,'{session_id}','"00000000-0000-4000-8000-00000000d004"')::text,true);
select is((select count(*) from public.recording_diagnostics),0::bigint,'Unrelated accounts cannot read diagnostic evidence');
select throws_ok('select public.recording_diagnostics_ingest(jsonb_build_array(value)) from diagnostic_event','42501','active organization session required','Confirmed users without shared membership cannot ingest');
select tests.clear_authentication();
reset role;
delete from auth.sessions where id = '00000000-0000-4000-8000-00000000d003';
select tests.authenticate_as('diagnostics_member');
select set_config('request.jwt.claims',jsonb_set(current_setting('request.jwt.claims')::jsonb,'{session_id}','"00000000-0000-4000-8000-00000000d003"')::text,true);
select throws_ok('select public.recording_diagnostics_ingest(jsonb_build_array(value)) from diagnostic_event','42501','active organization session required','Session revocation blocks ingestion despite unexpired JWT');
select * from finish();
rollback;
