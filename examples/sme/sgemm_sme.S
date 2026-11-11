// SME FP32 matrix multiply kernel for Apple M4 (and any SME CPU).
//
//   void sgemm_sme(const float *at, const float *b, float *c, i64 m, i64 n, i64 k)
//
// Computes C = A * B where C is m x n row-major, B is k x n row-major and A
// is given transposed (at is k x m row-major, i.e. A column-major). m and n
// must be multiples of the streaming vector length in floats (SVL/32: 16 on
// M4, whose SVL is 512 bits). Each 16x16 output tile is accumulated in ZA0
// with one FMOPA (outer product) per k step; the code is vector-length
// agnostic (it reads the length with cntw), so it also runs under qemu.
//
// SMSTART/SMSTOP reset the vector registers, so d8-d15 (callee-saved in
// both the Apple and AAPCS64 ABIs) are saved and restored around them.

#ifdef __APPLE__
#define SYM(x) _##x
#else
#define SYM(x) x
#endif

	.arch armv9-a
	.arch_extension sme
	.text
	.p2align 2
	.globl SYM(sgemm_sme)
SYM(sgemm_sme):
	stp	d8, d9, [sp, #-64]!
	stp	d10, d11, [sp, #16]
	stp	d12, d13, [sp, #32]
	stp	d14, d15, [sp, #48]
	smstart
	ptrue	p0.s
	cntw	x6                      // floats per streaming vector
	mov	x7, #0                  // i0
1:	cmp	x7, x3
	b.ge	6f
	mov	x8, #0                  // j0
2:	cmp	x8, x4
	b.ge	5f
	zero	{za0.s}
	add	x10, x0, x7, lsl #2     // &at[0][i0]
	add	x11, x1, x8, lsl #2     // &b[0][j0]
	mov	x9, #0                  // k
3:	cmp	x9, x5
	b.ge	4f
	ld1w	{z0.s}, p0/z, [x10]
	ld1w	{z1.s}, p0/z, [x11]
	fmopa	za0.s, p0/m, p0/m, z0.s, z1.s
	add	x10, x10, x3, lsl #2
	add	x11, x11, x4, lsl #2
	add	x9, x9, #1
	b	3b
4:	mul	x13, x7, x4             // store the tile row by row
	add	x13, x13, x8
	add	x13, x2, x13, lsl #2
	mov	w12, #0
7:	cmp	w12, w6
	b.ge	8f
	st1w	{za0h.s[w12, 0]}, p0, [x13]
	add	x13, x13, x4, lsl #2
	add	w12, w12, #1
	b	7b
8:	add	x8, x8, x6
	b	2b
5:	add	x7, x7, x6
	b	1b
6:	smstop
	ldp	d14, d15, [sp, #48]
	ldp	d12, d13, [sp, #32]
	ldp	d10, d11, [sp, #16]
	ldp	d8, d9, [sp], #64
	ret

	.globl SYM(sme_svl_floats)
	.p2align 2
// i64 sme_svl_floats(void): streaming vector length in floats
SYM(sme_svl_floats):
	rdsvl	x0, #1
	lsr	x0, x0, #2
	ret
