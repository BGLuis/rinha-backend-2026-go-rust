#include "textflag.h"

// Bridge from Go ABI to C ABI System V AMD64
// func SearchVectorFast(q *float32, scratch *byte) float32
TEXT ·SearchVectorFast(SB), NOSPLIT, $128-24
    // Go places arguments on the stack (ABI0)
    MOVQ q+0(FP), DI         // arg 1: *float32 -> RDI
    MOVQ scratch+8(FP), SI   // arg 2: *byte -> RSI

    // Switch SP to the top of the scratch buffer (128KB) to avoid Go stack overflow
    // Save current SP to R12 (callee-saved in C ABI)
    MOVQ SP, R12
    LEAQ 131072(SI), SP
    ANDQ $~15, SP // align to 16 bytes

    CALL search_vector(SB)

    // Restore Go SP
    MOVQ R12, SP

    // Return value from C function is in XMM0 (float)
    MOVSS X0, ret+16(FP)
    RET
