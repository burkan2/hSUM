import datetime,hashlib,json,math,pathlib,subprocess,sys
repo=pathlib.Path('/Users/b.k./Desktop/hSUM');sys.path.insert(0,str(repo))
from benches.retrieval_stress import harness as h
from benches.retrieval_scale import harness as scale
root=pathlib.Path(__file__).resolve().parent
read=lambda p:json.loads(p.read_text())
sha=lambda p:hashlib.file_digest(p.open('rb'),'sha256').hexdigest()
candidate='38272481bfbc9bb831ac05304135f3bdc54dda05'
r=read(root/'raw/linux-x86_64-1m.json');run=read(root/'run.json');meta=read(root/'artifact-metadata.json')['artifacts'][0]
assert meta['digest']=='sha256:'+sha(root/'original-artifact.zip')
assert r['git']=={'commit':candidate,'working_tree_dirty':False} and run['headSha']==meta['workflow_run']['head_sha']==candidate
assert run['status']=='completed' and run['conclusion']=='success' and r['passed'] is True and r['schema_version']==h.SCHEMA_VERSION
for name in ['benches/retrieval_stress/harness.py','benches/retrieval_stress/manifest.json','benches/retrieval_scale/harness.py']:
 assert subprocess.check_output(['git','show',candidate+':'+name],cwd=repo)==(repo/name).read_bytes(),name
manifest=h.load_manifest();assert r['manifest_sha256']==h.manifest_sha256() and r['corpus_fingerprint_sha256']==h.corpus_fingerprint(manifest)
dist=h.documents_per_source(manifest)
assert r['corpus']['documents_per_source']==dist and sum(dist)==100000 and max(dist)*10==15880
assert r['corpus']['embedding_input_identity']==manifest['embedding_input_identity']
assert r['machine']['system']=='Linux' and r['machine']['architecture']=='x86_64'
assert [s['source_count'] for s in r['scale_samples']]==manifest['scale_source_points']
observation_count=0
for sample in r['scale_samples']:
 docs=sum(dist[:sample['source_count']-1]);assert sample['documents']==docs and sample['passages']==docs*10
 assert sample['managed_index_bytes']>0 and set(sample['queries'])=={q['id'] for q in manifest['queries'][:-1]}
 for query in manifest['queries'][:-1]:
  summary=sample['queries'][query['id']];obs=summary['observations'];assert len(obs)==5 and h.summarize_probes(obs)==summary
  for o in obs:
   assert o['outcome']=='success' and 0<o['result_count']<=10 and o['client_round_trip_ms']>0
   packet={'requested_mode':query['mode'],'effective_mode':query['mode'],'degraded_mode':[],'results':[{}]*o['result_count'],'examined':o['examined'],'retrievers':o['retrievers'],'stop_reason':o['stop_reason'],'timing_ms':o['timing_ms']}
   h.validate_search_packet(query,packet)
   assert all(math.isfinite(v) and v>=0 for v in o['timing_ms'].values())
  observation_count+=len(obs)
assert observation_count==75
# JSON sorts object keys; restore the frozen manifest order used during execution.
ordered_samples=[dict(sample,queries={q['id']:sample['queries'][q['id']] for q in manifest['queries'][:-1]}) for sample in r['scale_samples']]
assert r['first_observed_knee']==h.first_observed_knee(ordered_samples,2.0)
assert r['first_observed_deadline']==h.first_observed_deadline(r['scale_samples']) is None
hybrid=r['final_hybrid'];assert len(hybrid['observations'])==5 and h.summarize_probes(hybrid['observations'])==hybrid
assert all(o['outcome']=='success' and 'vector' in o['retrievers'] and o['result_count']>0 for o in hybrid['observations'])
assert all(all(0<=v<=500 for v in o['examined'].values()) for o in hybrid['observations'])
assert len(r['ingest']['operations'])==5
for op,sample in zip(r['ingest']['operations'],r['scale_samples']):
 assert (op['source_count'],op['documents'],op['passages'])==(sample['source_count'],sample['documents'],sample['passages'])
 assert op['peak_process_tree_rss_bytes']>0 and math.isclose(op['passages_per_second'],op['passages_added']/op['seconds'])
assert math.isclose(r['ingest']['total_seconds'],sum(o['seconds'] for o in r['ingest']['operations']))
assert r['ingest']['maximum_peak_process_tree_rss_bytes']==max(o['peak_process_tree_rss_bytes'] for o in r['ingest']['operations'])
metadata=r['metadata_only_generations'];ops=metadata['operations'];old=metadata['old_readers']
assert ops['count']==ops['seconds']['n']==100 and ops['maximum_peak_process_tree_rss_bytes']>0
assert old['count']==4 and old['all_prior_snapshots_retained'] is True
assert old['prior_generations']==old['retained_generations']==[104]*4 and old['fresh_reader_generation']==105
assert r['database_counts']=={'chunk_embeddings':15880,'chunks':10,'content_blobs':1,'document_versions':100100,'documents':100000,'generations':106,'project_sources':64,'sources':64}
cancel=r['cancel_storm'];assert cancel['cancelled_requests']==8 and cancel['real_worker_processes_paused']==2
for key in ['late_responses_suppressed','lexical_recovery','hybrid_recovery']:assert cancel[key] is True
assert cancel['peak_process_tree_rss_bytes']>0
for key in ['lexical_examined','hybrid_examined']:assert all(0<=v<=500 for v in cancel[key].values())
assert cancel['lexical_examined']['lexical']==500 and cancel['hybrid_examined']['vector']==500
reembed=r['reembed'];assert reembed['passages']==1000000 and reembed['peak_process_tree_rss_bytes']>0
assert math.isclose(reembed['passages_per_second'],1000000/reembed['seconds'])
assert 'Active passages: 1000000' in reembed['stdout_tail'] and 'Generation: 106' in reembed['stdout_tail']
st=r['storage'];assert st['cached_vector_rows']==15880 and st['logical_live_bytes']==15000 and st['logical_history_bytes']==0
assert st['logical_active_vector_payload_bytes']==1000000*384*4 and st['logical_cached_vector_payload_bytes']==15880*384*4
assert st['logical_total_vector_payload_bytes']==st['logical_active_vector_payload_bytes']+st['logical_cached_vector_payload_bytes']
assert st['managed_index_bytes']==sum(st['database_files'].values())
assert math.isclose(r['database_amplification']['versus_encoded_snapshots'],st['managed_index_bytes']/r['corpus']['encoded_snapshot_bytes'])
assert math.isclose(r['database_amplification']['versus_unique_body'],st['managed_index_bytes']/15000)
q=r['qualification'];assert q['report_only'] is True and q['latency_slo_applied'] is False and q['observed_deadline_count']==0 and all(v is True for v in q['checks'].values())
log=(root/'raw/harness.log').read_text();marker='stress observations before cancellation: '
packet=json.loads(next(line[len(marker):] for line in log.splitlines() if line.startswith(marker)))
for key in ['scale_samples','metadata_only_generations','reembed','final_hybrid']:assert packet[key]==r[key]
assert 'retrieval stress PASS;' in log
report={'reviewed_at_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'candidate':candidate,'run_url':run['url'],'artifact_id':meta['id'],'artifact_zip_sha256':sha(root/'original-artifact.zip'),'native_report_only_stress_pass':True,'qualification':q,'scale_observations_recomputed':observation_count,'final_vector_hybrid_observations_recomputed':5,'cancel_storm':cancel,'first_observed_knee':r['first_observed_knee'],'first_observed_deadline':None,'checks':{'artifact_digest_and_exact_clean_source_candidate':True,'frozen_manifest_and_corpus':True,'all_75_scale_observations_and_summaries':True,'all_5_final_hybrid_observations_and_summary':True,'candidate_caps_finite_timings_and_result_presence':True,'scale_knee_and_deadline_recomputed':True,'four_old_readers_retained_generation':True,'hundred_metadata_operations_recorded':True,'million_memberships_and_embedding_cache_counts':True,'eight_cancellations_two_workers_no_late_responses_and_both_recoveries':True,'memory_storage_and_amplification_recorded':True,'pre_cancellation_log_matches_final_report':True},'limits':['Report-only stress workload; no 100k latency/CV/cold-cache or superiority claim.','No independent worker timestamps or full MCP transcript are uploaded; cancellation/no-late-response evidence is the completion of the frozen same-process harness and its final summary.','Full index and executable are not uploaded, so physical integrity/model provenance/candidate byte validation cannot be independently rerun from these artifacts.','Metadata operations contain aggregate timing statistics; individual operation timings cannot be independently recomputed from the report.']}
(root/'review.json').write_text(json.dumps(report,indent=2)+'\n')
print(json.dumps(report,indent=2))
