import sys,pathlib,json,time,hashlib,subprocess
repo=pathlib.Path('/Users/b.k./Desktop/hSUM');sys.path.insert(0,str(repo))
from benches.retrieval_stress import harness as stress
from benches.retrieval_scale import harness as scale
base=pathlib.Path(__file__).resolve().parent
binaries={'before':pathlib.Path('/Users/b.k./.cache/hsum-qualification/2026-10-23-100k-lbse5n1f/bin/hsum'),'after':pathlib.Path('/Users/b.k./.cache/hsum-qualification/2026-10-23-q05-recovery/bin/hsum-3827248')}
output=repo/'eval/results/lexical-cursor-3827248/paired-product.json'
assert not output.exists()
report={'qualification':False,'fixture':'Reduced real-product 100,000 duplicate passages / 64 sources, no embeddings or cancellation. Concurrent 100k preparation; these are diagnostics.','before_candidate':'1ce11d00bf8cad949c12a9c496ab7dba3675717f','after_candidate':'38272481bfbc9bb831ac05304135f3bdc54dda05','binaries':{k:stress.sha256_file(v) for k,v in binaries.items()},'observations':[]}
reference={}
for repetition in range(3):
 for condition in (('before','after') if repetition%2==0 else ('after','before')):
  for limit in [10,50]:
   argv=[str(binaries[condition]),'--no-color','--no-progress','search',stress.load_manifest()['queries'][2]['query'],'--mode','lexical','--limit',str(limit),'--timeout-ms','10000','--explain','--json']
   start=time.perf_counter();result=scale.run_command(argv,cwd=base/'repository',home=base/'home',check=False)
   row={'condition':condition,'repetition':repetition,'limit':limit,'seconds':time.perf_counter()-start,'exit_code':result.returncode,'stdout':result.stdout,'stderr':result.stderr}
   report['observations'].append(row);output.write_text(json.dumps(report,indent=2)+'\n')
   assert result.returncode==0,result.stderr
   packet=json.loads(result.stdout);stress.validate_search_packet(stress.load_manifest()['queries'][2],packet)
   assert packet['examined']['lexical']==500 and packet['stop_reason']=='work_budget_exhausted'
   identity=[{k:v for k,v in entry.items() if k not in ['source_state']} for entry in packet['results']]
   if limit not in reference:reference[limit]=identity
   assert identity==reference[limit],(condition,limit)
   for item in packet['results']:
    span=item['span'];body=stress.shared_body()[span['start_byte']:span['end_byte']]
    assert body.decode()==item['content'] and hashlib.sha256(body).hexdigest()==item['content_sha256']
   row['packet']=packet;row['ordered_results_verified']=True
   output.write_text(json.dumps(report,indent=2)+'\n')
   print(condition,repetition,limit,row['seconds'],packet['timing_ms'],flush=True)
report['probe_source']=pathlib.Path(__file__).read_text();output.write_text(json.dumps(report,indent=2)+'\n')
