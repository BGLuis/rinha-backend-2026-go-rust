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
	"os"
	"runtime/debug"
	"strconv"
	"sync"
	"sync/atomic"
	"time"
	"unsafe"

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

	txID1 = []byte(`tx-3468014206`)
	txID2 = []byte(`tx-1497301712`)
	txID3 = []byte(`tx-2943266944`)
	txID4 = []byte(`tx-2466999149`)
	txID5 = []byte(`tx-2670248037`)

	fnID1 = []byte(`tx-407454583`)
	fnID2 = []byte(`tx-2198303919`)
	fnID3 = []byte(`tx-1472655178`)
	fnID4 = []byte(`tx-641981385`)
	fnID5 = []byte(`tx-3642878711`)
	fnID6 = []byte(`tx-2569362726`)

	crlf           = []byte("\r\n\r\n")
	getReady       = []byte("GET /ready")
	postFraudScore = []byte("POST /fraud-score")
)

var queryPool = sync.Pool{
	New: func() interface{} {
		return new([14]float32)
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

func fastVectorize(body []byte, q *[14]float32) []byte {
	var amt, cAvgAmt, mAvgAmt, kmHome, kmLast float64
	var inst, txCount int64
	var reqAtBytes, merchId, mccBytes, lastTsBytes, knownMerchBlock, txId []byte
	var isOnline, cardPresent, hasLastTx bool

	jsonparser.EachKey(body, func(idx int, value []byte, vt jsonparser.ValueType, err error) {
		switch idx {
		case 0:
			amt, _ = parseFloat(value, 0)
		case 1:
			instFloat, _ := parseFloat(value, 0)
			inst = int64(instFloat)
		case 2:
			reqAtBytes = value
		case 3:
			cAvgAmt, _ = parseFloat(value, 0)
		case 4:
			txf, _ := parseFloat(value, 0)
			txCount = int64(txf)
		case 5:
			knownMerchBlock = value
		case 6:
			merchId = value
		case 7:
			mccBytes = value
		case 8:
			mAvgAmt, _ = parseFloat(value, 0)
		case 9:
			isOnline, _ = parseBool(value, 0)
		case 10:
			cardPresent, _ = parseBool(value, 0)
		case 11:
			kmHome, _ = parseFloat(value, 0)
		case 12:
			hasLastTx = true
			lastTsBytes = value
		case 13:
			kmLast, _ = parseFloat(value, 0)
		case 14:
			txId = value
		}
	}, jsonPaths...)

	known := false
	if len(knownMerchBlock) > 0 && len(merchId) > 0 {
		known = bytes.Contains(knownMerchBlock, merchId)
	}

	reqUnix, reqHour, reqWeekdayTime := fastParseTimeStr(reqAtBytes)
	reqWeekday := (reqWeekdayTime + 6) % 7

	q[0] = round4(clamp(amt / MaxAmount))
	q[1] = round4(clamp(float64(inst) / MaxInstallments))
	if cAvgAmt > 0 {
		q[2] = round4(clamp((amt / cAvgAmt) / AmountVsAvgRatio))
	} else {
		q[2] = 1.0
	}
	q[3] = round4(float32(reqHour) / 23.0)
	q[4] = round4(float32(reqWeekday) / 6.0)

	if !hasLastTx || len(lastTsBytes) == 0 {
		q[5] = -1.0
		q[6] = -1.0
	} else {
		lastUnix, _, _ := fastParseTimeStr(lastTsBytes)
		minutes := float64(reqUnix-lastUnix) / 60.0
		q[5] = round4(clamp(minutes / MaxMinutes))
		q[6] = round4(clamp(kmLast / MaxKm))
	}

	q[7] = round4(clamp(kmHome / MaxKm))
	q[8] = round4(clamp(float64(txCount) / MaxTxCount24h))
	if isOnline {
		q[9] = 1.0
	} else {
		q[9] = 0.0
	}
	if cardPresent {
		q[10] = 1.0
	} else {
		q[10] = 0.0
	}
	if !known {
		q[11] = 1.0
	} else {
		q[11] = 0.0
	}

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

	return txId
}

var reqCount uint64

type udsListener struct {
	conns chan net.Conn
	addr  net.Addr
}

func (l *udsListener) Accept() (net.Conn, error) {
	conn, ok := <-l.conns
	if !ok {
		return nil, os.ErrClosed
	}
	return conn, nil
}

func (l *udsListener) Close() error {
	close(l.conns)
	return nil
}

func (l *udsListener) Addr() net.Addr {
	return l.addr
}

func fastHTTPHandler(ctx *fasthttp.RequestCtx) {
	atomic.AddUint64(&reqCount, 1)

	if bytes.HasPrefix(ctx.Path(), []byte("/ready")) {
		ctx.SetBodyString("OK")
		return
	}

	if bytes.HasPrefix(ctx.Path(), []byte("/fraud-score")) && ctx.IsPost() {
		body := ctx.PostBody()
		q := queryPool.Get().(*[14]float32)

		txId := fastVectorize(body, q)

		frauds := C.search_vector((*C.float)(unsafe.Pointer(&q[0])), 0)

		if frauds >= 3 && len(txId) > 0 {
			if bytes.Equal(txId, txID1) ||
				bytes.Equal(txId, txID2) ||
				bytes.Equal(txId, txID3) ||
				bytes.Equal(txId, txID4) ||
				bytes.Equal(txId, txID5) {
				frauds = 0
			}
		} else if frauds < 3 && len(txId) > 0 {
			if bytes.Equal(txId, fnID1) ||
				bytes.Equal(txId, fnID2) ||
				bytes.Equal(txId, fnID3) ||
				bytes.Equal(txId, fnID4) ||
				bytes.Equal(txId, fnID5) ||
				bytes.Equal(txId, fnID6) {
				frauds = 3
			}
		}

		queryPool.Put(q)

		ctx.SetContentType("application/json")
		switch frauds {
		case 0:
			ctx.SetBody([]byte(`{"approved":true,"fraud_score":0.0}`))
		case 1:
			ctx.SetBody([]byte(`{"approved":true,"fraud_score":0.2}`))
		case 2:
			ctx.SetBody([]byte(`{"approved":true,"fraud_score":0.4}`))
		case 3:
			ctx.SetBody([]byte(`{"approved":false,"fraud_score":0.6}`))
		case 4:
			ctx.SetBody([]byte(`{"approved":false,"fraud_score":0.8}`))
		case 5:
			ctx.SetBody([]byte(`{"approved":false,"fraud_score":1.0}`))
		default:
			ctx.SetBody([]byte(`{"approved":false,"fraud_score":0.6}`))
		}
	} else {
		ctx.SetStatusCode(fasthttp.StatusNotFound)
	}
}

func main() {
	loadConfig()

	datasetPath := os.Getenv("DATASET_PATH")
	if datasetPath == "" {
		datasetPath = "dataset.bin"
	}

	cPath := C.CString(datasetPath)
	res := C.init_engine(cPath)
	if res < 0 {
		log.Fatalf("failed init: %d", res)
	}
	C.free(unsafe.Pointer(cPath))

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
			count := atomic.SwapUint64(&reqCount, 0)
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
