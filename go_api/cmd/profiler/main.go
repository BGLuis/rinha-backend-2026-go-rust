package main

/*
#cgo LDFLAGS: -L/usr/local/lib -lrust_engine -lm -ldl
#include <stdint.h>
int32_t init_engine();
float search_vector(const float* query, int32_t force_deep);
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
	res := C.init_engine()
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
	var q [16]float32

	// Run 50000 synthetic searches to build a representative profile
	for i := 0; i < 50000; i++ {
		for j := 0; j < 16; j++ {
			q[j] = rng.Float32()
		}
		C.search_vector((*C.float)(unsafe.Pointer(&q[0])), 0)
	}

	log.Println("PGO profile collected: cpu.pprof")
}
