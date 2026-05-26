#include "textflag.h"

// Bridge from Go ABI to C ABI System V AMD64
// func SearchVectorFast(q *float32, scratch *byte) float32
TEXT ·SearchVectorFast(SB), NOSPLIT, $128-24
    // Go places arguments on the stack (ABI0)
    MOVQ q+0(FP), DI         // arg 1: *float32 -> RDI
    MOVQ scratch+8(FP), SI   // arg 2: *byte -> RSI

    // We must ensure the stack pointer (SP) is 16-byte aligned before CALL (C ABI requirement).
    // Go stacks are usually not 16-byte aligned precisely.
    // Save current SP to BP, then align SP.
    MOVQ SP, BP
    ANDQ $~15, SP

    CALL search_vector(SB)

    // Restore SP
    MOVQ BP, SP

    // Return value from C function is in XMM0 (float)
    MOVSS X0, ret+16(FP)
    RET
