"""Diagnostic stack sampling of ordinary-02; never qualification."""
import json,pathlib,subprocess,sys,time
repo=pathlib.Path('/Users/b.k./Desktop/hSUM');sys.path.insert(0,str(repo))
from benches.retrieval_scale import harness as h
out=pathlib.Path(__file__).resolve().parent
fixture=pathlib.Path('/Users/b.k./.cache/hsum-qualification/2026-10-31-performance')
recovery=pathlib.Path('/Users/b.k./.cache/hsum-qualification/2026-10-24-macos-recovery-b6591b7')
corpus=pathlib.Path(json.loads((recovery/'recovery-launch.json').read_text())['corpus'])
binary=pathlib.Path('/Users/b.k./.cache/hsum-qualification/2026-11-02-5ca552c-macos-fresh/bin/hsum')
query=next(q for q in h.load_queries() if q['id']=='ordinary-02')
session=h.McpSession(binary,corpus,fixture/'home');calls=[];sampler=None
try:
 for _ in range(3):session.search(query)
 sampler=subprocess.Popen(['/usr/bin/sample',str(session.process.pid),'10','1','-file',str(out/'ordinary-02-sample.txt')],stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True)
 deadline=time.monotonic()+12
 while time.monotonic()<deadline:
  packet,ms=session.search(query);h.validate_search_packet(query,packet)
  calls.append({'timing_ms':packet['timing_ms'],'client_ms':ms})
 output=sampler.communicate(timeout=20)[0];(out/'sample-command.log').write_text(output)
 if sampler.returncode:raise RuntimeError(output)
finally:
 if sampler is not None and sampler.poll() is None:sampler.terminate();sampler.communicate()
 rss,error=session.close()
(out/'receipt.json').write_text(json.dumps({'qualification':False,'scope':'12-second repeated ordinary-02 stack sample on disposable existing index; timings are perturbed by profiling','binary_sha256':h.sha256_file(binary),'query':query,'calls':calls,'peak_rss_kib':rss,'rss_error':error},indent=2)+'\n')
print('profile captured',len(calls),'calls')
