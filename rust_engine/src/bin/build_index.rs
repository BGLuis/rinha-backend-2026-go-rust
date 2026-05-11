use serde::Deserialize;
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use flate2::read::GzDecoder;
use rayon::prelude::*;
use rand::seq::SliceRandom;

#[derive(Deserialize)]
struct RawEntry {
    vector: [f32; 14],
    label: String,
}

struct Entry {
    v_i16: [i16; 16],
    meta: u32,
    v_f32: [f32; 16],
}

fn main() {
    let references_path = "resources/references.json.gz";
    let example_path = "resources/example-references.json";
    
    // Check if references_path is an LFS pointer
    let is_lfs = {
        let f = File::open(references_path).expect("Failed to open references.json.gz");
        let mut buf = [0u8; 100];
        use std::io::Read;
        let n = (&f).read(&mut buf).unwrap_or(0);
        let s = String::from_utf8_lossy(&buf[..n]);
        s.contains("version https://git-lfs.github.com/spec/v1")
    };

    let raw_entries: Vec<RawEntry> = if is_lfs {
        println!("references.json.gz is an LFS pointer. Falling back to {}...", example_path);
        let file = File::open(example_path).expect("Failed to open example-references.json");
        let reader = BufReader::new(file);
        serde_json::from_reader(reader).expect("Failed to parse example JSON")
    } else {
        println!("Loading {}...", references_path);
        let file = File::open(references_path).expect("Failed to open references.json.gz");
        let decoder = GzDecoder::new(file);
        let reader = BufReader::new(decoder);
        serde_json::from_reader(reader).expect("Failed to parse gzipped JSON")
    };

    println!("Loaded {} entries", raw_entries.len());

    let entries: Vec<Entry> = raw_entries.into_par_iter().enumerate().map(|(i, e)| {
        let mut v_i16 = [0i16; 16];
        let mut v_f32 = [0.0f32; 16];
        for d in 0..14 {
            v_i16[d] = (e.vector[d] * 10000.0).round() as i16;
            v_f32[d] = e.vector[d];
        }
        let label_bit = if e.label == "fraud" { 1u32 } else { 0u32 };
        let meta = (label_bit << 31) | (i as u32 & 0x7FFFFFFF);
        Entry { v_i16, meta, v_f32 }
    }).collect();

    let k = 1024;
    println!("Clustering into {} clusters...", k);
    
    // Simple K-Means
    let mut rng = rand::thread_rng();
    let mut centroids: Vec<[f32; 16]> = entries.choose_multiple(&mut rng, k)
        .map(|e| e.v_f32)
        .collect();

    for iter in 0..10 {
        let next_centroids: Vec<([f64; 16], usize)> = entries.par_iter()
            .fold(|| vec![([0.0f64; 16], 0usize); k], |mut acc, e| {
                let mut best_d2 = f32::MAX;
                let mut best_ki = 0;
                for (ki, c) in centroids.iter().enumerate() {
                    let mut d2 = 0.0f32;
                    for d in 0..14 {
                        let diff = e.v_f32[d] - c[d];
                        d2 += diff * diff;
                    }
                    if d2 < best_d2 {
                        best_d2 = d2;
                        best_ki = ki;
                    }
                }
                for d in 0..14 {
                    acc[best_ki].0[d] += e.v_f32[d] as f64;
                }
                acc[best_ki].1 += 1;
                acc
            })
            .reduce(|| vec![([0.0f64; 16], 0usize); k], |mut a, b| {
                for i in 0..k {
                    for d in 0..14 {
                        a[i].0[d] += b[i].0[d];
                    }
                    a[i].1 += b[i].1;
                }
                a
            });

        for ki in 0..k {
            if next_centroids[ki].1 > 0 {
                for d in 0..14 {
                    centroids[ki][d] = (next_centroids[ki].0[d] / next_centroids[ki].1 as f64) as f32;
                }
            }
        }
        println!("Iteration {} complete", iter + 1);
    }

    println!("Assigning points to clusters and calculating BBoxes...");
    let mut clusters: Vec<Vec<&Entry>> = vec![Vec::new(); k];
    let mut assignments: Vec<usize> = vec![0; entries.len()];
    
    assignments.par_iter_mut().enumerate().for_each(|(i, ki)| {
        let e = &entries[i];
        let mut best_d2 = f32::MAX;
        let mut best_ki = 0;
        for (j, c) in centroids.iter().enumerate() {
            let mut d2 = 0.0f32;
            for d in 0..14 {
                let diff = e.v_f32[d] - c[d];
                d2 += diff * diff;
            }
            if d2 < best_d2 {
                best_d2 = d2;
                best_ki = j;
            }
        }
        *ki = best_ki;
    });

    for (i, &ki) in assignments.iter().enumerate() {
        clusters[ki].push(&entries[i]);
    }

    let mut bboxes = vec![[0i16; 32]; k];
    for ki in 0..k {
        let mut mins = [i16::MAX; 16];
        let mut maxs = [i16::MIN; 16];
        for e in &clusters[ki] {
            for d in 0..16 {
                if e.v_i16[d] < mins[d] { mins[d] = e.v_i16[d]; }
                if e.v_i16[d] > maxs[d] { maxs[d] = e.v_i16[d]; }
            }
        }
        bboxes[ki][0..16].copy_from_slice(&mins);
        bboxes[ki][16..32].copy_from_slice(&maxs);
    }

    println!("Writing dataset.bin to {}...", std::env::current_dir().unwrap().display());
    let out_file = File::create("dataset.bin").expect("Failed to create dataset.bin");
    let mut writer = BufWriter::new(out_file);

    // Header (64 bytes)
    let mut header = [0u32; 16];
    header[0] = 0x4E495452; // "NITR"
    header[1] = 1; // version
    header[2] = k as u32;
    header[3] = entries.len() as u32;
    
    let header_u8: &[u8] = unsafe {
        std::slice::from_raw_parts(header.as_ptr() as *const u8, 64)
    };
    writer.write_all(header_u8).unwrap();

    // Centroids (k * 64 bytes)
    for c in &centroids {
        let c_u8: &[u8] = unsafe {
            std::slice::from_raw_parts(c.as_ptr() as *const u8, 64)
        };
        writer.write_all(c_u8).unwrap();
    }

    // BBoxes (k * 64 bytes)
    for b in &bboxes {
        let b_u8: &[u8] = unsafe {
            std::slice::from_raw_parts(b.as_ptr() as *const u8, 64)
        };
        writer.write_all(b_u8).unwrap();
    }

    // Offsets (k * 4 bytes)
    let mut current_offset = 64 + k * 64 + k * 64 + k * 4 + k * 4;
    let mut offsets = vec![0u32; k];
    for ki in 0..k {
        offsets[ki] = current_offset as u32;
        current_offset += clusters[ki].len() * 36;
    }
    let offsets_u8: &[u8] = unsafe {
        std::slice::from_raw_parts(offsets.as_ptr() as *const u8, k * 4)
    };
    writer.write_all(offsets_u8).unwrap();

    // Sizes (k * 4 bytes)
    let mut sizes = vec![0u32; k];
    for ki in 0..k {
        sizes[ki] = clusters[ki].len() as u32;
    }
    let sizes_u8: &[u8] = unsafe {
        std::slice::from_raw_parts(sizes.as_ptr() as *const u8, k * 4)
    };
    writer.write_all(sizes_u8).unwrap();

    // Data
    for ki in 0..k {
        for e in &clusters[ki] {
            let v_u8: &[u8] = unsafe {
                std::slice::from_raw_parts(e.v_i16.as_ptr() as *const u8, 32)
            };
            writer.write_all(v_u8).unwrap();
            writer.write_all(&e.meta.to_ne_bytes()).unwrap();
        }
    }

    writer.flush().unwrap();
    println!("Done! dataset.bin generated successfully.");
}
