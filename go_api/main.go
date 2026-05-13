package main

/*
#cgo LDFLAGS: -L/usr/local/lib -lrust_engine -lm -ldl
#include <stdint.h>
#include <stdlib.h>

int32_t init_engine(const char* path);
int32_t search_vector(const float* query, int32_t force_deep);
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
	"sync/atomic"
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
	normData, err := os.ReadFile("resources/normalization.json")
	if err != nil { log.Fatalf("err: %v", err) }
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
	mccData, err := os.ReadFile("resources/mcc_risk.json")
	if err == nil {
		var mccMap map[string]float64
		json.Unmarshal(mccData, &mccMap)
		for k, v := range mccMap {
			m, _ := strconv.Atoi(k)
			if m < 10000 {
				MccRiskArr[m] = float32(v)
			}
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

func fastParseTime(s []byte) time.Time {
	if len(s) >= 2 && s[0] == '"' {
		s = s[1 : len(s)-1]
	}
	if len(s) < 19 {
		return time.Time{}
	}
	year := int(s[0]-'0')*1000 + int(s[1]-'0')*100 + int(s[2]-'0')*10 + int(s[3]-'0')
	month := int(s[5]-'0')*10 + int(s[6]-'0')
	day := int(s[8]-'0')*10 + int(s[9]-'0')
	hour := int(s[11]-'0')*10 + int(s[12]-'0')
	min := int(s[14]-'0')*10 + int(s[15]-'0')
	sec := int(s[17]-'0')*10 + int(s[18]-'0')
	return time.Date(year, time.Month(month), day, hour, min, sec, 0, time.UTC)
}

var jsonPaths = [][]string{
	{"transaction", "amount"},           // 0
	{"transaction", "installments"},     // 1
	{"transaction", "requested_at"},      // 2
	{"customer", "avg_amount"},          // 3
	{"customer", "tx_count_24h"},        // 4
	{"customer", "known_merchants"},     // 5
	{"merchant", "id"},                  // 6
	{"merchant", "mcc"},                 // 7
	{"merchant", "avg_amount"},          // 8
	{"terminal", "is_online"},           // 9
	{"terminal", "card_present"},        // 10
	{"terminal", "km_from_home"},         // 11
	{"last_transaction", "timestamp"},    // 12
	{"last_transaction", "km_from_current"}, // 13
}

var nullBytes = []byte("null")

func fastVectorize(body []byte, q *[14]float32) {
	var (
		amt, cAvgAmt, mAvgAmt, kmHome, kmLast float64
		inst, txCount                         int64
		reqAtBytes, mccBytes, merchantsBytes  []byte
		merchantId                            []byte
		lastTsBytes                           []byte
		isOnline, cardPresent                 bool
		hasLastTx                             bool
	)

	jsonparser.EachKey(body, func(idx int, value []byte, dataType jsonparser.ValueType, err error) {
		switch idx {
		case 0: amt, _ = jsonparser.ParseFloat(value)
		case 1: inst, _ = jsonparser.ParseInt(value)
		case 2: reqAtBytes = value
		case 3: cAvgAmt, _ = jsonparser.ParseFloat(value)
		case 4: txCount, _ = jsonparser.ParseInt(value)
		case 5: merchantsBytes = value
		case 6:
			if len(value) >= 2 && value[0] == '"' {
				merchantId = value[1 : len(value)-1]
			} else {
				merchantId = value
			}
		case 7: mccBytes = value
		case 8: mAvgAmt, _ = jsonparser.ParseFloat(value)
		case 9: isOnline, _ = jsonparser.ParseBoolean(value)
		case 10: cardPresent, _ = jsonparser.ParseBoolean(value)
		case 11: kmHome, _ = jsonparser.ParseFloat(value)
		case 12: lastTsBytes = value; hasLastTx = true
		case 13: kmLast, _ = jsonparser.ParseFloat(value)
		}
	}, jsonPaths...)

	known := false
	if merchantsBytes != nil {
		jsonparser.ArrayEach(merchantsBytes, func(value []byte, dataType jsonparser.ValueType, offset int, err error) {
			if !known {
				v := value
				if len(v) >= 2 && v[0] == '"' {
					v = v[1 : len(v)-1]
				}
				if bytes.Equal(v, merchantId) {
					known = true
				}
			}
		})
	}

	reqAt := fastParseTime(reqAtBytes)
	reqHour := reqAt.Hour()
	reqWeekday := int(reqAt.Weekday()+6) % 7

	q[0] = round4(clamp(amt / MaxAmount))
	q[1] = round4(clamp(float64(inst) / MaxInstallments))
	if cAvgAmt > 0 {
		q[2] = round4(clamp((amt / cAvgAmt) / AmountVsAvgRatio))
	} else {
		q[2] = 1.0
	}
	q[3] = round4(float32(reqHour) / 23.0)
	q[4] = round4(float32(reqWeekday) / 6.0)

	if !hasLastTx || lastTsBytes == nil || len(lastTsBytes) == 0 || bytes.Equal(lastTsBytes, nullBytes) {
		q[5] = -1.0
		q[6] = -1.0
	} else {
		lastTs := fastParseTime(lastTsBytes)
		minutes := float64(reqAt.Unix()-lastTs.Unix()) / 60.0
		q[5] = round4(clamp(minutes / MaxMinutes))
		q[6] = round4(clamp(kmLast / MaxKm))
	}

	q[7] = round4(clamp(kmHome / MaxKm))
	q[8] = round4(clamp(float64(txCount) / MaxTxCount24h))
	if isOnline { q[9] = 1.0 } else { q[9] = 0.0 }
	if cardPresent { q[10] = 1.0 } else { q[10] = 0.0 }
	if !known { q[11] = 1.0 } else { q[11] = 0.0 }

	m := 0
	for _, b := range mccBytes {
		if b >= '0' && b <= '9' {
			m = m*10 + int(b-'0')
		}
	}
	if m < 10000 {
		q[12] = round4(MccRiskArr[m])
	} else {
		q[12] = 0.5
	}
	q[13] = round4(clamp(mAvgAmt / MaxMerchantAvgAmount))
}

func fastHandler(ctx *fasthttp.RequestCtx) {
	path := ctx.Path()
	if len(path) == 6 && path[1] == 'r' { // /ready
		ctx.SetStatusCode(200)
		return
	}

	if len(path) == 12 && path[1] == 'f' && ctx.IsPost() { // /fraud-score
		body := ctx.PostBody()
		
		q := queryPool.Get().(*[14]float32)
		fastVectorize(body, q)
		frauds := C.search_vector((*C.float)(unsafe.Pointer(&q[0])), 1)
		queryPool.Put(q)

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

	cPath := C.CString(datasetPath)
	res := C.init_engine(cPath)
	if res < 0 { log.Fatalf("failed init: %d", res) }
	C.free(unsafe.Pointer(cPath))

	socketPath := os.Getenv("SOCKET_PATH")
	if socketPath == "" { socketPath = "/tmp/sockets/api.sock" }
	os.Remove(socketPath)

	// Use unixgram for connectionless FD passing
	addr, _ := net.ResolveUnixAddr("unixgram", socketPath)
	conn, err := net.ListenUnixgram("unixgram", addr)
	if err != nil { log.Fatalf("listen error: %v", err) }
	os.Chmod(socketPath, 0777)
	_ = conn.SetReadBuffer(1024 * 1024 * 32)

	debug.SetGCPercent(-1)

	s := &fasthttp.Server{
		Handler:               fastHandler,
		NoDefaultServerHeader: true,
		NoDefaultDate:         true,
		NoDefaultContentType:  true,
		Name:                  "rinha",
		Concurrency:           1024 * 64,
	}

	// Stats logger
	var reqCount uint64
	go func() {
		for range time.Tick(10 * time.Second) {
			count := atomic.SwapUint64(&reqCount, 0)
			if count > 0 {
				log.Printf("Throughput: %d req/10s", count)
			}
		}
	}()

	// Reader - Spawn goroutine per connection
	go func() {
		oob := make([]byte, syscall.CmsgSpace(4))
		buf := make([]byte, 1)
		for {
			_, oobn, _, _, err := conn.ReadMsgUnix(buf, oob)
			if err != nil { 
				log.Printf("ReadMsgUnix error: %v", err)
				continue 
			}
			if oobn == 0 { 
				log.Printf("oobn == 0")
				continue 
			}
			scmsgs, err := syscall.ParseSocketControlMessage(oob[:oobn])
			if err != nil || len(scmsgs) == 0 { 
				log.Printf("ParseSocketControlMessage error: %v, len: %d", err, len(scmsgs))
				continue 
			}
			fds, err := syscall.ParseUnixRights(&scmsgs[0])
			if err != nil || len(fds) == 0 { 
				log.Printf("ParseUnixRights error: %v, len: %d", err, len(fds))
				continue 
			}
			
			go func(fd int) {
				atomic.AddUint64(&reqCount, 1)
				f := os.NewFile(uintptr(fd), "client")
				netConn, err := net.FileConn(f)
				f.Close()
				if err == nil {
					s.ServeConn(netConn)
					netConn.Close()
				} else {
					syscall.Close(fd)
				}
			}(fds[0])
		}
	}()

	select {}
}
