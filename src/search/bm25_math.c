/* Arithmetic from SQLite's public-domain ext/fts5/fts5_aux.c.
 * Keep expression order and C floating-point behavior identical to bm25().
 * In particular, Rust's default FP contraction differs on Apple Silicon.
 * No pointers, allocation, or database access cross this boundary. */
#include <math.h>
#include <stdint.h>

double hsum_bm25_idf(int64_t rows, int64_t hits) {
    double idf = log((rows - hits + 0.5) / (hits + 0.5));
    return idf <= 0.0 ? 1e-6 : idf;
}

double hsum_bm25_accumulate(double score, double idf, double frequency,
                           double length, double average_length) {
    const double k1 = 1.2;
    const double b = 0.75;
    score += idf * ((frequency * (k1 + 1.0)) /
                   (frequency + k1 * (1 - b + b * length / average_length)));
    return score;
}
