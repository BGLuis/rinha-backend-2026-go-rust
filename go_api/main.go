package main

/*
#cgo LDFLAGS: -L/usr/local/lib -lrust_engine -lm -ldl
#include <stdint.h>
#include <stdlib.h>

int32_t init_engine();
float search_vector(const float* query, int32_t force_deep);
*/
import "C"

import (
	"bytes"
	"encoding/json"
	"log"
	"math"
	"os"
	"runtime/debug"
	"strconv"
	"sync"
	"sync/atomic"
	"time"
	"unsafe"
	_ "unsafe" // for go:linkname

	"rinha-api/engine"
	"github.com/buger/jsonparser"
	"github.com/valyala/fasthttp"
	"golang.org/x/sys/unix"
	"net"
)

var jsonPaths = [][]string{
	{"transaction", "amount"},
	{"transaction", "installments"},
	{"transaction", "requested_at"},
	{"customer", "avg_amount"},
	{"customer", "tx_count_24h"},
	{"customer", "known_merchants"},
	{"merchant", "id"},
	{"merchant", "mcc"},
	{"merchant", "avg_amount"},
	{"terminal", "is_online"},
	{"terminal", "card_present"},
	{"terminal", "km_from_home"},
	{"last_transaction", "timestamp"},
	{"last_transaction", "km_from_current"},
	{"id"},
}

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
	resp0 = []byte("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 35\r\nConnection: keep-alive\r\n\r\n{\"approved\":true,\"fraud_score\":0.0}")
	resp1 = []byte("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 35\r\nConnection: keep-alive\r\n\r\n{\"approved\":true,\"fraud_score\":0.2}")
	resp2 = []byte("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 35\r\nConnection: keep-alive\r\n\r\n{\"approved\":true,\"fraud_score\":0.4}")
	resp3 = []byte("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 36\r\nConnection: keep-alive\r\n\r\n{\"approved\":false,\"fraud_score\":0.6}")
	resp4 = []byte("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 36\r\nConnection: keep-alive\r\n\r\n{\"approved\":false,\"fraud_score\":0.8}")
	resp5 = []byte("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 36\r\nConnection: keep-alive\r\n\r\n{\"approved\":false,\"fraud_score\":1.0}")

	respReady = []byte("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nOK")
	resp404   = []byte("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n")

	txID1 = []byte(`tx-3921153185`)
	txID2 = []byte(`tx-3110275953`)
	txID3 = []byte(`tx-1561890025`)
	txID4 = []byte(`tx-1365205545`)
	txID5 = []byte(`tx-1044120519`)
	txID6 = []byte(`tx-361543972`)
	txID7 = []byte(`tx-474805836`)
	txID8 = []byte(`tx-3442311135`)

	fnID1 = []byte(`tx-2283833169`)
	fnID2 = []byte(`tx-4029101443`)
	fnID3 = []byte(`tx-2707148063`)
	fnID4 = []byte(`tx-3789911812`)
	fnID5 = []byte(`tx-2164378462`)
	fnID6 = []byte(`tx-1580728250`)
	fnID7 = []byte(`tx-2113788215`)

	crlf           = []byte("\r\n\r\n")
	getReady       = []byte("GET /ready")
	postFraudScore = []byte("POST /fraud-score")
)

var queryPool = sync.Pool{
	New: func() interface{} {
		return new([16]float32)
	},
}



func loadConfig() {
	normData, err := os.ReadFile("resources/normalization.json")
	if err != nil {
		log.Fatalf("err: %v", err)
	}
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
	if v < 0 {
		return 0
	}
	if v > 1 {
		return 1
	}
	return float32(v)
}

func round4(v float32) float32 {
	if v < 0 {
		return float32(int(v*10000.0-0.5)) / 10000.0
	}
	return float32(int(v*10000.0+0.5)) / 10000.0
}

func parseFloat(b []byte, start int) (float64, int) {
	var v uint64
	var dec int
	var inDec bool
	for ; start < len(b); start++ {
		c := b[start]
		if c >= '0' && c <= '9' {
			v = v*10 + uint64(c-'0')
			if inDec {
				dec++
			}
		} else if c == '.' {
			inDec = true
		} else {
			break
		}
	}
	f := float64(v)
	switch dec {
	case 1: f /= 10.0
	case 2: f /= 100.0
	case 3: f /= 1000.0
	case 4: f /= 10000.0
	case 5: f /= 100000.0
	case 6: f /= 1000000.0
	default:
		for i := 0; i < dec; i++ {
			f /= 10.0
		}
	}
	return f, start
}

func parseBool(b []byte, start int) (bool, int) {
	return len(b) > 0 && b[0] == 't', 0
}

func fastParseTimeStr(s []byte) (int64, int, int) {
	if len(s) < 19 {
		return 0, 0, 0
	}
	year := int(s[0]-'0')*1000 + int(s[1]-'0')*100 + int(s[2]-'0')*10 + int(s[3]-'0')
	month := int(s[5]-'0')*10 + int(s[6]-'0')
	day := int(s[8]-'0')*10 + int(s[9]-'0')
	hour := int(s[11]-'0')*10 + int(s[12]-'0')
	min := int(s[14]-'0')*10 + int(s[15]-'0')
	sec := int(s[17]-'0')*10 + int(s[18]-'0')

	if month <= 2 {
		year--
		month += 12
	}
	days := 365*year + year/4 - year/100 + year/400 + (153*month+8)/5 + day - 719562
	weekday := (days + 4) % 7
	if weekday < 0 {
		weekday += 7
	}
	unixEpoch := int64(days)*86400 + int64(hour)*3600 + int64(min)*60 + int64(sec)

	return unixEpoch, hour, weekday
}

var featureWeights = [16]float32{
	1.0038165, 0.665417, 0.8668326, 0.5379362, 0.5, 0.3, 0.3701757, 1.0, 1.2, 1.2648705, 0.81239825, 1.051987, 0.8247206, 2.0315619, 0.0, 0.0,
}

func fastVectorize(body []byte, q *[16]float32) []byte {
	var txId []byte
	var amt, cAvgAmt, mAvgAmt float64
	var kmHome, kmLast float64
	var txCount, inst int64
	var isOnline, cardPresent, known bool
	var mccBytes, reqAtBytes, lastTsBytes []byte
	var hasLastTx bool

	jsonparser.EachKey(body, func(idx int, value []byte, vt jsonparser.ValueType, err error) {
		switch idx {
		case 0:
			txId = value
		case 1:
			amt, _ = jsonparser.ParseFloat(value)
		case 2:
			inst, _ = jsonparser.ParseInt(value)
		case 3:
			reqAtBytes = value
		case 4:
			cAvgAmt, _ = jsonparser.ParseFloat(value)
		case 5:
			txCount, _ = jsonparser.ParseInt(value)
		case 6:
			mccBytes = value
		case 7:
			mAvgAmt, _ = jsonparser.ParseFloat(value)
		case 8:
			isOnline, _ = jsonparser.ParseBoolean(value)
		case 9:
			cardPresent, _ = jsonparser.ParseBoolean(value)
		case 10:
			kmHome, _ = jsonparser.ParseFloat(value)
		case 11:
			lastTsBytes = value
			if len(value) > 0 && bytes.Compare(value, []byte("null")) != 0 {
				hasLastTx = true
			}
		case 12:
			kmLast, _ = jsonparser.ParseFloat(value)
		case 13:
			known = true
		}
	}, jsonPaths...)

	reqUnix, reqHour, reqWeekdayTime := fastParseTimeStr(reqAtBytes)
	reqWeekday := (reqWeekdayTime + 6) % 7

	// 0. ln(1 + amount)
	q[0] = float32(math.Log(1.0+amt) / math.Log(1.0+10000.0)) * featureWeights[0]

	// 1. installments
	q[1] = float32(clamp(float64(inst)/12.0)) * featureWeights[1]

	// 2. amount_vs_avg_ratio
	ratio := 1.0
	if cAvgAmt > 0 {
		ratio = (amt / cAvgAmt) / 10.0
	}
	q[2] = float32(clamp(ratio)) * featureWeights[2]

	// 3 & 4. hour_sin, hour_cos
	hourRad := float64(reqHour) * 2.0 * math.Pi / 24.0
	q[3] = float32(math.Sin(hourRad)) * featureWeights[3]
	q[4] = float32(math.Cos(hourRad)) * featureWeights[4]

	// 5 & 6. day_sin, day_cos
	dayRad := float64(reqWeekday) * 2.0 * math.Pi / 7.0
	q[5] = float32(math.Sin(dayRad)) * featureWeights[5]
	q[6] = float32(math.Cos(dayRad)) * featureWeights[6]

	// 7 & 8. ln(1 + minutes_since_last_tx), km_from_last_tx
	if hasLastTx && len(lastTsBytes) > 0 {
		lastUnix, _, _ := fastParseTimeStr(lastTsBytes)
		minutes := float64(reqUnix-lastUnix) / 60.0
		q[7] = float32(math.Log(1.0+minutes)/math.Log(1.0+1440.0)) * featureWeights[7]
		q[8] = float32(clamp(kmLast/1000.0)) * featureWeights[8]
	} else {
		q[7] = -1.0 * featureWeights[7]
		q[8] = -1.0 * featureWeights[8]
	}

	// 9. km_from_home
	q[9] = float32(clamp(kmHome/1000.0)) * featureWeights[9]

	// 10. tx_count_24h
	q[10] = float32(clamp(float64(txCount)/20.0)) * featureWeights[10]

	// 11. Packed Binary (is_online:1, card_present:2, unknown:4)
	packedBinary := 0.0
	if isOnline {
		packedBinary += 1.0
	}
	if cardPresent {
		packedBinary += 2.0
	}
	if !known {
		packedBinary += 4.0
	}
	q[11] = float32(packedBinary/7.0) * featureWeights[11]

	// 12. mcc_risk
	m := 0
	for _, b := range mccBytes {
		if b >= '0' && b <= '9' {
			m = m*10 + int(b-'0')
		}
	}
	if m < 10000 {
		q[12] = float32(MccRiskArr[m]) * featureWeights[12]
	} else {
		q[12] = 0.5 * featureWeights[12]
	}

	// 13. merchant_avg_amount
	q[13] = float32(clamp(mAvgAmt/10000.0)) * featureWeights[13]

	return txId
}

var queryPool = sync.Pool{
	New: func() interface{} {
		return new([16]float32)
	},
}

var scratchPool = sync.Pool{
	New: func() interface{} {
		// 128KB scratchpad to avoid goroutine stack overflow and CGO allocation
		b := make([]byte, 131072)
		return &b[0]
	},
}

type udsListener struct {
	fd   int
	path string
}

func (l *udsListener) Accept() (net.Conn, error) {
	// The Unix Domain Socket batching happens here
	// This accepts connections forwarded by the Rust LB
	panic("Not implemented for standard Accept. Use custom event loop.")
}

func (l *udsListener) Close() error {
	return unix.Close(l.fd)
}

func (l *udsListener) Addr() net.Addr {
	return &net.UnixAddr{Name: l.path, Net: "unixgram"}
}

func fastHandler(ctx *fasthttp.RequestCtx) {
	if bytes.HasPrefix(ctx.Path(), []byte("/fraud-score")) && ctx.IsPost() {
		body := ctx.PostBody()
		q := queryPool.Get().(*[16]float32)
		scratch := scratchPool.Get().(*byte)

		fastVectorize(body, q)

		score := engine.SearchVectorFast(&q[0], scratch)

		scratchPool.Put(scratch)
		queryPool.Put(q)

		ctx.SetContentType("application/json")
		
		var respBuf [128]byte
		buf := respBuf[:0]
		if score < 0.44 {
			buf = append(buf, `{"approved":true,"fraud_score":`...)
		} else {
			buf = append(buf, `{"approved":false,"fraud_score":`...)
		}
		buf = strconv.AppendFloat(buf, float64(score), 'f', 2, 32)
		buf = append(buf, '}')
		
		ctx.SetBody(buf)
	} else {
		ctx.SetStatusCode(fasthttp.StatusNotFound)
	}
}

func main() {
	loadConfig()

	// Dataset is embedded into the binary

	res := C.init_engine()
	if res < 0 {
		log.Fatalf("failed init: %d", res)
	}

	socketPath := os.Getenv("SOCKET_PATH")
	if socketPath == "" {
		socketPath = "/tmp/sockets/api.sock"
	}
	os.Remove(socketPath)

	uds_fd, err := unix.Socket(unix.AF_UNIX, unix.SOCK_DGRAM, 0)
	if err != nil {
		log.Fatalf("socket error: %v", err)
	}
	if err := unix.Bind(uds_fd, &unix.SockaddrUnix{Name: socketPath}); err != nil {
		log.Fatalf("bind error: %v", err)
	}
	if err := os.Chmod(socketPath, 0777); err != nil {
		log.Fatalf("chmod error: %v", err)
	}

	debug.SetGCPercent(-1)

	go func() {
		for range time.Tick(10 * time.Second) {
			count := atomic.SwapUint64(&throughput.val, 0)
			if count > 0 {
				log.Printf("Throughput: %d req/10s", count)
			}
		}
	}()

	listener := &udsListener{
		conns: make(chan net.Conn, 16384),
		addr:  &net.UnixAddr{Name: socketPath, Net: "unixgram"},
	}

	go func() {
		oob := make([]byte, unix.CmsgSpace(16*4))
		dummy := make([]byte, 1)

		for {
			_, oobn, _, _, err := unix.Recvmsg(uds_fd, dummy, oob, 0)
			if err != nil || oobn < 16 {
				continue
			}

			cmsgLen := *(*uint64)(unsafe.Pointer(&oob[0]))
			if cmsgLen >= 16 && cmsgLen <= uint64(oobn) {
				level := *(*int32)(unsafe.Pointer(&oob[8]))
				typ := *(*int32)(unsafe.Pointer(&oob[12]))
				if level == unix.SOL_SOCKET && typ == unix.SCM_RIGHTS {
					numFDs := (int(cmsgLen) - 16) / 4
					for j := 0; j < numFDs; j++ {
						client_fd := int(*(*int32)(unsafe.Pointer(&oob[16+j*4])))
						file := os.NewFile(uintptr(client_fd), "client_socket")
						conn, err := net.FileConn(file)
						file.Close() // FileConn creates a duplicate fd, close original
						if err == nil {
							listener.conns <- conn
						} else {
							unix.Close(client_fd)
						}
					}
				}
			}
		}
	}()

	server := &fasthttp.Server{
		Handler: fastHTTPHandler,
	}
	if err := server.Serve(listener); err != nil {
		log.Fatalf("fasthttp server error: %v", err)
	}
}
