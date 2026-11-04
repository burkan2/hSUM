import hashlib,json,pathlib,subprocess,sys,time
repo=pathlib.Path('/Users/b.k./Desktop/hSUM');sys.path.insert(0,str(repo))
from benches.retrieval_scale import harness as h
root=pathlib.Path(__file__).resolve().parent
candidate='b6591b7405320c924dc341111006d8d87b1e0c8a'
base=pathlib.Path('/Users/b.k./.cache/hsum-qualification/2026-10-24-macos-recovery-b6591b7')
binary=base/'bin/hsum';corpus=pathlib.Path(json.loads((base/'recovery-launch.json').read_text())['corpus'])
queries=[q for q in h.load_queries() if q['id'] in ['identifier-01','ordinary-02']]
assert len(queries)==2
assert h.sha256_file(binary)==json.loads((base/'setup-recovered-macos-arm64.json').read_text())['binary_sha256']
assert subprocess.check_output(['git','rev-parse','HEAD'],cwd=base/'source',text=True).strip()==candidate
session=h.McpSession(binary,corpus,root/'home');observations=[];sampler=None
try:
 for _ in range(5):
  for q in queries:
   packet,_=session.search(q);h.validate_search_packet(q,packet)
 sampler=subprocess.Popen(['/usr/bin/sample',str(session.process.pid),'8','10','-file',str(root/'sample.txt')],stdout=(root/'sample.stdout').open('w'),stderr=(root/'sample.stderr').open('w'))
 for i in range(1,61):
  for q in queries:
   packet,ms=session.search(q);h.validate_search_packet(q,packet)
   observations.append(h.compact_observation(q,packet,ms,i))
finally:
 rss,error=session.close()
 if sampler is not None:sampler.wait(timeout=20)
assert len(observations)==120 and error is None and sampler.returncode==0
report={'candidate':candidate,'binary_sha256':h.sha256_file(binary),'scope':'Reduced Mac native CPU sampling on a disposable APFS-cloned recovered 100k index; two lexical queries, five warmups, 60 repetitions, no cold eviction. Diagnostic only; not Linux evidence or frozen qualification. Sampling can perturb latency.','observations':observations,'summary':h.summarize_observations(observations),'peak_process_tree_rss_bytes':rss,'sample_exit':sampler.returncode,'sample_sha256':h.sha256_file(root/'sample.txt'),'source_sha256':{p:hashlib.sha256(subprocess.check_output(['git','show',candidate+':'+p],cwd=repo)).hexdigest() for p in ['src/store/doctor.rs','src/store/open.rs','src/app/search_evidence.rs']}}
(root/'profile.json').write_text(json.dumps(report,indent=2)+'\n')
print(json.dumps({'observations':len(observations),'sample_exit':sampler.returncode,'summary':report['summary']},indent=2))
