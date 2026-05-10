package main

import (
	"compress/gzip"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"log"
	"os"
)

type Record struct {
	Vector []float64 `json:"vector"`
	Label  string    `json:"label"`
}

type Point struct {
	Vector [14]float32
	Index  uint32
	Label  uint8
	_pad   [3]byte
}

type ClusterHeader struct {
	Centroid [14]float32
	RadiusSq float32
	Offset   uint32
	Count    uint32
	_pad     [12]byte
}

const K = 1 // Brute Force total

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

	fmt.Println("Carregando vetores (brutos f32)...")
	var idx uint32 = 0
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
		p.Index = idx
		idx++
		points = append(points, p)
	}
	gz.Close()
	file.Close()

	n := len(points)
	headers := make([]ClusterHeader, 1)
	headers[0].Count = uint32(n)
	headers[0].Offset = 0
	headers[0].RadiusSq = 1e30 // Never prune

	fmt.Println("Gravando dataset.bin (Brute Force)...")
	out, _ := os.Create("dataset.bin")
	binary.Write(out, binary.LittleEndian, uint32(1))
	padding := make([]byte, 60)
	out.Write(padding)
	binary.Write(out, binary.LittleEndian, headers)
	binary.Write(out, binary.LittleEndian, points)
	out.Close()
	fmt.Println("Concluído!")
}
