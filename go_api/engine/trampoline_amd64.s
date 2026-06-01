#include "textflag.h"

// Bridge from Go ABI to C ABI System V AMD64
// func SearchVectorFast(q *float32, scratch *byte) int32
TEXT ·SearchVectorFast(SB), NOSPLIT, $128-20
    // Go places arguments on the stack (ABI0)
    MOVQ q+0(FP), DI         // arg 1: *float32 -> RDI
    XORL SI, SI              // arg 2: int32 -> ESI (force_deep = 0)

    // Switch SP to the top of the scratch buffer (128KB) to avoid Go stack overflow
    // Save current SP to R12 (callee-saved in C ABI)
    MOVQ SP, R12
    MOVQ scratch+8(FP), CX   // load scratch pointer to CX
    LEAQ 131072(CX), SP      // use scratch buffer + 128KB as stack
    ANDQ $~15, SP // align to 16 bytes

    CALL search_vector(SB)

    // Restore Go SP
    MOVQ R12, SP

    // Return value from C function is in AX (int32)
    MOVL AX, ret+16(FP)
    RET
