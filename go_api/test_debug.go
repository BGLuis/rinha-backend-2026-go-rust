package main

/*
#cgo LDFLAGS: -L../rust_engine/target/release -lrust_engine -lm -ldl
#include <stdint.h>
#include <stdlib.h>

int32_t init_engine(const char* path);
int32_t search(const float* query);
*/
import "C"

import (
	"encoding/json"
	"fmt"
	"log"
	"math"
	"os"
	"time"
	"unsafe"
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

func loadConfig() {
	normData, _ := os.ReadFile("../resources/normalization.json")
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

	mccData, _ := os.ReadFile("../resources/mcc_risk.json")
	json.Unmarshal(mccData, &MccRisk)
}

func clamp(v float64) float64 {
	if v < 0 { return 0 }
	if v > 1 { return 1 }
	return v
}

func round4(v float64) float32 {
	return float32(math.Round(v*10000) / 10000)
}

type Entry struct {
	Request          json.RawMessage `json:"request"`
	ExpectedApproved bool            `json:"expected_approved"`
}

func vectorize(body []byte) [14]float32 {
	var vector [14]float32

	var req struct {
		Transaction struct {
			Amount       float64 `json:"amount"`
			Installments float64 `json:"installments"`
			RequestedAt  string  `json:"requested_at"`
		} `json:"transaction"`
		Customer struct {
			AvgAmount      float64  `json:"avg_amount"`
			TxCount24h     float64  `json:"tx_count_24h"`
			KnownMerchants []string `json:"known_merchants"`
		} `json:"customer"`
		Merchant struct {
			Id        string  `json:"id"`
			Mcc       string  `json:"mcc"`
			AvgAmount float64 `json:"avg_amount"`
		} `json:"merchant"`
		Terminal struct {
			IsOnline    bool    `json:"is_online"`
			CardPresent bool    `json:"card_present"`
			KmFromHome  float64 `json:"km_from_home"`
		} `json:"terminal"`
		LastTransaction *struct {
			Timestamp      string  `json:"timestamp"`
			KmFromCurrent  float64 `json:"km_from_current"`
		} `json:"last_transaction"`
	}

	json.Unmarshal(body, &req)

	amt := req.Transaction.Amount
	vector[0] = round4(clamp(amt / MaxAmount))

	inst := req.Transaction.Installments
	vector[1] = round4(clamp(inst / MaxInstallments))

	cAvgAmt := req.Customer.AvgAmount
	vector[2] = round4(clamp((amt / cAvgAmt) / AmountVsAvgRatio))

	reqAt, _ := time.Parse(time.RFC3339, req.Transaction.RequestedAt)
	reqAt = reqAt.UTC()
	vector[3] = round4(float64(reqAt.Hour()) / 23.0)
	vector[4] = round4(float64((int(reqAt.Weekday())+6)%7) / 6.0)

	if req.LastTransaction == nil {
		vector[5] = -1.0
		vector[6] = -1.0
	} else {
		lastTs, _ := time.Parse(time.RFC3339, req.LastTransaction.Timestamp)
		lastTs = lastTs.UTC()
		// Usar diferença inteira de segundos para minutos (floor)
		diffSeconds := reqAt.Unix() - lastTs.Unix()
		minutes := float64(diffSeconds / 60)
		vector[5] = round4(clamp(minutes / MaxMinutes))

		kmLast := req.LastTransaction.KmFromCurrent
		vector[6] = round4(clamp(kmLast / MaxKm))
	}

	kmHome := req.Terminal.KmFromHome
	vector[7] = round4(clamp(kmHome / MaxKm))

	txCount := req.Customer.TxCount24h
	vector[8] = round4(clamp(txCount / MaxTxCount24h))

	if req.Terminal.IsOnline {
		vector[9] = 1.0
	} else {
		vector[9] = 0.0
	}

	if req.Terminal.CardPresent {
		vector[10] = 1.0
	} else {
		vector[10] = 0.0
	}

	known := false
	for _, m := range req.Customer.KnownMerchants {
		if m == req.Merchant.Id {
			known = true
			break
		}
	}
	if !known {
		vector[11] = 1.0
	} else {
		vector[11] = 0.0
	}

	risk, ok := MccRisk[req.Merchant.Mcc]
	if !ok {
		risk = 0.5
	}
	vector[12] = round4(risk)

	mAvgAmt := req.Merchant.AvgAmount
	vector[13] = round4(clamp(mAvgAmt / MaxMerchantAvgAmount))

	return vector
}

func main() {
	loadConfig()

	cPath := C.CString("../dataset.bin")
	res := C.init_engine(cPath)
	C.free(unsafe.Pointer(cPath))
	if res != 0 {
		log.Fatalf("Falha ao inicializar engine: %d", res)
	}

	data, err := os.ReadFile("../test/test-data.json")
	if err != nil {
		log.Fatal(err)
	}

	var root struct {
		Entries []Entry `json:"entries"`
	}
	json.Unmarshal(data, &root)

	fmt.Printf("Analisando %d requisições...\n", len(root.Entries))

	failures := 0
	for i, entry := range root.Entries {
		v := vectorize(entry.Request)
		frauds := C.search((*C.float)(unsafe.Pointer(&v[0])))
		fraudScore := float64(frauds) / 5.0
		approved := fraudScore < 0.6

		if approved != entry.ExpectedApproved {
			failures++
			if failures <= 10 {
				fmt.Printf("--- Falha %d (Index %d) ---\n", failures, i)
				fmt.Printf("  Payload: %s\n", entry.Request)
				fmt.Printf("  Vector: %v\n", v)
				fmt.Printf("  Actual Frauds: %d (Approved: %v)\n", frauds, approved)
				fmt.Printf("  Expected Approved: %v\n\n", entry.ExpectedApproved)
			}
		}
	}

	fmt.Printf("Análise completa. Total de falhas: %d\n", failures)
}
