package main

/*
#cgo LDFLAGS: -L${SRCDIR}/../rust_engine/target/release -lrust_engine -lm -ldl
#include <stdint.h>
#include <stdlib.h>

int32_t init_engine(const char* path);
int32_t search_vector(const float* query);
*/
import "C"

import (
	"bytes"
	"encoding/json"
	"log"
	"math"
	"net"
	"os"
	"runtime"
	"runtime/debug"
	"strconv"
	"sync"
	"syscall"
	"time"
	"unsafe"

	"github.com/buger/jsonparser"
	"github.com/valyala/fasthttp"
)

var (
	MaxAmount            float64
	MaxInstallments      float64
	AmountVsAvgRatio     float64
	MaxMinutes           float64
	MaxKm                float64
	MaxTxCount24h        float64
	MaxMerchantAvgAmount float64
)

var MccRiskArr [10000]float32

var (
	resp0 = []byte(`{"approved":true,"fraud_score":0.0}`)
	resp1 = []byte(`{"approved":true,"fraud_score":0.2}`)
	resp2 = []byte(`{"approved":true,"fraud_score":0.4}`)
	resp3 = []byte(`{"approved":false,"fraud_score":0.6}`)
	resp4 = []byte(`{"approved":false,"fraud_score":0.8}`)
	resp5 = []byte(`{"approved":false,"fraud_score":1.0}`)
)

var queryPool = sync.Pool{
	New: func() interface{} {
		return new([14]float32)
	},
}

func loadConfig() {
	normData, _ := os.ReadFile("resources/normalization.json")
	var norm struct {
		MaxAmount            float64 `json:"max_amount"`
		MaxInstallments      float64 `json:"max_installments"`
		AmountVsAvgRatio     float64 `json:"amount_vs_avg_ratio"`
		MaxMinutes           float64 `json:"max_minutes"`
		MaxKm                float64 `json:"max_km"`
		MaxTxCount24h        float64 `json:"max_tx_count_24h"`
		MaxMerchantAvgAmount float64 `json:"max_merchant_avg_amount"`
	}
	json.Unmarshal(normData, &norm)
	MaxAmount = norm.MaxAmount
	MaxInstallments = norm.MaxInstallments
	AmountVsAvgRatio = norm.AmountVsAvgRatio
	MaxMinutes = norm.MaxMinutes
	MaxKm = norm.MaxKm
	MaxTxCount24h = norm.MaxTxCount24h
	MaxMerchantAvgAmount = norm.MaxMerchantAvgAmount

	for i := range MccRiskArr {
		MccRiskArr[i] = 0.5
	}
	mccData, _ := os.ReadFile("resources/mcc_risk.json")
	var mccMap map[string]float64
	json.Unmarshal(mccData, &mccMap)
	for k, v := range mccMap {
		m, _ := strconv.Atoi(k)
		if m < 10000 {
			MccRiskArr[m] = float32(v)
		}
	}
}

func clamp(v float64) float32 {
	if v < 0 { return 0 }
	if v > 1 { return 1 }
	return float32(v)
}

func round4(v float32) float32 {
	if v == -1 { return -1 }
	return float32(math.Round(float64(v)*10000) / 10000)
}

func fastHandler(ctx *fasthttp.RequestCtx) {
	path := ctx.Path()
	if len(path) == 6 && path[1] == 'r' { // /ready
		ctx.SetStatusCode(200)
		return
	}

	if len(path) == 12 && path[1] == 'f' && ctx.IsPost() { // /fraud-score
		body := ctx.PostBody()

		var (
			amt         float64
			inst        int64
			reqAtStr    string
			cAvgAmt     float64
			txCount     int64
			merchantId  []byte
			mccStr      string
			mAvgAmt     float64
			isOnline    bool
			cardPresent bool
			kmHome      float64
		)

		paths := [][]string{
			{"transaction", "amount"},
			{"transaction", "installments"},
			{"transaction", "requested_at"},
			{"customer", "avg_amount"},
			{"customer", "tx_count_24h"},
			{"merchant", "id"},
			{"merchant", "mcc"},
			{"merchant", "avg_amount"},
			{"terminal", "is_online"},
			{"terminal", "card_present"},
			{"terminal", "km_from_home"},
		}

		jsonparser.EachKey(body, func(idx int, value []byte, dataType jsonparser.ValueType, err error) {
			switch idx {
			case 0: amt, _ = jsonparser.ParseFloat(value)
			case 1: inst, _ = jsonparser.ParseInt(value)
			case 2: reqAtStr, _ = jsonparser.ParseString(value)
			case 3: cAvgAmt, _ = jsonparser.ParseFloat(value)
			case 4: txCount, _ = jsonparser.ParseInt(value)
			case 5:
				if len(value) >= 2 && value[0] == '"' {
					merchantId = value[1 : len(value)-1]
				} else {
					merchantId = value
				}
			case 6: mccStr, _ = jsonparser.ParseString(value)
			case 7: mAvgAmt, _ = jsonparser.ParseFloat(value)
			case 8: isOnline, _ = jsonparser.ParseBoolean(value)
			case 9: cardPresent, _ = jsonparser.ParseBoolean(value)
			case 10: kmHome, _ = jsonparser.ParseFloat(value)
			}
		}, paths...)

		lastTx, _, _, _ := jsonparser.Get(body, "last_transaction")

		known := false
		jsonparser.ArrayEach(body, func(value []byte, dataType jsonparser.ValueType, offset int, err error) {
			if !known && bytes.Equal(value, merchantId) {
				known = true
			}
		}, "customer", "known_merchants")

		reqAt, _ := time.Parse(time.RFC3339, reqAtStr)
		reqHour := reqAt.Hour()
		reqWeekday := int(reqAt.Weekday()+6) % 7 // Monday=0

		q := queryPool.Get().(*[14]float32)
		defer queryPool.Put(q)

		q[0] = round4(clamp(amt / MaxAmount))
		q[1] = round4(clamp(float64(inst) / MaxInstallments))
		if cAvgAmt > 0 {
			q[2] = round4(clamp((amt / cAvgAmt) / AmountVsAvgRatio))
		} else {
			q[2] = 1.0
		}
		q[3] = round4(float32(reqHour) / 23.0)
		q[4] = round4(float32(reqWeekday) / 6.0)

		if lastTx == nil || len(lastTx) == 0 || bytes.Equal(lastTx, []byte("null")) {
			q[5] = -1.0
			q[6] = -1.0
		} else {
			lastTsStr, _ := jsonparser.GetString(lastTx, "timestamp")
			kmLast, _ := jsonparser.GetFloat(lastTx, "km_from_current")
			lastTs, _ := time.Parse(time.RFC3339, lastTsStr)
			minutes := float64(reqAt.Unix()-lastTs.Unix()) / 60.0
			q[5] = round4(clamp(minutes / MaxMinutes))
			q[6] = round4(clamp(kmLast / MaxKm))
		}

		q[7] = round4(clamp(kmHome / MaxKm))
		q[8] = round4(clamp(float64(txCount) / MaxTxCount24h))
		if isOnline { q[9] = 1.0 } else { q[9] = 0.0 }
		if cardPresent { q[10] = 1.0 } else { q[10] = 0.0 }
		if !known { q[11] = 1.0 } else { q[11] = 0.0 }

		m, _ := strconv.Atoi(mccStr)
		if m < 10000 {
			q[12] = round4(MccRiskArr[m])
		} else {
			q[12] = 0.5
		}
		q[13] = round4(clamp(mAvgAmt / MaxMerchantAvgAmount))

		frauds := C.search_vector((*C.float)(unsafe.Pointer(&q[0])))

		ctx.SetContentType("application/json")
		switch frauds {
		case 0: ctx.Write(resp0)
		case 1: ctx.Write(resp1)
		case 2: ctx.Write(resp2)
		case 3: ctx.Write(resp3)
		case 4: ctx.Write(resp4)
		case 5: ctx.Write(resp5)
		default: ctx.Write(resp3)
		}
		return
	}
	ctx.SetStatusCode(404)
}

func main() {
	runtime.GOMAXPROCS(1)
	loadConfig()

	datasetPath := os.Getenv("DATASET_PATH")
	if datasetPath == "" { datasetPath = "dataset.bin" }

	// Pre-warm
	f, err := os.Open(datasetPath)
	if err == nil {
		st, _ := f.Stat()
		size := st.Size()
		if size > 0 {
			data, err := syscall.Mmap(int(f.Fd()), 0, int(size), syscall.PROT_READ, syscall.MAP_SHARED)
			if err == nil {
				var sum byte
				for i := 0; i < len(data); i += 4096 {
					sum ^= data[i]
				}
				if sum == 42 && time.Now().Unix() == 0 { log.Print(sum) }
			}
		}
	}

	cPath := C.CString(datasetPath)
	res := C.init_engine(cPath)
	if res < 0 { log.Fatalf("error initializing rust engine: %d", res) }
	C.free(unsafe.Pointer(cPath))

	socketPath := os.Getenv("SOCKET_PATH")
	if socketPath == "" { socketPath = "/tmp/sockets/api.sock" }
	os.Remove(socketPath)

	ln, err := net.Listen("unix", socketPath)
	if err != nil { log.Fatalf("error in net.Listen: %s", err) }
	os.Chmod(socketPath, 0777)

	debug.SetGCPercent(-1)

	s := &fasthttp.Server{
		Handler:               fastHandler,
		NoDefaultServerHeader: true,
		NoDefaultDate:         true,
		NoDefaultContentType:  true,
		Name:                  "rinha",
		Concurrency:           1024,
	}
	if err := s.Serve(ln); err != nil { log.Fatalf("error in Serve: %s", err) }
}
