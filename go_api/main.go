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
	"io"
	"log"
	"os"
	"runtime"
	"runtime/debug"
	"strconv"
	"time"
	"unsafe"

	"golang.org/x/sys/unix"
	"rinha-api/engine"
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
	resp0     = []byte("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 35\r\nConnection: keep-alive\r\n\r\n{\"approved\":true,\"fraud_score\":0.0}")
	resp1     = []byte("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 35\r\nConnection: keep-alive\r\n\r\n{\"approved\":true,\"fraud_score\":0.2}")
	resp2     = []byte("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 35\r\nConnection: keep-alive\r\n\r\n{\"approved\":true,\"fraud_score\":0.4}")
	resp3     = []byte("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 36\r\nConnection: keep-alive\r\n\r\n{\"approved\":false,\"fraud_score\":0.6}")
	resp4     = []byte("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 36\r\nConnection: keep-alive\r\n\r\n{\"approved\":false,\"fraud_score\":0.8}")
	resp5     = []byte("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 36\r\nConnection: keep-alive\r\n\r\n{\"approved\":false,\"fraud_score\":1.0}")
	resp404   = []byte("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
	respReady = []byte("HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n")

	resps = [6][]byte{resp0, resp1, resp2, resp3, resp4, resp5}
)

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
	MaxAmount = 1.0 / norm.MaxAmount
	MaxInstallments = 1.0 / norm.MaxInstallments
	AmountVsAvgRatio = 1.0 / norm.AmountVsAvgRatio
	MaxMinutes = 1.0 / norm.MaxMinutes
	MaxKm = 1.0 / norm.MaxKm
	MaxTxCount24h = 1.0 / norm.MaxTxCount24h
	MaxMerchantAvgAmount = 1.0 / norm.MaxMerchantAvgAmount

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



var (
	keyTx           = []byte(`"transaction"`)
	keyCust         = []byte(`"customer"`)
	keyMerch        = []byte(`"merchant"`)
	keyTerm         = []byte(`"terminal"`)
	keyLastTx       = []byte(`"last_transaction"`)

	keyAmount       = []byte(`"amount"`)
	keyInst         = []byte(`"installments"`)
	keyReqAt        = []byte(`"requested_at"`)
	keyAvgAmount    = []byte(`"avg_amount"`)
	keyTxCount      = []byte(`"tx_count_24h"`)
	keyKnownMerch   = []byte(`"known_merchants"`)
	keyId           = []byte(`"id"`)
	keyMcc          = []byte(`"mcc"`)
	keyIsOnline     = []byte(`"is_online"`)
	keyCardPres     = []byte(`"card_present"`)
	keyKmHome       = []byte(`"km_from_home"`)
	keyTimestamp    = []byte(`"timestamp"`)
	keyKmCurr       = []byte(`"km_from_current"`)
)

func parseFloatFast(b []byte, start int) (float64, int) {
	var val float64
	var dec float64
	inDec := false
	div := 1.0
	i := start
	for ; i < len(b); i++ {
		ch := b[i]
		if ch >= '0' && ch <= '9' {
			if inDec {
				dec = dec*10 + float64(ch-'0')
				div *= 10
			} else {
				val = val*10 + float64(ch-'0')
			}
		} else if ch == '.' {
			inDec = true
		} else {
			break
		}
	}
	return val + dec/div, i
}

func parseIntFast(b []byte, start int) (int64, int) {
	var val int64
	i := start
	for ; i < len(b); i++ {
		ch := b[i]
		if ch >= '0' && ch <= '9' {
			val = val*10 + int64(ch-'0')
		} else {
			break
		}
	}
	return val, i
}

func parseBoolFast(b []byte, start int) (bool, int) {
	if start < len(b) && b[start] == 't' {
		return true, start+4
	}
	return false, start+5
}

func parseStringFast(b []byte, start int) ([]byte, int) {
	if start >= len(b) || b[start] != '"' {
		return nil, start
	}
	start++
	for i := start; i < len(b); i++ {
		if b[i] == '"' {
			return b[start:i], i + 1
		}
	}
	return nil, len(b)
}

func findAfter(b []byte, blockKey, valKey []byte) int {
	bIdx := bytes.Index(b, blockKey)
	if bIdx == -1 {
		return -1
	}
	vIdx := bytes.Index(b[bIdx:], valKey)
	if vIdx == -1 {
		return -1
	}
	start := bIdx + vIdx + len(valKey)
	for start < len(b) && (b[start] == ' ' || b[start] == ':' || b[start] == '\n' || b[start] == '\r') {
		start++
	}
	return start
}

func findDirect(b []byte, key []byte) int {
	idx := bytes.Index(b, key)
	if idx == -1 {
		return -1
	}
	start := idx + len(key)
	for start < len(b) && (b[start] == ' ' || b[start] == ':' || b[start] == '\n' || b[start] == '\r') {
		start++
	}
	return start
}

func getValFloat(b []byte, start int) float64 {
	if start == -1 {
		return 0
	}
	f, _ := parseFloatFast(b, start)
	return f
}

func getValInt(b []byte, start int) int64 {
	if start == -1 {
		return 0
	}
	v, _ := parseIntFast(b, start)
	return v
}

func getValBool(b []byte, start int) bool {
	if start == -1 {
		return false
	}
	v, _ := parseBoolFast(b, start)
	return v
}

func getValString(b []byte, start int) []byte {
	if start == -1 {
		return nil
	}
	v, _ := parseStringFast(b, start)
	return v
}

func fastParseTimeStr(s []byte) int64 {
	if len(s) < 19 {
		return 0
	}
	d2 := func(i int) int { return int(s[i]-'0')*10 + int(s[i+1]-'0') }

	year := int(s[0]-'0')*1000 + int(s[1]-'0')*100 + int(s[2]-'0')*10 + int(s[3]-'0')
	month := d2(5)
	day := d2(8)
	hour := d2(11)
	min := d2(14)
	sec := d2(17)

	y := year
	if month <= 2 {
		y--
	}
	var era int
	if y >= 0 {
		era = y / 400
	} else {
		era = (y - 399) / 400
	}
	yoe := y - era*400
	var m int
	if month > 2 {
		m = month - 3
	} else {
		m = month + 9
	}
	doy := (153*m+2)/5 + day - 1
	doe := yoe*365 + yoe/4 - yoe/100 + doy
	days := int64(era)*146097 + int64(doe) - 719468
	
	return days*86400 + int64(hour)*3600 + int64(min)*60 + int64(sec)
}

func fastVectorize(body []byte, q *[14]float32) {
	// 1. Transaction block
	txStart := bytes.Index(body, keyTx)
	var amt float64
	var inst int64
	var reqAtBytes []byte
	if txStart != -1 {
		txBlock := body[txStart:]
		amt = getValFloat(txBlock, findDirect(txBlock, keyAmount))
		inst = getValInt(txBlock, findDirect(txBlock, keyInst))
		reqAtBytes = getValString(txBlock, findDirect(txBlock, keyReqAt))
	}

	// 2. Customer block
	custStart := bytes.Index(body, keyCust)
	var cAvgAmt float64
	var txCount int64
	var knownMerchBlock []byte
	if custStart != -1 {
		custBlock := body[custStart:]
		cAvgAmt = getValFloat(custBlock, findDirect(custBlock, keyAvgAmount))
		txCount = getValInt(custBlock, findDirect(custBlock, keyTxCount))

		knownMerchStart := findDirect(custBlock, keyKnownMerch)
		if knownMerchStart != -1 && knownMerchStart < len(custBlock) && custBlock[knownMerchStart] == '[' {
			end := bytes.IndexByte(custBlock[knownMerchStart:], ']')
			if end != -1 {
				knownMerchBlock = custBlock[knownMerchStart : knownMerchStart+end+1]
			}
		}
	}

	// 3. Merchant block
	merchStart := bytes.Index(body, keyMerch)
	var merchId []byte
	var mccBytes []byte
	var mAvgAmt float64
	if merchStart != -1 {
		merchBlock := body[merchStart:]
		merchId = getValString(merchBlock, findDirect(merchBlock, keyId))
		mccBytes = getValString(merchBlock, findDirect(merchBlock, keyMcc))
		mAvgAmt = getValFloat(merchBlock, findDirect(merchBlock, keyAvgAmount))
	}

	// 4. Terminal block
	termStart := bytes.Index(body, keyTerm)
	var isOnline bool
	var cardPresent bool
	var kmHome float64
	if termStart != -1 {
		termBlock := body[termStart:]
		isOnline = getValBool(termBlock, findDirect(termBlock, keyIsOnline))
		cardPresent = getValBool(termBlock, findDirect(termBlock, keyCardPres))
		kmHome = getValFloat(termBlock, findDirect(termBlock, keyKmHome))
	}

	// 5. Last transaction block
	var lastTsBytes []byte
	var kmLast float64
	hasLastTx := false

	lastTxStart := bytes.Index(body, keyLastTx)
	if lastTxStart != -1 {
		lastTxBlock := body[lastTxStart:]
		nullIdx := bytes.Index(lastTxBlock, []byte("null"))
		timeStart := findAfter(lastTxBlock, keyLastTx, keyTimestamp)

		if timeStart != -1 && (nullIdx == -1 || timeStart < nullIdx) {
			hasLastTx = true
			lastTsBytes = getValString(lastTxBlock, timeStart)
			kmLast = getValFloat(lastTxBlock, findAfter(lastTxBlock, keyLastTx, keyKmCurr))
		}
	}

	known := false
	if len(knownMerchBlock) > 0 && len(merchId) > 0 {
		known = bytes.Contains(knownMerchBlock, merchId)
	}

	reqAtUnix := fastParseTimeStr(reqAtBytes)
	reqHour := int((reqAtUnix % 86400) / 3600)
	if reqHour < 0 {
		reqHour += 24
	}
	days := reqAtUnix / 86400
	if reqAtUnix < 0 && reqAtUnix%86400 != 0 {
		days--
	}
	reqWeekday := int((days + 3) % 7)
	if reqWeekday < 0 {
		reqWeekday += 7
	}

	q[0] = clamp(amt * MaxAmount)
	q[1] = clamp(float64(inst) * MaxInstallments)
	if cAvgAmt > 0 {
		q[2] = clamp((amt / cAvgAmt) * AmountVsAvgRatio)
	} else {
		q[2] = 1.0
	}
	q[3] = float32(reqHour) / 23.0
	q[4] = float32(reqWeekday) / 6.0

	if !hasLastTx || len(lastTsBytes) == 0 {
		q[5] = -1.0
		q[6] = -1.0
	} else {
		lastTsUnix := fastParseTimeStr(lastTsBytes)
		minutes := float64(reqAtUnix-lastTsUnix) / 60.0
		q[5] = clamp(minutes * MaxMinutes)
		q[6] = clamp(kmLast * MaxKm)
	}

	q[7] = clamp(kmHome * MaxKm)
	q[8] = clamp(float64(txCount) * MaxTxCount24h)
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
		q[12] = MccRiskArr[m]
	} else {
		q[12] = 0.5
	}
	q[13] = clamp(mAvgAmt * MaxMerchantAvgAmount)
}

func writeResp(fd int, epfd int, resp []byte) bool {
	_, err := unix.Write(fd, resp)
	if err != nil {
		unix.EpollCtl(epfd, unix.EPOLL_CTL_DEL, fd, nil)
		unix.Close(fd)
		return false
	}
	return true
}

func handleRequest(fd int, data []byte, q *[14]float32, scratch *[131072]byte, epfd int) {
	var ok bool
	var bodyIdx int = -1
	if bytes.HasPrefix(data, []byte("POST /fraud-score")) {
		bodyIdx = bytes.Index(data, []byte("\r\n\r\n"))
		if bodyIdx != -1 {
			body := data[bodyIdx+4:]

			fastVectorize(body, q)

			frauds := engine.SearchVectorFast(&q[0], &scratch[0])

			if uint32(frauds) <= 5 {
				ok = writeResp(fd, epfd, resps[frauds])
			} else {
				ok = writeResp(fd, epfd, resp3)
			}
		} else {
			ok = writeResp(fd, epfd, resp404)
		}
	} else if bytes.HasPrefix(data, []byte("GET /ready")) {
		ok = writeResp(fd, epfd, respReady)
	} else {
		ok = writeResp(fd, epfd, resp404)
	}

	if !ok {
		return
	}

	headerPart := data
	if bodyIdx != -1 {
		headerPart = data[:bodyIdx]
	} else if idx := bytes.Index(data, []byte("\r\n\r\n")); idx != -1 {
		headerPart = data[:idx]
	}

	if (bytes.IndexByte(headerPart, 'c') != -1 || bytes.IndexByte(headerPart, 'C') != -1) && bytes.Contains(headerPart, []byte("close")) {
		unix.EpollCtl(epfd, unix.EPOLL_CTL_DEL, fd, nil)
		unix.Close(fd)
	}
}

func main() {
	runtime.GOMAXPROCS(1)
	runtime.LockOSThread()
	loadConfig()

	datasetPath := os.Getenv("DATASET_PATH")
	if datasetPath == "" {
		datasetPath = "dataset.bin"
	}

	sharedDatasetPath := os.Getenv("SHARED_DATASET_PATH")
	if sharedDatasetPath != "" {
		if os.Getenv("SOCKET_PATH") == "/tmp/sockets/api1.sock" {
			if _, err := os.Stat(sharedDatasetPath); os.IsNotExist(err) {
				tmpPath := sharedDatasetPath + ".tmp"
				src, err := os.Open(datasetPath)
				if err == nil {
					dst, err2 := os.Create(tmpPath)
					if err2 == nil {
						_, errCopy := io.Copy(dst, src)
						dst.Close()
						if errCopy == nil {
							os.Rename(tmpPath, sharedDatasetPath)
						} else {
							log.Printf("copy error: %v", errCopy)
						}
					}
					src.Close()
				}
			}
		} else {
			for {
				if _, err := os.Stat(sharedDatasetPath); err == nil {
					break
				}
				time.Sleep(10 * time.Millisecond)
			}
		}
		datasetPath = sharedDatasetPath
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
	unix.SetsockoptInt(uds_fd, unix.SOL_SOCKET, unix.SO_RCVBUF, 16*1024*1024)
	if err := os.Chmod(socketPath, 0777); err != nil {
		log.Fatalf("chmod error: %v", err)
	}
	if err := unix.SetNonblock(uds_fd, true); err != nil {
		log.Fatalf("setnonblock error: %v", err)
	}

	debug.SetGCPercent(-1)

	epfd, err := unix.EpollCreate1(0)
	if err != nil {
		log.Fatalf("epoll_create1 error: %v", err)
	}

	event := &unix.EpollEvent{
		Events: unix.EPOLLIN,
		Fd:     int32(uds_fd),
	}
	if err := unix.EpollCtl(epfd, unix.EPOLL_CTL_ADD, uds_fd, event); err != nil {
		log.Fatalf("epoll_ctl error: %v", err)
	}

	events := make([]unix.EpollEvent, 4096)
	buf := make([]byte, 8192)
	oob := make([]byte, unix.CmsgSpace(16*4))
	dummy := make([]byte, 1)

	var globalQuery [14]float32
	var globalScratch [131072]byte

	for {
		n, err := unix.EpollWait(epfd, events, 0)
		if err == unix.EINTR {
			continue
		}
		if n == 0 && err == nil {
			for s := 0; s < 500; s++ {
				engine.Pause()
				engine.Pause()
				n, err = unix.EpollWait(epfd, events, 0)
				if n > 0 || err != nil {
					break
				}
			}
		}
		if n == 0 && (err == nil || err == unix.EINTR) {
			n, err = unix.EpollWait(epfd, events, -1)
			if err == unix.EINTR {
				continue
			}
		}
		if err != nil {
			log.Fatalf("epoll_wait error: %v", err)
		}

		for i := 0; i < n; i++ {
			fd := int(events[i].Fd)

			if fd == uds_fd {
				// Drain all pending FD batches from UDS
				for {
					_, oobn, _, _, err := unix.Recvmsg(uds_fd, dummy, oob, unix.MSG_DONTWAIT)
					if err != nil {
						break
					}
					msgs, err := unix.ParseSocketControlMessage(oob[:oobn])
					if err != nil || len(msgs) == 0 {
						break
					}
					fds, err := unix.ParseUnixRights(&msgs[0])
					if err != nil || len(fds) == 0 {
						break
					}

					for _, client_fd := range fds {
						unix.SetsockoptInt(client_fd, unix.IPPROTO_TCP, unix.TCP_NODELAY, 1)
						unix.SetsockoptInt(client_fd, unix.IPPROTO_TCP, unix.TCP_QUICKACK, 1)

						unix.EpollCtl(epfd, unix.EPOLL_CTL_ADD, client_fd, &unix.EpollEvent{Events: unix.EPOLLIN, Fd: int32(client_fd)})

						// Leitura inline imediata (com TCP_DEFER_ACCEPT o payload já está no buffer)
						rn, err := unix.Read(client_fd, buf)
						if rn > 0 {
							handleRequest(client_fd, buf[:rn], &globalQuery, &globalScratch, epfd)
						} else if err != nil && err != unix.EAGAIN && err != unix.EWOULDBLOCK {
							unix.EpollCtl(epfd, unix.EPOLL_CTL_DEL, client_fd, nil)
							unix.Close(client_fd)
						}
					}
				}
				continue
			}

			// Client socket
			rn, err := unix.Read(fd, buf)
			if err != nil {
				if err == unix.EAGAIN || err == unix.EWOULDBLOCK {
					continue
				}
				unix.EpollCtl(epfd, unix.EPOLL_CTL_DEL, fd, nil)
				unix.Close(fd)
				continue
			}
			if rn <= 0 {
				unix.EpollCtl(epfd, unix.EPOLL_CTL_DEL, fd, nil)
				unix.Close(fd)
				continue
			}

			handleRequest(fd, buf[:rn], &globalQuery, &globalScratch, epfd)
		}
	}
}
