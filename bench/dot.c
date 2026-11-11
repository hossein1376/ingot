#include <stdio.h>
#include <stdlib.h>
/* Plain C; clang -O2 auto-vectorizes this loop with NEON (with -ffast-math-free
   reassociation disabled, so it keeps a scalar order unless written explicitly). */
#include <arm_neon.h>
static float dot(const float *a, const float *b, long n) {
    float32x4_t a0 = vdupq_n_f32(0), a1 = a0, a2 = a0, a3 = a0;
    for (long i = 0; i < n; i += 16) {
        a0 = vfmaq_f32(a0, vld1q_f32(a + i), vld1q_f32(b + i));
        a1 = vfmaq_f32(a1, vld1q_f32(a + i + 4), vld1q_f32(b + i + 4));
        a2 = vfmaq_f32(a2, vld1q_f32(a + i + 8), vld1q_f32(b + i + 8));
        a3 = vfmaq_f32(a3, vld1q_f32(a + i + 12), vld1q_f32(b + i + 12));
    }
    return vaddvq_f32(vaddq_f32(vaddq_f32(a0, a1), vaddq_f32(a2, a3)));
}
int main(void) {
    long n = 65536;
    float *a = malloc(n * 4), *b = malloc(n * 4);
    for (long i = 0; i < n; i++) { a[i] = (float)(i % 8) * 0.125f; b[i] = 1.0f; }
    double s = 0;
    for (int r = 0; r < 2000; r++) s += dot(a, b, n);
    printf("%.1f\n", s);
    return 0;
}
