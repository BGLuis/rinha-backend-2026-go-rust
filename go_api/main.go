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
	"time"
	"unsafe"

	"github.com/buger/jsonparser"
	"github.com/valyala/fasthttp"
)

var (
	MaxAmount             float64
	MaxInstallments      float64
	AmountVsAvgRatio     float64
	MaxMinutes            float64
	MaxKm                 float64
	MaxTxCount24h         float64
	MaxMerchantAvgAmount float64
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

func loadConfig() {
	normData, _ := os.ReadFile("resources/normalization.json")
	var norm struct {
		MaxAmount             float64 `json:"max_amount"`
		MaxInstallments      float64 `json:"max_installments"`
		AmountVsAvgRatio     float64 `json:"amount_vs_avg_ratio"`
		MaxMinutes            float64 `json:"max_minutes"`
		MaxKm                 float64 `json:"max_km"`
		MaxTxCount24h         float64 `json:"max_tx_count_24h"`
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

func fastHandler(ctx *fasthttp.RequestCtx) {
	start := time.Now()
	path := ctx.Path()
	if string(path) == "/ready" {
		ctx.SetStatusCode(200)
		return
	}

	if string(path) == "/fraud-score" && ctx.IsPost() {
		body := ctx.PostBody()

		var vector [14]float32

		amt, _ := jsonparser.GetFloat(body, "transaction", "amount")
		vector[0] = float32(clamp(amt / MaxAmount))

		inst, _ := jsonparser.GetInt(body, "transaction", "installments")
		vector[1] = float32(clamp(float64(inst) / MaxInstallments))

		cAvgAmt, _ := jsonparser.GetFloat(body, "customer", "avg_amount")
		vector[2] = float32(clamp((amt / cAvgAmt) / AmountVsAvgRatio))

		reqAtStr, _ := jsonparser.GetString(body, "transaction", "requested_at")
		reqAt, _ := time.Parse(time.RFC3339, reqAtStr)
		reqAt = reqAt.UTC()
		vector[3] = float32(float64(reqAt.Hour()) / 23.0)
		vector[4] = float32(float64((int(reqAt.Weekday())+6)%7) / 6.0)

		lastTx, _, _, _ := jsonparser.Get(body, "last_transaction")
		if lastTx == nil {
			vector[5] = -1.0
			vector[6] = -1.0
		} else {
			lastTsStr, _ := jsonparser.GetString(lastTx, "timestamp")
			lastTs, _ := time.Parse(time.RFC3339, lastTsStr)
			lastTs = lastTs.UTC()
			diffSeconds := reqAt.Unix() - lastTs.Unix()
			minutes := float64(diffSeconds / 60)
			vector[5] = float32(clamp(minutes / MaxMinutes))

			kmLast, _ := jsonparser.GetFloat(lastTx, "km_from_current")
			vector[6] = float32(clamp(kmLast / MaxKm))
		}

		kmHome, _ := jsonparser.GetFloat(body, "terminal", "km_from_home")
		vector[7] = float32(clamp(kmHome / MaxKm))

		txCount, _ := jsonparser.GetInt(body, "customer", "tx_count_24h")
		vector[8] = float32(clamp(float64(txCount) / MaxTxCount24h))

		isOnline, _ := jsonparser.GetBoolean(body, "terminal", "is_online")
		if isOnline {
			vector[9] = 1.0
		} else {
			vector[9] = 0.0
		}

		cardPresent, _ := jsonparser.GetBoolean(body, "terminal", "card_present")
		if cardPresent {
			vector[10] = 1.0
		} else {
			vector[10] = 0.0
		}

		merchantId, _ := jsonparser.GetString(body, "merchant", "id")
		known := false
		jsonparser.ArrayEach(body, func(value []byte, dataType jsonparser.ValueType, offset int, err error) {
			if dataType == jsonparser.String {
				val, _ := jsonparser.ParseString(value)
				if val == merchantId {
					known = true
				}
			}
		}, "customer", "known_merchants")
		if !known {
			vector[11] = 1.0
		} else {
			vector[11] = 0.0
		}

		mcc, _ := jsonparser.GetString(body, "merchant", "mcc")
		risk, ok := MccRisk[mcc]
		if !ok {
			risk = 0.5
		}
		vector[12] = float32(risk)

		mAvgAmt, _ := jsonparser.GetFloat(body, "merchant", "avg_amount")
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
