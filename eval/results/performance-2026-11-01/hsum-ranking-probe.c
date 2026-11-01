#include <sqlite3.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <assert.h>
#include <time.h>
typedef struct Fts5Bm25Data Fts5Bm25Data;
struct Fts5Bm25Data {
  int nPhrase;                    /* Number of phrases in query */
  double avgdl;                   /* Average number of tokens in each row */
  double *aIDF;                   /* IDF for each phrase */
  int *lens; int maxid;
  double *aFreq;                  /* Array used to calculate phrase freq. */
};

/*
** Callback used by fts5Bm25GetData() to count the number of rows in the
** table matched by each individual phrase within the query.
*/
static int fts5CountCb(
  const Fts5ExtensionApi *pApi,
  Fts5Context *pFts,
  void *pUserData                 /* Pointer to sqlite3_int64 variable */
){
  sqlite3_int64 *pn = (sqlite3_int64*)pUserData;
  (void)pApi; (void)pFts;
  (*pn)++;
  return SQLITE_OK;
}

/*
** Set *ppData to point to the Fts5Bm25Data object for the current query.
** If the object has not already been allocated, allocate and populate it
** now.
*/
static int fts5Bm25GetData(
  const Fts5ExtensionApi *pApi,
  Fts5Context *pFts,
  sqlite3 *db,
  Fts5Bm25Data **ppData           /* OUT: bm25-data object for this query */
){
  int rc = SQLITE_OK;             /* Return code */
  Fts5Bm25Data *p;                /* Object to return */

  p = (Fts5Bm25Data*)pApi->xGetAuxdata(pFts, 0);
  if( p==0 ){
    int nPhrase;                  /* Number of phrases in query */
    sqlite3_int64 nRow = 0;       /* Number of rows in table */
    sqlite3_int64 nToken = 0;     /* Number of tokens in table */
    sqlite3_int64 nByte;          /* Bytes of space to allocate */
    int i;

    /* Allocate the Fts5Bm25Data object */
    nPhrase = pApi->xPhraseCount(pFts);
    sqlite3_stmt *stmt=0; int maxid=0;
    if(sqlite3_prepare_v2(db,"SELECT max(id) FROM passages_fts_docsize",-1,&stmt,0)==SQLITE_OK){
      if(sqlite3_step(stmt)==SQLITE_ROW)maxid=sqlite3_column_int(stmt,0);
    }
    sqlite3_finalize(stmt);stmt=0;
    if(maxid<0 || maxid>2000000)maxid=0;
    nByte = ((sqlite3_int64)maxid+1)*sizeof(int)+sizeof(Fts5Bm25Data) + nPhrase*2*sizeof(double);
    p = (Fts5Bm25Data*)sqlite3_malloc64(nByte);
    if( p==0 ){
      rc = SQLITE_NOMEM;
    }else{
      memset(p, 0, (size_t)nByte);
      p->nPhrase = nPhrase;
      p->aIDF = (double*)&p[1];
      p->aFreq = &p->aIDF[nPhrase];
      p->maxid=maxid;p->lens=(int*)&p->aFreq[nPhrase];
      for(int k=0;k<=maxid;k++)p->lens[k]=-1;
      if(maxid && sqlite3_prepare_v2(db,"SELECT id,sz FROM passages_fts_docsize",-1,&stmt,0)==SQLITE_OK){
        while(sqlite3_step(stmt)==SQLITE_ROW){
          int id=sqlite3_column_int(stmt,0),n=sqlite3_column_bytes(stmt,1),total=0,v=0;
          const unsigned char *bytes=sqlite3_column_blob(stmt,1);
          for(int k=0;k<n;k++){v=(v<<7)+(bytes[k]&127);if(!(bytes[k]&128)){total+=v;v=0;}}
          if(id>=0 && id<=maxid)p->lens[id]=total;
        }
      }
      sqlite3_finalize(stmt);stmt=0;
    }

    /* Calculate the average document length for this FTS5 table */
    if( rc==SQLITE_OK ) rc = pApi->xRowCount(pFts, &nRow);
    assert( rc!=SQLITE_OK || nRow>0 );
    if( rc==SQLITE_OK ) rc = pApi->xColumnTotalSize(pFts, -1, &nToken);
    if( rc==SQLITE_OK ) p->avgdl = (double)nToken  / (double)nRow;

    /* Calculate an IDF for each phrase in the query */
    for(i=0; rc==SQLITE_OK && i<nPhrase; i++){
      sqlite3_int64 nHit = 0;
      rc = pApi->xQueryPhrase(pFts, i, (void*)&nHit, fts5CountCb);
      if( rc==SQLITE_OK ){
        /* Calculate the IDF (Inverse Document Frequency) for phrase i.
        ** This is done using the standard BM25 formula as found on wikipedia:
        **
        **   IDF = log( (N - nHit + 0.5) / (nHit + 0.5) )
        **
        ** where "N" is the total number of documents in the set and nHit
        ** is the number that contain at least one instance of the phrase
        ** under consideration.
        **
        ** The problem with this is that if (N < 2*nHit), the IDF is
        ** negative. Which is undesirable. So the minimum allowable IDF is
        ** (1e-6) - roughly the same as a term that appears in just over
        ** half of set of 5,000,000 documents.  */
        double idf = log( (nRow - nHit + 0.5) / (nHit + 0.5) );
        if( idf<=0.0 ) idf = 1e-6;
        p->aIDF[i] = idf;
      }
    }

    if( rc!=SQLITE_OK ){
      sqlite3_free(p);
    }else{
      rc = pApi->xSetAuxdata(pFts, p, sqlite3_free);
    }
    if( rc!=SQLITE_OK ) p = 0;
  }
  *ppData = p;
  return rc;
}

/*
** Implementation of bm25() function.
*/
static void fts5Bm25Function(
  const Fts5ExtensionApi *pApi,   /* API offered by current FTS version */
  Fts5Context *pFts,              /* First arg to pass to pApi functions */
  sqlite3_context *pCtx,          /* Context for returning result/error */
  int nVal,                       /* Number of values in apVal[] array */
  sqlite3_value **apVal           /* Array of trailing arguments */
){
  const double k1 = 1.2;          /* Constant "k1" from BM25 formula */
  const double b = 0.75;          /* Constant "b" from BM25 formula */
  int rc;                         /* Error code */
  double score = 0.0;             /* SQL function return value */
  Fts5Bm25Data *pData;            /* Values allocated/calculated once only */
  int i;                          /* Iterator variable */
  int nInst = 0;                  /* Value returned by xInstCount() */
  double D = 0.0;                 /* Total number of tokens in row */
  double *aFreq = 0;              /* Array of phrase freq. for current row */

  /* Calculate the phrase frequency (symbol "f(qi,D)" in the documentation)
  ** for each phrase in the query for the current row. */
  rc = fts5Bm25GetData(pApi, pFts, sqlite3_context_db_handle(pCtx), &pData);
  if( rc==SQLITE_OK ){
    aFreq = pData->aFreq;
    memset(aFreq, 0, sizeof(double) * pData->nPhrase);
  }
  for(i=0; rc==SQLITE_OK && i<pData->nPhrase; i++){
    Fts5PhraseIter iter;
    int col,off;
    rc = pApi->xPhraseFirst(pFts,i,&iter,&col,&off);
    for(; rc==SQLITE_OK && col>=0; pApi->xPhraseNext(pFts,&iter,&col,&off)){
      aFreq[i] += 1.0;
    }
  }

  /* Figure out the total size of the current row in tokens. */
  if( rc==SQLITE_OK ){
    int nTok;
    sqlite3_int64 id=pApi->xRowid(pFts);
    if(id>=0 && id<=pData->maxid && pData->lens[id]>=0)nTok=pData->lens[id];
    else rc = pApi->xColumnSize(pFts, -1, &nTok);
    D = (double)nTok;
  }

  /* Determine and return the BM25 score for the current row. Or, if an
  ** error has occurred, throw an exception. */
  if( rc==SQLITE_OK ){
    for(i=0; i<pData->nPhrase; i++){
      score += pData->aIDF[i] * (
          ( aFreq[i] * (k1 + 1.0) ) /
          ( aFreq[i] + k1 * (1 - b + b * D / pData->avgdl) )
      );
    }
    sqlite3_result_double(pCtx, -1.0 * score);
  }else{
    sqlite3_result_error_code(pCtx, rc);
  }
}


static double now(void){struct timespec t;clock_gettime(CLOCK_MONOTONIC,&t);return t.tv_sec+t.tv_nsec*1e-9;}
int main(int argc,char**argv){
 sqlite3 *db=0; sqlite3_stmt *st=0; fts5_api *api=0;
 if(sqlite3_open_v2(argv[1],&db,SQLITE_OPEN_READONLY,0))return 2;
 sqlite3_prepare_v2(db,"SELECT fts5(?1)",-1,&st,0);
 sqlite3_bind_pointer(st,1,&api,"fts5_api_ptr",0); sqlite3_step(st);sqlite3_finalize(st);
 if(!api)return 3;
 api->xCreateFunction(api,"probe_bm25",0,fts5Bm25Function,0);
 const char *q="immutable AND citation AND evidence";
 const char *sql[]={
"WITH candidates AS MATERIALIZED (SELECT rowid AS id,bm25(passages_fts) AS score FROM passages_fts WHERE passages_fts MATCH ?) SELECT ap.id,k.score FROM candidates AS k CROSS JOIN active_passages AS ap ON ap.id=k.id JOIN project_sources AS ps ON ps.source_id=ap.source_id AND ps.removed_at IS NULL JOIN document_heads AS dh ON dh.document_id=ap.document_id AND dh.document_version_id=ap.document_version_id AND dh.state='active' JOIN document_versions AS dv ON dv.id=ap.document_version_id AND dv.document_id=ap.document_id JOIN chunks AS c INDEXED BY chunks_order_idx ON c.id=ap.chunk_id WHERE ps.project_id=(SELECT id FROM projects LIMIT 1) ORDER BY k.score,ap.source_id,ap.document_id,c.start_byte LIMIT 500",
"WITH scores AS MATERIALIZED (SELECT rowid AS id,probe_bm25(passages_fts) AS score FROM passages_fts WHERE passages_fts MATCH ?), boundary AS (SELECT score FROM scores ORDER BY score LIMIT 1 OFFSET 499), candidates AS MATERIALIZED (SELECT * FROM scores WHERE score<=(SELECT score FROM boundary)) SELECT ap.id,k.score FROM candidates AS k CROSS JOIN active_passages AS ap ON ap.id=k.id JOIN project_sources AS ps ON ps.source_id=ap.source_id AND ps.removed_at IS NULL JOIN document_heads AS dh ON dh.document_id=ap.document_id AND dh.document_version_id=ap.document_version_id AND dh.state='active' JOIN document_versions AS dv ON dv.id=ap.document_version_id AND dv.document_id=ap.document_id JOIN chunks AS c INDEXED BY chunks_order_idx ON c.id=ap.chunk_id WHERE ps.project_id=(SELECT id FROM projects LIMIT 1) ORDER BY k.score,ap.source_id,ap.document_id,c.start_byte LIMIT 500",
"WITH candidates AS MATERIALIZED (SELECT rowid AS id,probe_bm25(passages_fts) AS score FROM passages_fts WHERE passages_fts MATCH ?) SELECT ap.id,k.score FROM candidates AS k CROSS JOIN active_passages AS ap ON ap.id=k.id JOIN project_sources AS ps ON ps.source_id=ap.source_id AND ps.removed_at IS NULL JOIN document_heads AS dh ON dh.document_id=ap.document_id AND dh.document_version_id=ap.document_version_id AND dh.state='active' JOIN document_versions AS dv ON dv.id=ap.document_version_id AND dv.document_id=ap.document_id JOIN chunks AS c INDEXED BY chunks_order_idx ON c.id=ap.chunk_id WHERE ps.project_id=(SELECT id FROM projects LIMIT 1) ORDER BY k.score,ap.source_id,ap.document_id,c.start_byte LIMIT 500" };
 for(int j=0;j<3;j++)for(int r=0;r<5;r++){
 double start=now();int n=0,mismatch=0,rc;
 if(sqlite3_prepare_v2(db,sql[j],-1,&st,0)){fprintf(stderr,"%s\n",sqlite3_errmsg(db));return 4;}
 sqlite3_bind_text(st,1,q,-1,SQLITE_STATIC);
 while((rc=sqlite3_step(st))==SQLITE_ROW){n++;if(r==0)fprintf(stderr,"%d %lld %.17g\n",j,sqlite3_column_int64(st,0),sqlite3_column_double(st,1));}
 sqlite3_finalize(st);printf("%d %d %.3f rows=%d mismatches=%d rc=%d\n",j,r,(now()-start)*1000,n,mismatch,rc);fflush(stdout);
 }
 sqlite3_close(db);return 0;
}
