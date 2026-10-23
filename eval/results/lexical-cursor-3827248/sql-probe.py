import ctypes as c, pathlib, json, sqlite3, time, hashlib, re, platform
base=pathlib.Path(__file__).resolve().parent
library=pathlib.Path('/var/folders/_1/t15r_skj51qcmkjy9bfz6z3h0000gn/T/hsum-q04-probe-igo4nljw/libsqlite-probe.dylib')
lib=c.CDLL(str(library))
for name,args in [('open_v2',[c.c_char_p,c.POINTER(c.c_void_p),c.c_int,c.c_char_p]),('prepare_v2',[c.c_void_p,c.c_char_p,c.c_int,c.POINTER(c.c_void_p),c.c_void_p]),('step',[c.c_void_p]),('column_count',[c.c_void_p]),('column_bytes',[c.c_void_p,c.c_int]),('column_blob',[c.c_void_p,c.c_int]),('finalize',[c.c_void_p]),('close',[c.c_void_p]),('stmt_status',[c.c_void_p,c.c_int,c.c_int])]:
    getattr(lib,'sqlite3_'+name).argtypes=args
lib.sqlite3_column_blob.restype=c.c_void_p
lib.sqlite3_libversion.restype=c.c_char_p
lib.sqlite3_errmsg.argtypes=[c.c_void_p];lib.sqlite3_errmsg.restype=c.c_char_p
path=base/'home/data/indexes/retrieval-stress-1m/index.sqlite'
conn=sqlite3.connect(f'file:{path}?mode=ro',uri=True);project=conn.execute('SELECT hex(id) FROM projects').fetchone()[0];conn.close()
raw=json.loads(pathlib.Path('/Users/b.k./.cache/hsum-qualification/2026-10-21-WkRefX/lexical-budget-probe-2026-10-23.json').read_text())['observations'][0]['sql']
raw=re.sub(r"x'[A-F0-9]{32}'",f"x'{project}'",raw).replace('"immutable" AND "citation" AND "evidence"','"duplicate" AND "bounded" AND "evidence" AND "archive"')
raw=re.sub(r'LIMIT 50 OFFSET 0', 'LIMIT {limit} OFFSET {offset}',raw)
assert '{limit}' in raw
report={'qualification':False,'candidate':'1ce11d00bf8cad949c12a9c496ab7dba3675717f','fixture':'100,000 real product lexical passages, 64 sources; no cancellation or embeddings','sqlite_version':lib.sqlite3_libversion().decode(),'library':str(library),'library_sha256':hashlib.sha256(library.read_bytes()).hexdigest(),'platform':platform.platform(),'sql_template':raw,'observations':[]}
db=c.c_void_p();assert lib.sqlite3_open_v2(str(path).encode(),c.byref(db),1,None)==0

def run(limit,offset):
    sql=raw.format(limit=limit,offset=offset);stmt=c.c_void_p()
    started=time.perf_counter();assert lib.sqlite3_prepare_v2(db,sql.encode(),-1,c.byref(stmt),None)==0,lib.sqlite3_errmsg(db)
    rows=[]
    while (code:=lib.sqlite3_step(stmt))==100:
        row=[]
        for i in range(lib.sqlite3_column_count(stmt)):
            ptr=lib.sqlite3_column_blob(stmt,i); size=lib.sqlite3_column_bytes(stmt,i)
            row.append(c.string_at(ptr,size).hex() if ptr else None)
        rows.append(row)
    assert code==101,lib.sqlite3_errmsg(db)
    result={'seconds':time.perf_counter()-started,'vm_steps':lib.sqlite3_stmt_status(stmt,4,0),'sort_operations':lib.sqlite3_stmt_status(stmt,2,0),'row_count':len(rows),'row_sha256':[hashlib.sha256(json.dumps(row,separators=(',',':')).encode()).hexdigest() for row in rows]}
    assert lib.sqlite3_finalize(stmt)==0
    return result
for repetition in range(3):
    for name in (('paged','one_cursor') if repetition%2==0 else ('one_cursor','paged')):
        results=[run(50,offset) for offset in range(0,500,50)] if name=='paged' else [run(500,0)]
        item={'name':name,'repetition':repetition,'seconds':sum(r['seconds'] for r in results),'vm_steps':sum(r['vm_steps'] for r in results),'sort_operations':sum(r['sort_operations'] for r in results),'rows_sha256':[h for r in results for h in r['row_sha256']],'pages':results}
        assert len(item['rows_sha256'])==500
        if report['observations']:assert item['rows_sha256']==report['observations'][0]['rows_sha256']
        report['observations'].append(item)
        (base/'sql.json').write_text(json.dumps(report,indent=2)+'\n')
        print(name,repetition,item['seconds'],item['vm_steps'],item['sort_operations'],flush=True)
assert lib.sqlite3_close(db)==0
