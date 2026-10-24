import pathlib, json, ctypes as c, time, hashlib
base=pathlib.Path(__file__).resolve().parent
# Reuse the exact native wrapper from the reduced real-index SQL probe.
source=(base/'sql-probe.py').read_text()
prefix=source.split('path=base/',1)[0]
exec(prefix)
path=base/'synthetic-million.sqlite'
db=c.c_void_p();assert lib.sqlite3_open_v2(str(path).encode(),c.byref(db),2|4,None)==0
sql_source=json.loads((base/'sql.json').read_text())['sql_template']
rank=sql_source.split('WITH ranked AS MATERIALIZED (',1)[1].split('\n         )',1)[0]
# This short-body fixture isolates ranking/join repetition; it is not a product index.
rank=rank.replace("x'"+rank.split("x'",1)[1].split("'",1)[0]+"'", "x'00000000000000000000000000000001'")
callback=c.CFUNCTYPE(c.c_int,c.c_void_p,c.c_int,c.POINTER(c.c_char_p),c.POINTER(c.c_char_p))
lib.sqlite3_exec.argtypes=[c.c_void_p,c.c_char_p,callback,c.c_void_p,c.POINTER(c.c_char_p)]
report={'qualification':False,'fixture':'Synthetic one-million-row short-body FTS5 ranking fixture, 64 sources, 100k document heads, 10 offsets each; no payload validation, vectors, product orchestration, cancellation, or cold-cache qualification','sqlite_version':lib.sqlite3_libversion().decode(),'native_library_sha256':hashlib.sha256(library.read_bytes()).hexdigest(),'ranking_sql_template':rank,'observations':[]}
statements=[
'PRAGMA journal_mode=OFF','PRAGMA synchronous=OFF',
'CREATE TABLE active_passages(id INTEGER PRIMARY KEY,source_id BLOB,document_id BLOB,document_version_id BLOB,chunk_id INTEGER)',
'CREATE TABLE project_sources(project_id BLOB,source_id BLOB,removed_at TEXT,PRIMARY KEY(project_id,source_id)) WITHOUT ROWID',
'CREATE TABLE document_heads(document_id BLOB PRIMARY KEY,document_version_id BLOB,state TEXT) WITHOUT ROWID',
'CREATE TABLE document_versions(id BLOB PRIMARY KEY,document_id BLOB) WITHOUT ROWID',
'CREATE TABLE chunks(id INTEGER PRIMARY KEY,start_byte INTEGER)',
'CREATE INDEX chunks_order_idx ON chunks(id,start_byte)',
'CREATE VIRTUAL TABLE passages_fts USING fts5(body)',
"WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<64) INSERT INTO project_sources SELECT unhex(printf('%032x',1)),unhex(printf('%032x',i)),NULL FROM n",
"WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<100000) INSERT INTO document_heads SELECT unhex(printf('%032x',i)),unhex(printf('%032x',i)),'active' FROM n",
'INSERT INTO document_versions SELECT document_id,document_id FROM document_heads',
'WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<10) INSERT INTO chunks SELECT i,(i-1)*1440 FROM n',
"INSERT INTO active_passages SELECT (row_number() OVER (ORDER BY dh.document_id)-1)*10+c.id,unhex(printf('%032x',1+(row_number() OVER (ORDER BY dh.document_id)-1)/10%64)),dh.document_id,dh.document_version_id,c.id FROM document_heads dh CROSS JOIN chunks c",
"INSERT INTO passages_fts(rowid,body) SELECT id,'duplicate bounded evidence archive' FROM active_passages",
]
# Populate IDs directly; window row numbering above remains unique across the million rows.
for sql in statements:
    err=c.c_char_p();started=time.perf_counter()
    code=lib.sqlite3_exec(db,sql.encode(),callback(),None,c.byref(err));assert code==0,(code,err.value)
    print('setup',sql[:65],round(time.perf_counter()-started,3),flush=True)
report['setup_sql']=statements
# Use the same stepping, byte hashing, and statement-status assertions.
run_source=source.split('def run(limit,offset):',1)[1].split('for repetition in range(3):',1)[0]
raw=rank
exec('def run(limit,offset):'+run_source)
for repetition in range(2):
    for name in (('paged','one_cursor') if repetition%2==0 else ('one_cursor','paged')):
        pages=[run(50,offset) for offset in range(0,500,50)] if name=='paged' else [run(500,0)]
        item={'name':name,'repetition':repetition,'seconds':sum(p['seconds'] for p in pages),'vm_steps':sum(p['vm_steps'] for p in pages),'sort_operations':sum(p['sort_operations'] for p in pages),'rows_sha256':[h for p in pages for h in p['row_sha256']],'pages':pages}
        assert len(item['rows_sha256'])==500
        if report['observations']: assert item['rows_sha256']==report['observations'][0]['rows_sha256']
        report['observations'].append(item)
        (base/'million-sql.json').write_text(json.dumps(report,indent=2)+'\n')
        print(name,repetition,item['seconds'],item['vm_steps'],item['sort_operations'],flush=True)
assert lib.sqlite3_close(db)==0
