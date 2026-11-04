import json,os,pathlib,subprocess,time
root=pathlib.Path(__file__).resolve().parent
launch=json.loads((root/'launch.json').read_text())
source=pathlib.Path(launch['source_checkout'])
start=time.time()
with (root/'prepare.log').open('w') as log:
 process=subprocess.Popen(['python3',str(source/'benches/retrieval_scale/harness.py'),'prepare','--hsum',str(root/'bin/hsum'),'--corpus',str(root/'corpus'),'--home',str(root/'home'),'--model-id','bge-small-en-v1-5-fp32','--output',str(root/'setup-macos-arm64.json')],cwd=source,env={**os.environ,'PYTHONDONTWRITEBYTECODE':'1'},stdout=log,stderr=subprocess.STDOUT)
 (root/'prepare.pid').write_text(str(process.pid)+'\n')
 result=process.wait()
(root/'prepare.exit').write_text(str(result)+'\n')
(root/'prepare-timing.json').write_text(json.dumps({'started_at_unix_seconds':start,'finished_at_unix_seconds':time.time(),'exit':result},indent=2)+'\n')
raise SystemExit(result)
