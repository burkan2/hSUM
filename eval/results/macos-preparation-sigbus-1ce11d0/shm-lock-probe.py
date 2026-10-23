import ctypes as c,fcntl,hashlib,json,os,pathlib,subprocess,sys,tempfile,time
library=pathlib.Path('/var/folders/_1/t15r_skj51qcmkjy9bfz6z3h0000gn/T/hsum-q04-probe-igo4nljw/libsqlite-probe.dylib')
lib=c.CDLL(str(library));lib.sqlite3_open_v2.argtypes=[c.c_char_p,c.POINTER(c.c_void_p),c.c_int,c.c_char_p];lib.sqlite3_close.argtypes=[c.c_void_p]
cb=c.CFUNCTYPE(c.c_int,c.c_void_p,c.c_int,c.POINTER(c.c_char_p),c.POINTER(c.c_char_p));lib.sqlite3_exec.argtypes=[c.c_void_p,c.c_char_p,cb,c.c_void_p,c.POINTER(c.c_char_p)]
lib.sqlite3_libversion.restype=c.c_char_p
libc=c.CDLL(None,use_errno=True)
class Flock(c.Structure):
 _fields_=[('start',c.c_longlong),('length',c.c_longlong),('pid',c.c_int),('type',c.c_short),('whence',c.c_short)]
if len(sys.argv)>1:
 fd=os.open(sys.argv[1],os.O_RDONLY);lock=Flock(128,1,0,fcntl.F_WRLCK,os.SEEK_SET)
 lock=Flock.from_buffer_copy(fcntl.fcntl(fd,fcntl.F_GETLK,bytes(lock)))
 print(json.dumps({'type':lock.type,'pid':lock.pid,'shared_lock':lock.type==fcntl.F_RDLCK,'unlocked':lock.type==fcntl.F_UNLCK}));os.close(fd);sys.exit(0)
root=pathlib.Path(__file__).resolve().parent
report={'qualification':False,'scope':'Small isolated native SQLite 3.51.3 WAL lock diagnostic, no product index mutation','library_sha256':hashlib.sha256(library.read_bytes()).hexdigest(),'sqlite_version':lib.sqlite3_libversion().decode(),'observations':[]}
with tempfile.TemporaryDirectory(prefix='hsum-shm-lock-',dir=root) as tmp:
 path=pathlib.Path(tmp)/'probe.sqlite';db=c.c_void_p();assert lib.sqlite3_open_v2(str(path).encode(),c.byref(db),6,None)==0
 def sql(text):
  error=c.c_char_p();code=lib.sqlite3_exec(db,text.encode(),cb(),None,c.byref(error));assert code==0,(code,error.value)
 sql('PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE x(body BLOB); INSERT INTO x VALUES(zeroblob(20000000));')
 shm=pathlib.Path(str(path)+'-shm')
 def observe(label):
  lock=json.loads(subprocess.check_output([sys.executable,__file__,str(shm)],text=True));report['observations'].append({'label':label,'shm_bytes':shm.stat().st_size,**lock});print(label,report['observations'][-1],flush=True);return lock
 before=observe('native connection holds WAL DMS lock');assert before['shared_lock']
 fd=os.open(shm,os.O_RDONLY|os.O_NOFOLLOW);os.fstat(fd);os.close(fd)
 after=observe('after external validation open/fstat/close of same SHM inode');assert after['unlocked']
 # A new process now considers itself the first reader and rebuilds a truncated SHM.
 child="import sqlite3,sys; c=sqlite3.connect('file:'+sys.argv[1]+'?mode=ro',uri=True); print(c.execute('select count(*) from x').fetchone()[0]); c.close()"
 result=subprocess.run([sys.executable,'-c',child,str(path)],capture_output=True,text=True)
 report['other_reader']={'exit':result.returncode,'stdout':result.stdout,'stderr':result.stderr,'shm_bytes_after':shm.stat().st_size};assert result.returncode==0
 # Do not write through a mapping after the proven lock loss; avoid a deliberate SIGBUS.
 assert lib.sqlite3_close(db)==0
report['cause']='Closing a non-SQLite descriptor for the WAL SHM inode releases that process\'s POSIX record locks, including SQLite\'s shared DMS lock. Another process can then treat the shared mapping as unowned and truncate/rebuild it.'
(root/'shm-lock-probe.json').write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(report,indent=2))
