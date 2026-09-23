package main

/*
#cgo LDFLAGS: -L/usr/local/lib -lrust_engine -lm -ldl
#include <stdint.h>
#include <stdlib.h>

int32_t init_engine(const char* path);
int32_t search_vector(const int16_t* query, int32_t force_deep);
*/
import "C"

import (
	"log"
	"math/rand"
	"os"
	"runtime/pprof"
	"unsafe"
)

func main() {
	cPath := C.CString("dataset.bin")
	res := C.init_engine(cPath)
	C.free(unsafe.Pointer(cPath))
	
	if res < 0 {
		log.Fatalf("init failed: %d", res)
	}

	f, err := os.Create("cpu.pprof")
	if err != nil {
		log.Fatal(err)
	}
	defer f.Close()

	pprof.StartCPUProfile(f)
	defer pprof.StopCPUProfile()

	rng := rand.New(rand.NewSource(42))
	var q [16]int16

	// Run 50000 synthetic searches to build a representative profile
	for i := 0; i < 50000; i++ {
		for j := 0; j < 14; j++ {
			q[j] = int16(rng.Intn(10000))
		}
		q[14] = 0
		q[15] = 0
		C.search_vector((*C.int16_t)(unsafe.Pointer(&q[0])), 0)
	}

	log.Println("PGO profile collected: cpu.pprof")
}
