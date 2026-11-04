import hashlib,json,pathlib,subprocess,sys,time
repo=pathlib.Path('/Users/b.k./Desktop/hSUM');sys.path.insert(0,str(repo))
from benches.retrieval_scale.harness import command_environment,readiness_probe_arguments
from benches.retrieval_stress.harness import run_cancel_storm,load_manifest
root=pathlib.Path('/Users/b.k./.cache/hsum-qualification/2026-10-21-WkRefX')
binaries={'before':pathlib.Path('/Users/b.k./.cache/hsum-qualification/2026-10-23-100k-lbse5n1f/bin/hsum'),'after':pathlib.Path('/Users/b.k./.cache/hsum-qualification/2026-10-23-q05-recovery/bin/hsum-3827248')}
output=repo/'eval/results/lexical-cursor-3827248/after-3827248/small-model-controls.json'
assert not output.exists()
report={'qualification':False,'before_candidate':'1ce11d00bf8cad949c12a9c496ab7dba3675717f','after_candidate':'38272481bfbc9bb831ac05304135f3bdc54dda05'}
report['binaries']={name:{'path':str(path),'sha256':hashlib.sha256(path.read_bytes()).hexdigest()} for name,path in binaries.items()}
report['cli_controls']={'scope':'Small real-model controls, not latency qualification. Full ordered citations/scores/hashes compared, and every content checked against the live source byte window.','observations':[]}
reference={}
for repetition in range(3):
 for condition in (('before','after') if repetition%2==0 else ('after','before')):
  for mode in ['lexical','semantic','hybrid']:
   argv=readiness_probe_arguments(binaries[condition],'duplicate bounded evidence archive',mode)+['--explain']
   start=time.perf_counter()
   result=subprocess.run(argv,cwd=root/'cancel-smoke',env=command_environment(root/'home'),text=True,capture_output=True,timeout=40)
   elapsed=time.perf_counter()-start
   assert result.returncode==0,result.stderr
   packet=json.loads(result.stdout)
   identity=[(r['citation_uri'],r['score'],r['content_sha256']) for r in packet['results']]
   assert identity,packet
   if mode not in reference:reference[mode]=identity
   assert identity==reference[mode],(condition,mode)
   for row in packet['results']:
    assert row['source_uri']=='repo://evidence.md',row['source_uri']
    content=row['content'].encode();span=row['span']
    body=(root/'cancel-smoke/evidence.md').read_bytes()
    assert body[span['start_byte']:span['end_byte']]==content
    assert hashlib.sha256(content).hexdigest()==row['content_sha256']
   report['cli_controls']['observations'].append({'condition':condition,'repetition':repetition,'mode':mode,'argv':argv,'seconds':elapsed,'packet':packet,'stderr':result.stderr})
   output.write_text(json.dumps(report,indent=2)+'\n')
   print(condition,repetition,mode,len(identity),flush=True)
start=time.perf_counter()
report['cancellation_smoke']=run_cancel_storm(binaries['after'],root/'cancel-smoke',root/'home',load_manifest())
report['cancellation_smoke']['seconds']=time.perf_counter()-start
report['cancellation_smoke']['scope']='Small real-model fixture; not million-passage qualification.'
report['controls_source']=pathlib.Path(__file__).read_text()
output.write_text(json.dumps(report,indent=2)+'\n')
print(json.dumps(report['cancellation_smoke']),flush=True)
