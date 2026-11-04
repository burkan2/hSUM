import sys, pathlib, json, time, hashlib, subprocess
sys.path.insert(0, '/Users/b.k./Desktop/hSUM')
from benches.retrieval_stress import harness as stress
from benches.retrieval_scale import harness as scale
base = pathlib.Path(__file__).resolve().parent
binary = pathlib.Path('/Users/b.k./.cache/hsum-qualification/2026-10-23-100k-lbse5n1f/bin/hsum')
root, home = base/'repository', base/'home'
root.mkdir(mode=0o700); (root/'.git').mkdir(); home.mkdir(mode=0o700)
manifest = stress.load_manifest(); manifest['document_count'] = 10000
paths, corpus = stress.create_snapshots(base/'snapshots', manifest)
common = [str(binary), '--no-color', '--no-progress']
stress.run_command(common+['init', str(root), '--index', stress.INDEX_NAME, '--project', 'default'], cwd=root, home=home)
for index, path in enumerate(paths):
    stress.run_command(common+['source','add','jsonl',str(path),'--name',stress.source_name(index)], cwd=root, home=home)
print('ingesting reduced lexical diagnostic: 10k documents, 100k passages, 64 sources; no model or cancellation', flush=True)
ingest = stress.run_monitored(common+['ingest','--strict'], cwd=root, home=home)
report = {'qualification':False,'candidate':'1ce11d00bf8cad949c12a9c496ab7dba3675717f','binary_sha256':stress.sha256_file(binary),'source_sha256':stress.sha256_file(pathlib.Path('/Users/b.k./Desktop/hSUM/src/search/retrieval.rs')),'corpus':corpus,'ingest':ingest,'status':stress.status_packet(binary,root,home),'observations':[]}
stress.validate_status(report['status'], sources=64, documents=10000, passages=100000)
for limit in (10,50):
    for repetition in range(3):
        arguments = common+['search',manifest['queries'][2]['query'],'--mode','lexical','--limit',str(limit),'--timeout-ms','10000','--explain','--json']
        started=time.perf_counter(); result=scale.run_command(arguments,cwd=root,home=home,check=False)
        observation={'limit':limit,'repetition':repetition,'seconds':time.perf_counter()-started,'exit':result.returncode,'stdout':result.stdout,'stderr':result.stderr}
        if result.returncode==0:
            packet=json.loads(result.stdout)
            for item in packet['results']:
                span=item['byte_span']; body=stress.shared_body()[span['start']:span['end']]
                assert body.decode()==item['content']
                assert hashlib.sha256(body).hexdigest()==item['content_sha256']
            observation['verified_passages']=len(packet['results'])
        report['observations'].append(observation)
        (base/'product.json').write_text(json.dumps(report,indent=2)+'\n')
        print('query',limit,repetition,observation['seconds'],result.returncode,flush=True)
