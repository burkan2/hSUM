import copy,datetime,hashlib,json,math,pathlib,subprocess,sys
repo=pathlib.Path('/Users/b.k./Desktop/hSUM');sys.path.insert(0,str(repo))
from benches.retrieval_scale import harness as h
root=pathlib.Path(__file__).resolve().parent
candidate='b6591b7405320c924dc341111006d8d87b1e0c8a'
read=lambda p:json.loads(p.read_text())
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
r=read(root/'raw/macos-arm64-100k-recovered.json');setup=read(root/'raw/setup-recovered-macos-arm64.json')
recovery=read(root/'raw/recovery-report.json');launch=read(root/'raw/recovery-launch.json')
source=lambda p:subprocess.check_output(['git','show',candidate+':'+p],cwd=repo)
assert r['git']=={'commit':candidate,'working_tree_dirty':False}
assert (root/'raw/measurement.exit').read_text().strip()=='1'
assert (root/'raw/recovery.exit').read_text().strip()=='0' and recovery['passed'] is True
assert recovery['candidate']==setup['recovery']['candidate']==launch['candidate']==candidate
assert setup['recovery']['source_failed_candidate']=='1ce11d00bf8cad949c12a9c496ab7dba3675717f'
assert setup['recovery']['fresh_preparation_qualified'] is False and setup['recovery']['cached_inference_reused'] is True
assert setup['recovery']['launch_receipt_sha256']==sha(root/'raw/recovery-launch.json')
assert setup['ingest']['seconds'] is None and setup['ingest']['passages_per_second'] is None
assert recovery['phases']['cached_reembedding']['seconds']==setup['reembed']['seconds']
assert 'Inputs reused: 100000' in recovery['phases']['cached_reembedding']['stdout_tail']
assert 'Inputs cached: 0' in recovery['phases']['cached_reembedding']['stdout_tail']
assert 'Index diagnosis passed.' in recovery['phases']['full_doctor_before']['stdout_tail']
assert 'Index diagnosis passed.' in recovery['phases']['full_doctor_after']['stdout_tail']
assert 'Model verification passed.' in recovery['phases']['offline_model_verify']['stdout_tail']
assert recovery['lexical_paths_validated']==19
binary=pathlib.Path('/Users/b.k./.cache/hsum-qualification/2026-10-24-macos-recovery-b6591b7/bin/hsum')
assert sha(binary)==r['hsum']['binary_sha256']==recovery['binary_sha256']
assert source('benches/retrieval_scale/harness.py')==(repo/'benches/retrieval_scale/harness.py').read_bytes()
queries=h.load_queries();query_bytes=source('benches/retrieval_scale/queries.json');query_digest=hashlib.sha256(query_bytes).hexdigest()
assert query_bytes==(repo/'benches/retrieval_scale/queries.json').read_bytes()
assert r['schema_version']==h.SCHEMA_VERSION and setup['schema_version']==h.SETUP_SCHEMA_VERSION
assert r['setup']==setup and r['corpus']==setup['corpus']
assert r['hsum']['binary_sha256']==setup['binary_sha256']
assert r['protocol']['query_manifest_sha256']==setup['query_manifest_sha256']==query_digest
assert r['corpus']['fingerprint_sha256']==h.corpus_fingerprint(queries)
assert r['corpus']['documents']==10000 and r['corpus']['expected_passages']==100000 and r['corpus']['bytes']==150000000
assert setup['model_id']=='bge-small-en-v1-5-fp32' and setup['index_name']=='retrieval-scale-100k' and setup['project_name']=='default'
assert setup['status']['active_documents']==10000 and setup['status']['active_passages']==100000 and setup['status']['problems']==[]
assert r['machine']['system']=='Darwin' and r['machine']['architecture']=='arm64' and r['machine']['cpu']=='Apple M2' and r['machine']['memory_bytes']==17179869184
helper_digest=hashlib.sha256(source('benches/retrieval_scale/drop-cache-macos.sh')).hexdigest()
assert r['protocol']['cold_cache_helper_sha256']==helper_digest
p=r['protocol'];assert (p['runs'],p['warmup_full_query_passes'],p['measured_full_query_passes'],p['queries_per_pass'],p['observations_per_run'],p['limit'])==(3,5,30,25,750,10)
assert p['percentile_method']=='nearest-rank ceil(p*n)' and p['fixed_query_order']==[q['id'] for q in queries]
assert h.CLASS_SLOS_MS=={'identifier':100.0,'ordinary_lexical':100.0,'exact_fallback':750.0,'hybrid_no_fallback':500.0}
all_obs=[]
for index,item in enumerate(r['runs'],1):
 assert item['run']==index and item['fresh_process'] is True
 assert item['warmup_passes']==5 and item['measured_passes']==30 and item['observation_count']==len(item['observations'])==750
 expected=[(i,q['id'],q['class']) for i in range(1,31) for q in queries]
 assert [(o['pass'],o['query_id'],o['class']) for o in item['observations']]==expected
 for o in item['observations']:
  assert 0<o['result_count']<=10 and o['stop_reason']=='limit_reached'
  assert all(isinstance(o['timing_ms'][stage],(int,float)) and math.isfinite(o['timing_ms'][stage]) and o['timing_ms'][stage]>=0 for stage in h.STAGES)
  assert o['client_round_trip_ms']>0 and set(o['examined'])=={'exact','exact_fallback','lexical','vector'}
  assert all(type(v) is int and 0<=v<=500 for v in o['examined'].values())
  if o['class']=='exact_fallback':assert 'exact_fallback' in o['retrievers']
  if o['class']=='hybrid_no_fallback':assert 'vector' in o['retrievers'] and 'exact_fallback' not in o['retrievers']
 assert h.summarize_observations(item['observations'])==item['summary']
 assert h.evaluate_class_slos(item['summary'])==item['class_slos']
 assert item['peak_process_tree_rss_bytes']>0 and item['rss_sampling_error'] is None
 assert item['cold']['cache_eviction'] is not None and item['cold']['cache_eviction']['elapsed_ms']>0
 assert item['cold']['os_cache_lexical']['query_id']==next(q['id'] for q in queries if q['class']=='ordinary_lexical')
 for cold_name in ['os_cache_lexical','first_model_load','first_hybrid']:
  assert item['cold'][cold_name]['client_round_trip_ms']>0
  assert all(math.isfinite(v) and v>=0 for v in item['cold'][cold_name]['timing_ms'].values())
 assert item['cold']['process_start_ms']>0
 all_obs+=item['observations']
assert len(all_obs)==2250
recomputed=h.qualify_report(r['runs'],canonical_parameters=True,setup=setup,cold_cache_available=True)
assert recomputed==r['qualification'] and recomputed['pass'] is False
assert {k for k,v in recomputed['checks'].items() if not v}=={'all_class_slos','total_p95_cv_at_most_10_percent','setup_and_ingest_throughput_recorded'}
st=r['storage'];assert st['managed_index_bytes']==sum(st['database_files'].values())
assert (st['logical_live_bytes'],st['logical_history_bytes'],st['cached_vector_rows'])==(150000000,0,100000)
assert st['logical_active_vector_payload_bytes']==st['logical_cached_vector_payload_bytes']==100000*384*4
assert st['logical_total_vector_payload_bytes']==307200000
for key in ['reembed']:
 assert math.isclose(setup[key]['passages_per_second'],100000/setup[key]['seconds'])
by_query={}
for query in queries:
 obs=[o for o in all_obs if o['query_id']==query['id']]
 assert len(obs)==90
 by_query[query['id']]={'class':query['class'],'timing_ms':{stage:h.distribution(o['timing_ms'][stage] for o in obs) for stage in h.STAGES},'client_round_trip_ms':h.distribution(o['client_round_trip_ms'] for o in obs),'max_examined':h.max_examined(obs),'unassigned_wall_ms':h.distribution(o['timing_ms']['total']-sum(o['timing_ms'][k] for k in ['query_embedding','exact','lexical','vector','fusion']) for o in obs)}
trend=[]
for run in r['runs']:
 row={'run':run['run'],'measured_pass_blocks':[]}
 for start in [1,11,21]:
  obs=[o for o in run['observations'] if start<=o['pass']<start+10]
  assert len(obs)==250
  row['measured_pass_blocks'].append({'first_pass':start,'last_pass':start+9,'server_total_ms':h.distribution(o['timing_ms']['total'] for o in obs),'client_ms':h.distribution(o['client_round_trip_ms'] for o in obs),'stage_ms':{stage:h.distribution(o['timing_ms'][stage] for o in obs) for stage in h.STAGES}})
 trend.append(row)
receipt={'reviewed_at_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'candidate':candidate,'preparation_kind':'Recovered vector membership using all 100,000 cached embeddings; no fresh-candidate ingest/inference throughput','recomputed_qualification':recomputed,'observations_reviewed':2250,'per_run_class_slos':[x['class_slos'] for x in r['runs']],'per_query':by_query,'trend':trend,'peak_process_tree_rss_bytes':[x['peak_process_tree_rss_bytes'] for x in r['runs']],'cold':[x['cold'] for x in r['runs']],'storage':st,'checks':{'exact_clean_source_candidate_and_live_binary_hash':True,'recovered_setup_and_launch_hash_bound':True,'fresh_throughput_missing_and_qualification_false':True,'frozen_protocol_manifest_corpus_and_helper_hashes':True,'all_2250_query_order_pass_counts_and_class_paths':True,'all_raw_stage_numbers_finite_and_candidate_counts_bounded':True,'all_summaries_slos_and_cv_recomputed_exactly':True,'cold_measurements_separate_and_eviction_recorded':True,'rss_recorded_without_sampler_error':True,'logical_and_physical_storage_consistent':True},'limits':['Performance observation and explicit cached recovery only; no full fresh-preparation qualification.','Different candidate from Linux baseline and Linux stress; no same-candidate platform or causal before/after performance comparison.','No CPU frequency, thermal, background-process or page-fault telemetry was collected, so timing trends cannot prove environmental causes.','Full response bytes are not retained by compact harness observations; the existing field/mode/result integrity checks were enforced during execution, while raw model/index checks are bound to the recorded Doctor and model verification.','Stage timers overlap for body materialization and exact fallback; the unassigned remainder is not attribution to a measured operation.']}
(root/'review.json').write_text(json.dumps(receipt,indent=2)+'\n')
print(json.dumps({'qualification':recomputed,'rss':receipt['peak_process_tree_rss_bytes'],'trend':trend,'checks':receipt['checks']},indent=2))
