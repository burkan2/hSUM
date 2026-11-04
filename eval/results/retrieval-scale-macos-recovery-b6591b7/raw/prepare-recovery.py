import hashlib,json,pathlib,subprocess,sys,time,traceback
root=pathlib.Path(__file__).resolve().parent
sys.path.insert(0,str(root/'source'))
from benches.retrieval_scale import harness as scale
from benches.retrieval_stress import harness as stress
launch=json.loads((root/'recovery-launch.json').read_text())
binary=root/'bin/hsum';home=root/'home';corpus=pathlib.Path(launch['corpus'])
report={'candidate':launch['candidate'],'preparation_kind':launch['preparation_kind'],'original_fresh_candidate_ingest_throughput_available':False,'binary_sha256':scale.sha256_file(binary),'phases':{},'passed':False}
def save():scale.write_json_atomic(root/'recovery-report.json',report)
def monitored(name,argv):
 print('Starting',name,flush=True);report['phases'][name]=stress.run_monitored(argv,cwd=corpus,home=home);save();print('Completed',name,report['phases'][name],flush=True)
try:
 assert scale.git_revision()=={'commit':launch['candidate'],'working_tree_dirty':False}
 assert not (root/'setup-recovered-macos-arm64.json').exists()
 queries=scale.load_queries();manifest=scale.read_json(corpus/'.hsum-retrieval-scale-corpus.json')
 assert manifest['fingerprint_sha256']==scale.corpus_fingerprint(queries)
 save()
 common=[str(binary),'--no-color','--no-progress']
 report['status_before']=scale.run_json_command(common+['status','--json'],cwd=corpus,home=home);save()
 assert report['status_before']['active_documents']==10000 and report['status_before']['active_passages']==100000 and not report['status_before']['problems']
 monitored('full_doctor_before',common+['doctor','--integrity'])
 monitored('offline_model_verify',common+['model','verify','bge-small-en-v1-5-fp32'])
 scale.validate_lexical_query_paths(binary,queries,corpus=corpus,home=home)
 report['lexical_paths_validated']=19;save()
 monitored('cached_reembedding',common+['ingest','--reembed'])
 monitored('full_doctor_after',common+['doctor','--integrity'])
 packet=scale.run_json_command(scale.readiness_probe_arguments(binary,queries[-1]['query'],'hybrid'),cwd=corpus,home=home)
 assert packet['effective_mode']=='hybrid' and not packet['degraded_mode']
 report['hybrid_readiness']=packet;save()
 status=scale.run_json_command(common+['status','--json'],cwd=corpus,home=home)
 assert status['active_passages']==100000 and not status['problems']
 elapsed=report['phases']['cached_reembedding']['seconds']
 setup={'schema_version':scale.SETUP_SCHEMA_VERSION,'created_at_unix_seconds':time.time(),'hsum_version':scale.hsum_version(binary,cwd=corpus,home=home),'binary_sha256':scale.sha256_file(binary),'model_id':'bge-small-en-v1-5-fp32','index_name':'retrieval-scale-100k','project_name':'default','corpus':manifest,'query_manifest_sha256':scale.sha256_file(root/'source/benches/retrieval_scale/queries.json'),'ingest':{'seconds':None,'passages_per_second':None,'reason':'No fresh initialization/ingest was timed on this candidate; source index is a labeled recovery clone, so the frozen throughput gate must fail.'},'reembed':{'seconds':elapsed,'passages_per_second':100000/elapsed,'preparation_kind':'cached vector-membership recovery, not fresh inference'},'status':status,'recovery':{'candidate':launch['candidate'],'source_failed_candidate':launch['base_failure_candidate'],'launch_receipt_sha256':scale.sha256_file(root/'recovery-launch.json'),'original_failure_retained':True,'cached_inference_reused':True,'fresh_preparation_qualified':False}}
 scale.write_json_atomic(root/'setup-recovered-macos-arm64.json',setup)
 report['status_after']=status;report['passed']=True;report['fresh_preparation_qualified']=False;save()
 print('Recovery ready for frozen query protocol; fresh-ingest throughput intentionally unavailable and unqualified.',flush=True)
except Exception as error:
 report['error']=str(error);report['traceback']=traceback.format_exc();save();print(report['traceback'],flush=True);sys.exit(2)
