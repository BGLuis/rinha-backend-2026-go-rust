import gzip
import json
import struct
import os
import numpy as np
from sklearn.cluster import MiniBatchKMeans

def stream_json_array(f):
    while True:
        char = f.read(1)
        if not char: return
        if char == '[': break
    
    buffer = ""
    depth = 0
    while True:
        char = f.read(1)
        if not char: break
        if char == '{': depth += 1
        if depth > 0: buffer += char
        if char == '}':
            depth -= 1
            if depth == 0:
                yield json.loads(buffer)
                buffer = ""
        elif depth == 0 and char == ',':
            continue

def main():
    print("Iniciando pré-processamento IVF (Python)...")
    references_path = "resources/references.json.gz"
    output_path = "dataset.bin"
    num_clusters = 1024
    
    sample_size = 100000
    sample_data = []
    
    print("Coletando amostra para treinamento do K-Means...")
    with gzip.open(references_path, "rt") as f:
        count = 0
        for item in stream_json_array(f):
            sample_data.append(item['vector'])
            count += 1
            if count >= sample_size:
                break
                
    X_sample = np.array(sample_data, dtype=np.float32)
    print("Treinando MiniBatchKMeans...")
    kmeans = MiniBatchKMeans(n_clusters=num_clusters, batch_size=10000, n_init="auto", random_state=42)
    kmeans.fit(X_sample)
    
    centroids = kmeans.cluster_centers_
    quantized_centroids = np.round(centroids * 8192).astype(np.int16)
    
    tmp_dir = "clusters_tmp"
    os.makedirs(tmp_dir, exist_ok=True)
    cluster_files = [open(f"{tmp_dir}/cluster_{i}.bin", "wb") for i in range(num_clusters)]
    cluster_counts = [0] * num_clusters
    
    print("Processando e particionando todos os vetores...")
    batch_size = 50000
    batch_vectors = []
    batch_labels = []
    
    def process_batch(vectors, labels):
        X_batch = np.array(vectors, dtype=np.float32)
        cluster_ids = kmeans.predict(X_batch)
        X_quantized = np.round(X_batch * 8192).astype(np.int16)
        
        fmt = "<14hb3x"
        for i in range(len(vectors)):
            cid = cluster_ids[i]
            label = 1 if labels[i] == "fraud" else 0
            data = struct.pack(fmt, *X_quantized[i], label)
            cluster_files[cid].write(data)
            cluster_counts[cid] += 1
            
    with gzip.open(references_path, "rt") as f:
        count = 0
        for item in stream_json_array(f):
            batch_vectors.append(item['vector'])
            batch_labels.append(item['label'])
            count += 1
            
            if len(batch_vectors) >= batch_size:
                process_batch(batch_vectors, batch_labels)
                batch_vectors.clear()
                batch_labels.clear()
                print(f"Processados {count} registros...")
                
        if batch_vectors:
            process_batch(batch_vectors, batch_labels)
            print(f"Processados {count} registros...")

    for cf in cluster_files:
        cf.close()
        
    print("Gerando arquivo binário final (dataset.bin)...")
    with open(output_path, "wb") as out:
        out.write(struct.pack("<II", num_clusters, 14))
        
        for c in quantized_centroids:
            out.write(struct.pack("<14h", *c))
            
        current_offset = 0
        offsets = []
        for i in range(num_clusters):
            offsets.append(current_offset)
            current_offset += cluster_counts[i]
            
        for i in range(num_clusters):
            out.write(struct.pack("<II", offsets[i], cluster_counts[i]))
            
        for i in range(num_clusters):
            with open(f"{tmp_dir}/cluster_{i}.bin", "rb") as cf:
                out.write(cf.read())
            os.remove(f"{tmp_dir}/cluster_{i}.bin")
            
    os.rmdir(tmp_dir)
    print(f"Pré-processamento concluído! Total: {count}")

if __name__ == "__main__":
    main()
