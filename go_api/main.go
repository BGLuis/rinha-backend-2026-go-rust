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

func clampI16(v float64) int16 {
	if v <= 0 {
		return 0
	}
	if v >= 1.0 {
		return 10000
	}
	return int16(v*10000.0 + 0.5)
}

func fastParseTimeStr(s []byte) int64 {
	if len(s) < 19 {
		return 0
	}
	year := int(s[0]-'0')*1000 + int(s[1]-'0')*100 + int(s[2]-'0')*10 + int(s[3]-'0')
	month := int(s[5]-'0')*10 + int(s[6]-'0')
	day := int(s[8]-'0')*10 + int(s[9]-'0')
	hour := int(s[11]-'0')*10 + int(s[12]-'0')
	min := int(s[14]-'0')*10 + int(s[15]-'0')
	sec := int(s[17]-'0')*10 + int(s[18]-'0')

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

func fastVectorize(body []byte, q *[16]int16) {
	var (
		amt, inst                                   float64
		reqAtUnix                                   int64
		cAvgAmt, txCount                            float64
		mAvgAmt                                     float64
		mccCode                                     int
		isOnline, cardPresent                       bool
		kmHome, kmLast                              float64
		hasLastTx                                   bool
		lastTsUnix                                  int64
		known                                       bool
		merchIdStart, merchIdLen                    int
		knownMerchStart, knownMerchLen              int
	)

	L := len(body)
	i := 0
	for i < L {
		if body[i] != '"' {
			i++
			continue
		}
		i++ // skip open quote
		kStart := i
		for i < L && body[i] != '"' {
			i++
		}
		kLen := i - kStart
		i++ // skip close quote

		for i < L && (body[i] == ':' || body[i] == ' ' || body[i] == '\t' || body[i] == '\r' || body[i] == '\n') {
			i++
		}
		if i >= L {
			break
		}

		vStart := i

		switch kLen {
		case 2: // id
			if body[kStart] == 'i' && body[kStart+1] == 'd' {
				if body[vStart] == '"' {
					vStart++
					end := bytes.IndexByte(body[vStart:], '"')
					if end != -1 {
						merchIdStart = vStart
						merchIdLen = end
						i = vStart + end + 1
					}
				}
			}
		case 3: // mcc
			if body[kStart] == 'm' && body[kStart+1] == 'c' {
				if body[vStart] == '"' {
					vStart++
					end := bytes.IndexByte(body[vStart:], '"')
					if end != -1 {
						m := 0
						for p := vStart; p < vStart+end; p++ {
							if body[p] >= '0' && body[p] <= '9' {
								m = m*10 + int(body[p]-'0')
							}
						}
						mccCode = m
						i = vStart + end + 1
					}
				}
			}
		case 6: // amount
			if body[kStart] == 'a' && body[kStart+1] == 'm' {
				amt, i = parseFloatFast(body, vStart)
			}
		case 9: // is_online, timestamp
			if body[kStart] == 'i' { // is_online
				isOnline, i = parseBoolFast(body, vStart)
			} else if body[kStart] == 't' { // timestamp
				if body[vStart] == '"' {
					vStart++
					end := bytes.IndexByte(body[vStart:], '"')
					if end != -1 {
						lastTsUnix = fastParseTimeStr(body[vStart : vStart+end])
						hasLastTx = true
						i = vStart + end + 1
					}
				}
			}
		case 10: // avg_amount
			if body[kStart] == 'a' {
				f, next := parseFloatFast(body, vStart)
				i = next
				if cAvgAmt == 0 {
					cAvgAmt = f
				} else {
					mAvgAmt = f
				}
			}
		case 12: // installments, requested_at, tx_count_24h, card_present, km_from_home
			switch body[kStart] {
			case 'i': // installments
				var v int64
				v, i = parseIntFast(body, vStart)
				inst = float64(v)
			case 'r': // requested_at
				if body[vStart] == '"' {
					vStart++
					end := bytes.IndexByte(body[vStart:], '"')
					if end != -1 {
						reqAtUnix = fastParseTimeStr(body[vStart : vStart+end])
						i = vStart + end + 1
					}
				}
			case 't': // tx_count_24h
				var v int64
				v, i = parseIntFast(body, vStart)
				txCount = float64(v)
			case 'c': // card_present
				cardPresent, i = parseBoolFast(body, vStart)
			case 'k': // km_from_home
				kmHome, i = parseFloatFast(body, vStart)
			}
		case 15: // known_merchants, km_from_current
			if body[kStart] == 'k' {
				if body[kStart+1] == 'n' { // known_merchants
					end := bytes.IndexByte(body[vStart:], ']')
					if end != -1 {
						knownMerchStart = vStart
						knownMerchLen = end + 1
						i = vStart + end + 1
					}
				} else { // km_from_current
					kmLast, i = parseFloatFast(body, vStart)
				}
			}
		case 16: // last_transaction
			if body[kStart] == 'l' {
				if body[vStart] == 'n' { // null
					hasLastTx = false
					i = vStart + 4
				}
			}
		}
	}

	if knownMerchLen > 0 && merchIdLen > 0 {
		known = bytes.Contains(body[knownMerchStart:knownMerchStart+knownMerchLen], body[merchIdStart:merchIdStart+merchIdLen])
	}

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

	q[0] = clampI16(amt * MaxAmount)
	q[1] = clampI16(inst * MaxInstallments)
	if cAvgAmt > 0 {
		q[2] = clampI16((amt / cAvgAmt) * AmountVsAvgRatio)
	} else {
		q[2] = 10000
	}
	q[3] = int16((float32(reqHour)/23.0)*10000.0 + 0.5)
	q[4] = int16((float32(reqWeekday)/6.0)*10000.0 + 0.5)

	if !hasLastTx {
		q[5] = -10000
		q[6] = -10000
	} else {
		minutes := float64(reqAtUnix-lastTsUnix) / 60.0
		q[5] = clampI16(minutes * MaxMinutes)
		q[6] = clampI16(kmLast * MaxKm)
	}

	q[7] = clampI16(kmHome * MaxKm)
	q[8] = clampI16(txCount * MaxTxCount24h)
	if isOnline {
		q[9] = 10000
	} else {
		q[9] = 0
	}
	if cardPresent {
		q[10] = 10000
	} else {
		q[10] = 0
	}
	if !known {
		q[11] = 10000
	} else {
		q[11] = 0
	}

	if mccCode < 10000 {
		q[12] = int16(MccRiskArr[mccCode]*10000.0 + 0.5)
	} else {
		q[12] = 5000
	}
	q[13] = clampI16(mAvgAmt * MaxMerchantAvgAmount)
	q[14] = 0
	q[15] = 0
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

func handleRequest(fd int, data []byte, q *[16]int16, scratch *[131072]byte, epfd int) {
	var ok bool
	var bodyIdx int = -1
	if len(data) >= 17 && data[0] == 'P' && data[1] == 'O' && data[2] == 'S' && data[3] == 'T' && data[5] == '/' && data[6] == 'f' {
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
	} else if len(data) >= 10 && data[0] == 'G' && data[1] == 'E' && data[2] == 'T' && data[4] == '/' && data[5] == 'r' {
		ok = writeResp(fd, epfd, respReady)
	} else {
		ok = writeResp(fd, epfd, resp404)
	}

	if !ok {
		return
	}

	if bodyIdx != -1 && bytes.Contains(data[:bodyIdx], []byte("close")) {
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

	var globalQuery [16]int16
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
