import hashlib,json,pathlib,subprocess,sys
repo=pathlib.Path('/Users/b.k./Desktop/hSUM');sys.path.insert(0,str(repo))
from benches.retrieval_scale import harness as h
root=pathlib.Path(__file__).resolve().parent
base=pathlib.Path('/Users/b.k./.cache/hsum-qualification/2026-10-24-macos-recovery-b6591b7')
corpus=pathlib.Path(json.loads((base/'recovery-launch.json').read_text())['corpus'])
queries=[q for q in h.load_queries() if q['id'] in ['identifier-01','ordinary-02']]
reports=[]; expected={}
for name,binary in [('before',base/'bin/hsum'),('request-connection',repo/'target/release/hsum'),('before-control',base/'bin/hsum')]:
 session=h.McpSession(binary,corpus,root/'home');obs=[]
 try:
  for i in range(-5,20):
   for q in queries:
    packet,ms=session.search(q);h.validate_search_packet(q,packet)
    signature=[{k:v for k,v in r.items() if k not in ['source_state']} for r in packet['results']]
    key=q['id']
    if key not in expected: expected[key]=signature
    assert expected[key]==signature,(name,key,'results changed')
    if i>=0:obs.append(h.compact_observation(q,packet,ms,i+1))
 finally:rss,error=session.close()
 report={'phase':name,'binary_sha256':h.sha256_file(binary),'observations':obs,'peak_process_tree_rss_bytes':rss,'sampler_error':error,'summary':{q['id']:{'server_total_ms':h.distribution(o['timing_ms']['total'] for o in obs if o['query_id']==q['id']),'lexical_ms':h.distribution(o['timing_ms']['lexical'] for o in obs if o['query_id']==q['id'])} for q in queries}}
 reports.append(report);print(json.dumps({'phase':name,'summary':report['summary']}),flush=True)
(root/'connection-ab.json').write_text(json.dumps({'qualification':False,'scope':'Reduced A/B/A on disposable recovered 100k clone, five warmups and twenty passes of two queries; all results including explanations equal; no cold eviction or fresh inference. Not native qualification.','base_commit':subprocess.check_output(['git','rev-parse','HEAD'],cwd=repo,text=True).strip(),'source_sha256':h.sha256_file(repo/'src/app/search_evidence.rs'),'reports':reports},indent=2)+'\n')
