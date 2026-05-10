package main

import (
	"compress/gzip"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"io"
	"log"
	"math"
	"os"
)

type Record struct {
	Vector []float64 `json:"vector"`
	Label  string    `json:"label"`
}

func main() {
	numClusters := 1024
	numCentroidSamples := 1024
	
	centroids := make([][14]int16, numClusters)
	
	file, err := os.Open("resources/references.json.gz")
	if err != nil {
		log.Fatal(err)
	}
	gz, _ := gzip.NewReader(file)
	decoder := json.NewDecoder(gz)
	decoder.Token()
	
	count := 0
	for decoder.More() && count < numCentroidSamples {
		var r Record
		decoder.Decode(&r)
		for i := 0; i < 14; i++ {
			centroids[count][i] = int16(math.Round(r.Vector[i] * 8192.0))
		}
		count++
	}
	gz.Close()
	file.Close()

	clusterFiles := make([]*os.File, numClusters)
	clusterCounts := make([]uint32, numClusters)
	for i := 0; i < numClusters; i++ {
		f, _ := os.Create(fmt.Sprintf("cluster_%d.bin", i))
		clusterFiles[i] = f
	}

	file, _ = os.Open("resources/references.json.gz")
	gz, _ = gzip.NewReader(file)
	decoder = json.NewDecoder(gz)
	decoder.Token()

	totalCount := 0
	recordBuf := make([]byte, 32)
	for decoder.More() {
		var r Record
		decoder.Decode(&r)
		
		var v [14]int16
		for i := 0; i < 14; i++ {
			v[i] = int16(math.Round(r.Vector[i] * 8192.0))
		}

		bestCluster := 0
		minDist := int32(math.MaxInt32)
		for c := 0; c < numClusters; c++ {
			var dist int32 = 0
			for i := 0; i < 14; i++ {
				diff := int32(v[i]) - int32(centroids[c][i])
				dist += diff * diff
			}
			if dist < minDist {
				minDist = dist
				bestCluster = c
			}
		}

		for i := 0; i < 14; i++ {
			binary.LittleEndian.PutUint16(recordBuf[i*2:i*2+2], uint16(v[i]))
		}
		if r.Label == "fraud" {
			recordBuf[28] = 1
		} else {
			recordBuf[28] = 0
		}
		recordBuf[29], recordBuf[30], recordBuf[31] = 0, 0, 0
		
		clusterFiles[bestCluster].Write(recordBuf)
		clusterCounts[bestCluster]++

		totalCount++
	}
	
	for _, f := range clusterFiles {
		f.Close()
	}
	gz.Close()
	file.Close()

	out, _ := os.Create("dataset.bin")
	
	binary.Write(out, binary.LittleEndian, uint32(numClusters))
	binary.Write(out, binary.LittleEndian, uint32(14))

	for i := 0; i < numClusters; i++ {
		binary.Write(out, binary.LittleEndian, centroids[i])
	}

	currentOffset := uint32(0)
	for i := 0; i < numClusters; i++ {
		binary.Write(out, binary.LittleEndian, currentOffset)
		binary.Write(out, binary.LittleEndian, clusterCounts[i])
		currentOffset += clusterCounts[i]
	}

	for i := 0; i < numClusters; i++ {
		name := fmt.Sprintf("cluster_%d.bin", i)
		f, _ := os.Open(name)
		io.Copy(out, f)
		f.Close()
		os.Remove(name)
	}
	out.Close()
}
