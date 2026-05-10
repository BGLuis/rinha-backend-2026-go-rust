package main

import (
	"compress/gzip"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"log"
	"math"
	"os"
)

type Record struct {
	Vector []float64 `json:"vector"`
	Label  string    `json:"label"`
}

type Point struct {
	Vector [14]float32
	Label  uint8
	_pad   [7]byte
}

type ClusterHeader struct {
	Centroid [14]float32
	RadiusSq float32
	Offset   uint32
	Count    uint32
	_pad     [12]byte
}

const K = 2048

func calcDistSq(a, b *[14]float32) float32 {
	var sum float32
	for i := 0; i < 14; i++ {
		diff := a[i] - b[i]
		sum += diff * diff
	}
	return sum
}

func round4(v float64) float32 {
	return float32(math.Round(v*10000) / 10000)
}

func main() {
	file, err := os.Open("resources/references.json.gz")
	if err != nil {
		log.Fatal(err)
	}
	gz, _ := gzip.NewReader(file)
	decoder := json.NewDecoder(gz)
	decoder.Token()

	const N = 3000000
	points := make([]Point, 0, N)

	fmt.Println("Carregando vetores...")
	for decoder.More() {
		var r Record
		decoder.Decode(&r)
		var p Point
		for i := 0; i < 14; i++ {
			p.Vector[i] = float32(r.Vector[i])
		}
		if r.Label == "fraud" {
			p.Label = 1
		}
		points = append(points, p)
	}
	gz.Close()
	file.Close()

	n := len(points)
	centroids := make([][14]float32, K)
	for i := 0; i < K; i++ {
		centroids[i] = points[(i*2931)%n].Vector
	}

	assignments := make([]int, n)
	for iter := 0; iter < 10; iter++ {
		fmt.Printf("K-Means iter %d...\n", iter+1)
		for i := 0; i < n; i++ {
			bestD2 := float32(1e30)
			bestC := 0
			for c := 0; c < K; c++ {
				d2 := calcDistSq(&points[i].Vector, &centroids[c])
				if d2 < bestD2 {
					bestD2 = d2
					bestC = c
				}
			}
			assignments[i] = bestC
		}
		newCentroids := make([][14]float32, K)
		counts := make([]int, K)
		for i := 0; i < n; i++ {
			c := assignments[i]
			for d := 0; d < 14; d++ {
				newCentroids[c][d] += points[i].Vector[d]
			}
			counts[c]++
		}
		for c := 0; c < K; c++ {
			if counts[c] > 0 {
				for d := 0; d < 14; d++ {
					centroids[c][d] = newCentroids[c][d] / float32(counts[c])
				}
			}
		}
	}

	headers := make([]ClusterHeader, K)
	for c := 0; c < K; c++ {
		headers[c].Centroid = centroids[c]
	}

	for i := 0; i < n; i++ {
		c := assignments[i]
		headers[c].Count++
		d2 := calcDistSq(&points[i].Vector, &centroids[c])
		if d2 > headers[c].RadiusSq {
			headers[c].RadiusSq = d2
		}
	}

	var offset uint32 = 0
	for c := 0; c < K; c++ {
		headers[c].Offset = offset
		offset += headers[c].Count
	}

	reordered := make([]Point, n)
	insertPos := make([]uint32, K)
	for c := 0; c < K; c++ {
		insertPos[c] = headers[c].Offset
	}
	for i := 0; i < n; i++ {
		c := assignments[i]
		reordered[insertPos[c]] = points[i]
		insertPos[c]++
	}

	fmt.Println("Gravando dataset.bin...")
	out, _ := os.Create("dataset.bin")
	binary.Write(out, binary.LittleEndian, uint32(K))
	padding := make([]byte, 60) // Align to 64
	out.Write(padding)
	binary.Write(out, binary.LittleEndian, headers)
	binary.Write(out, binary.LittleEndian, reordered)
	out.Close()
	fmt.Println("Concluído!")
}
