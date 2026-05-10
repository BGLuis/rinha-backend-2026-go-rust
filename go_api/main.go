package main

/*
#cgo LDFLAGS: -lrust_engine -lm -ldl
#include <stdint.h>
#include <stdlib.h>

int32_t init_engine(const char* path);
int32_t search(const float* query);
*/
import "C"

import (
	"encoding/json"
	"log"
	"net"
	"os"
	"runtime/debug"
	"time"
	"unsafe"

	"github.com/buger/jsonparser"
	"github.com/valyala/fasthttp"
)

var (
	MaxAmount             float64
	MaxInstallments       float64
	AmountVsAvgRatio      float64
	MaxMinutes            float64
	MaxKm                 float64
	MaxTxCount24h         float64
	MaxMerchantAvgAmount  float64
)

var MccRisk = make(map[string]float64)

var (
	resp0 = []byte(`{"approved":true,"fraud_score":0.0}`)
	resp1 = []byte(`{"approved":true,"fraud_score":0.2}`)
	resp2 = []byte(`{"approved":true,"fraud_score":0.4}`)
	resp3 = []byte(`{"approved":false,"fraud_score":0.6}`)
	resp4 = []byte(`{"approved":false,"fraud_score":0.8}`)
	resp5 = []byte(`{"approved":false,"fraud_score":1.0}`)
)

// Movido para o escopo global para evitar alocação na stack/heap a cada requisição
var jsonPaths = [][]string{
	{"transaction", "amount"},
	{"transaction", "installments"},
	{"transaction", "requested_at"},
	{"customer", "avg_amount"},
	{"customer", "tx_count_24h"},
	{"customer", "known_merchants"},
	{"last_transaction", "timestamp"},
	{"last_transaction", "km_from_current"},
	{"terminal", "km_from_home"},
	{"terminal", "is_online"},
	{"terminal", "card_present"},
	{"merchant", "id"},
	{"merchant", "mcc"},
	{"merchant", "avg_amount"},
}

// Lookup table O(1) pré-computada para os dias de Março de 2026
// 1º de Março de 2026 cai em um Domingo (0). Array é 1-indexed.
var march2026Weekdays = [32]int{
	0, // padding (dia 0 não existe)
	0, 1, 2, 3, 4, 5, 6, // 1-7
	0, 1, 2, 3, 4, 5, 6, // 8-14
	0, 1, 2, 3, 4, 5, 6, // 15-21
	0, 1, 2, 3, 4, 5, 6, // 22-28
	0, 1, 2,             // 29-31
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

	mccData, _ := os.ReadFile("resources/mcc_risk.json")
	json.Unmarshal(mccData, &MccRisk)
}

func clamp(v float64) float64 {
	if v < 0 { return 0 }
	if v > 1 { return 1 }
	return v
}

func parseRFC3339(b []byte) (hour int, weekday int, unix int64) {
	if len(b) < 19 {
		return 0, 0, 0
	}
	y := int(b[0]-'0')*1000 + int(b[1]-'0')*100 + int(b[2]-'0')*10 + int(b[3]-'0')
	m := int(b[5]-'0')*10 + int(b[6]-'0')
	d := int(b[8]-'0')*10 + int(b[9]-'0')
	hour = int(b[11]-'0')*10 + int(b[12]-'0')
	min := int(b[14]-'0')*10 + int(b[15]-'0')
	sec := int(b[17]-'0')*10 + int(b[18]-'0')

	// Fast Path extremo (O(1)) restrito ao mês da competição (Março de 2026)
	if y == 2026 && m == 3 {
		// Unix Timestamp exato para 2026-03-01T00:00:00Z é 1772323200
		unix = 1772323200 + int64(d-1)*86400 + int64(hour)*3600 + int64(min)*60 + int64(sec)
		weekday = march2026Weekdays[d]
		return hour, weekday, unix
	}

	unix = fastUnix(y, m, d, hour, min, sec)
	weekday = int((unix/86400 + 4) % 7)
	return hour, weekday, unix
}

func fastUnix(year, month, day, hour, min, sec int) int64 {
	y := int64(year)
	m := int64(month)
	d := int64(day)
	cumDays := [13]int64{0, 0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334}
	yearDays := (y-1970)*365 + (y-1969)/4 - (y-1901)/100 + (y-1601)/400
	res := yearDays + cumDays[m] + d - 1
	if m > 2 && (y%4 == 0 && (y%100 != 0 || y%400 == 0)) {
		res++
	}
	return res*86400 + int64(hour)*3600 + int64(min)*60 + int64(sec)
}

func fastHandler(ctx *fasthttp.RequestCtx) {
	start := time.Now()
	path := ctx.Path()
	if string(path) == "/ready" {
		ctx.SetStatusCode(200)
		return
	}

	if string(path) == "/fraud-score" && ctx.IsPost() {
		body := ctx.PostBody()

		var (
			amt               float64
			inst              int64
			cAvgAmt           float64
			reqAtStr          string
			lastTsStr         string
			kmLast            float64 = -1
			kmHome            float64
			txCount           int64
			isOnline          bool
			cardPresent       bool
			merchantId        string
			mccStr            string
			mAvgAmt           float64
			knownMerchantsVal []byte
		)

		// Varredura linear usando a matriz paths global
		jsonparser.EachKey(body, func(idx int, value []byte, vt jsonparser.ValueType, err error) {
			switch idx {
			case 0: amt, _ = jsonparser.ParseFloat(value)
			case 1: inst, _ = jsonparser.ParseInt(value)
			case 2: reqAtStr, _ = jsonparser.ParseString(value)
			case 3: cAvgAmt, _ = jsonparser.ParseFloat(value)
			case 4: txCount, _ = jsonparser.ParseInt(value)
			case 5: knownMerchantsVal = value
			case 6: lastTsStr, _ = jsonparser.ParseString(value)
			case 7: kmLast, _ = jsonparser.ParseFloat(value)
			case 8: kmHome, _ = jsonparser.ParseFloat(value)
			case 9: isOnline, _ = jsonparser.ParseBoolean(value)
			case 10: cardPresent, _ = jsonparser.ParseBoolean(value)
			case 11: merchantId, _ = jsonparser.ParseString(value)
			case 12: mccStr, _ = jsonparser.ParseString(value)
			case 13: mAvgAmt, _ = jsonparser.ParseFloat(value)
			}
		}, jsonPaths...)

		reqHour, reqWeekday, reqUnix := 0, 0, int64(0)
		if reqAtStr != "" {
			reqHour, reqWeekday, reqUnix = parseRFC3339([]byte(reqAtStr))
		}

		var vector [14]float32

		vector[0] = float32(clamp(amt / MaxAmount))
		vector[1] = float32(clamp(float64(inst) / MaxInstallments))
		vector[2] = float32(clamp((amt / cAvgAmt) / AmountVsAvgRatio))
		vector[3] = float32(float64(reqHour) / 23.0)
		vector[4] = float32(float64((reqWeekday+6)%7) / 6.0)

		if lastTsStr == "" {
			vector[5] = -1.0
			vector[6] = -1.0
		} else {
			_, _, lastUnix := parseRFC3339([]byte(lastTsStr))
			minutes := float64(reqUnix-lastUnix) / 60.0
			vector[5] = float32(clamp(minutes / MaxMinutes))
			vector[6] = float32(clamp(kmLast / MaxKm))
		}

		vector[7] = float32(clamp(kmHome / MaxKm))
		vector[8] = float32(clamp(float64(txCount) / MaxTxCount24h))

		if isOnline {
			vector[9] = 1.0
		} else {
			vector[9] = 0.0
		}

		if cardPresent {
			vector[10] = 1.0
		} else {
			vector[10] = 0.0
		}

		known := false
		if knownMerchantsVal != nil {
			jsonparser.ArrayEach(knownMerchantsVal, func(value []byte, dataType jsonparser.ValueType, offset int, err error) {
				if dataType == jsonparser.String {
					val, _ := jsonparser.ParseString(value)
					if val == merchantId {
						known = true
					}
				}
			})
		}
		if !known {
			vector[11] = 1.0
		} else {
			vector[11] = 0.0
		}

		risk, ok := MccRisk[mccStr]
		if !ok {
			risk = 0.5
		}
		vector[12] = float32(risk)
		vector[13] = float32(clamp(mAvgAmt / MaxMerchantAvgAmount))

		if time.Since(start) > 1800*time.Millisecond {
			ctx.SetContentType("application/json")
			ctx.Write(resp0)
			return
		}

		frauds := C.search((*C.float)(unsafe.Pointer(&vector[0])))

		ctx.SetContentType("application/json")
		switch frauds {
		case 0: ctx.Write(resp0)
		case 1: ctx.Write(resp1)
		case 2: ctx.Write(resp2)
		case 3: ctx.Write(resp3)
		case 4: ctx.Write(resp4)
		case 5: ctx.Write(resp5)
		default: ctx.Write(resp0)
		}
		return
	}

	ctx.SetStatusCode(404)
}

func main() {
	// Tuning do GC: Retarda a coleta, trocando um pouco de RAM pelo aumento severo de CPU throughput.
	debug.SetGCPercent(1000)

	loadConfig()

	cPath := C.CString(os.Getenv("DATASET_PATH"))
	C.init_engine(cPath)
	C.free(unsafe.Pointer(cPath))

	socketPath := os.Getenv("SOCKET_PATH")
	if socketPath == "" {
		socketPath = "/tmp/sockets/api.sock"
	}
	os.Remove(socketPath)

	ln, err := net.Listen("unix", socketPath)
	if err != nil {
		log.Fatalf("error in net.Listen: %s", err)
	}
	os.Chmod(socketPath, 0777)

	s := &fasthttp.Server{
		Handler: fastHandler,
	}

	if err := s.Serve(ln); err != nil {
		log.Fatalf("error in Serve: %s", err)
	}
}
