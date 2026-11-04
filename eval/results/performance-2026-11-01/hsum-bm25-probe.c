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
    nByte = sizeof(Fts5Bm25Data) + nPhrase*2*sizeof(double);
    p = (Fts5Bm25Data*)sqlite3_malloc64(nByte);
    if( p==0 ){
      rc = SQLITE_NOMEM;
    }else{
      memset(p, 0, (size_t)nByte);
      p->nPhrase = nPhrase;
      p->aIDF = (double*)&p[1];
      p->aFreq = &p->aIDF[nPhrase];
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
  rc = fts5Bm25GetData(pApi, pFts, &pData);
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
    rc = pApi->xColumnSize(pFts, -1, &nTok);
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
 "SELECT rowid,bm25(passages_fts) AS score FROM passages_fts WHERE passages_fts MATCH ? ORDER BY score LIMIT 500",
 "SELECT rowid,probe_bm25(passages_fts) AS score FROM passages_fts WHERE passages_fts MATCH ? ORDER BY score LIMIT 500",
 "SELECT rowid,bm25(passages_fts),probe_bm25(passages_fts) FROM passages_fts WHERE passages_fts MATCH ?"
 };
 for(int j=0;j<3;j++)for(int r=0;r<5;r++){
 double start=now();int n=0,mismatch=0,rc;
 if(sqlite3_prepare_v2(db,sql[j],-1,&st,0)){fprintf(stderr,"%s\n",sqlite3_errmsg(db));return 4;}
 sqlite3_bind_text(st,1,q,-1,SQLITE_STATIC);
 while((rc=sqlite3_step(st))==SQLITE_ROW){n++;if(j==2){double a=sqlite3_column_double(st,1),b=sqlite3_column_double(st,2);if(memcmp(&a,&b,sizeof(a)))mismatch++;}}
 sqlite3_finalize(st);printf("%d %d %.3f rows=%d mismatches=%d rc=%d\n",j,r,(now()-start)*1000,n,mismatch,rc);fflush(stdout);
 }
 sqlite3_close(db);return 0;
}
