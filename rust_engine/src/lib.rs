use std::ffi::CStr;
use std::fs::File;
use std::os::raw::c_char;
use std::slice;
use memmap2::{Mmap, MmapOptions};

static mut DATASET_MMAP: Option<Mmap> = None;
static mut NUM_CLUSTERS: u32 = 0;
static mut CENTROIDS: Option<&'static [i16]> = None;
static mut OFFSETS: Option<&'static [(u32, u32)]> = None;

const RECORD_SIZE: usize = 32;

#[no_mangle]
pub extern "C" fn init_engine(path_ptr: *const c_char) -> i32 {
    unsafe {
        let c_str = CStr::from_ptr(path_ptr);
        let path = match c_str.to_str() {
            Ok(s) => s,
            Err(_) => return -1,
        };

        let file = match File::open(path) {
            Ok(f) => f,
            Err(_) => return -2,
        };

        let mmap = match MmapOptions::new().map(&file) {
            Ok(m) => m,
            Err(_) => return -3,
        };
        
        let mmap_ptr = mmap.as_ptr();
        
        let num_clusters = u32::from_le_bytes(mmap_ptr.add(0).cast::<[u8; 4]>().read());
        NUM_CLUSTERS = num_clusters;
        
        let centroids_size = (num_clusters as usize) * 14;
        let centroids_ptr = mmap_ptr.add(8).cast::<i16>();
        CENTROIDS = Some(slice::from_raw_parts(centroids_ptr, centroids_size));
        
        let offsets_offset = 8 + centroids_size * 2;
        let offsets_ptr = mmap_ptr.add(offsets_offset).cast::<(u32, u32)>();
        OFFSETS = Some(slice::from_raw_parts(offsets_ptr, num_clusters as usize));

        DATASET_MMAP = Some(mmap);
        0
    }
}

#[no_mangle]
pub extern "C" fn search(query_ptr: *const i16) -> i32 {
    let query: &[i16] = unsafe { slice::from_raw_parts(query_ptr, 14) };
    let mmap = unsafe { DATASET_MMAP.as_ref().unwrap() };
    let num_clusters = unsafe { NUM_CLUSTERS };
    let centroids = unsafe { CENTROIDS.unwrap() };
    let offsets = unsafe { OFFSETS.unwrap() };

    let mut top_clusters = [(i32::MAX, 0usize); 8]; 

    for i in 0..num_clusters as usize {
        let mut dist_sq = 0i32;
        let c_base = i * 14;
        for j in 0..14 {
            let diff = (query[j] as i32) - (centroids[c_base + j] as i32);
            dist_sq += diff * diff;
        }

        if dist_sq < top_clusters[7].0 {
            insert_top_clusters(dist_sq, i, &mut top_clusters);
        }
    }

    let mut top_distances = [i32::MAX; 5];
    let mut top_labels = [0u8; 5];

    let data_start_offset = 8 + (num_clusters as usize) * 28 + (num_clusters as usize) * 8;

    for &(_, cluster_id) in top_clusters.iter() {
        let (rec_offset, rec_count) = offsets[cluster_id];
        if rec_count == 0 { continue; }
        
        let byte_offset = data_start_offset + (rec_offset as usize) * RECORD_SIZE;
        let chunk_data = &mmap[byte_offset .. byte_offset + (rec_count as usize) * RECORD_SIZE];
        
        for chunk in chunk_data.chunks_exact(32) {
            let mut dist_sq = 0i32;
            for j in 0..14 {
                let ref_val = i16::from_le_bytes([chunk[j * 2], chunk[j * 2 + 1]]) as i32;
                let diff = (query[j] as i32) - ref_val;
                dist_sq += diff * diff;
            }

            if dist_sq < top_distances[4] {
                let label = chunk[28];
                insert_top5(dist_sq, label, &mut top_distances, &mut top_labels);
            }
        }
    }

    top_labels.iter().map(|&l| l as i32).sum()
}

#[inline(always)]
fn insert_top_clusters(dist: i32, id: usize, arr: &mut [(i32, usize); 8]) {
    for i in 0..8 {
        if dist < arr[i].0 {
            for j in (i+1..8).rev() {
                arr[j] = arr[j-1];
            }
            arr[i] = (dist, id);
            break;
        }
    }
}

#[inline(always)]
fn insert_top5(dist: i32, label: u8, dists: &mut [i32; 5], labels: &mut [u8; 5]) {
    if dist < dists[0] {
        dists[4] = dists[3]; labels[4] = labels[3];
        dists[3] = dists[2]; labels[3] = labels[2];
        dists[2] = dists[1]; labels[2] = labels[1];
        dists[1] = dists[0]; labels[1] = labels[0];
        dists[0] = dist; labels[0] = label;
    } else if dist < dists[1] {
        dists[4] = dists[3]; labels[4] = labels[3];
        dists[3] = dists[2]; labels[3] = labels[2];
        dists[2] = dists[1]; labels[2] = labels[1];
        dists[1] = dist; labels[1] = label;
    } else if dist < dists[2] {
        dists[4] = dists[3]; labels[4] = labels[3];
        dists[3] = dists[2]; labels[3] = labels[2];
        dists[2] = dist; labels[2] = label;
    } else if dist < dists[3] {
        dists[4] = dists[3]; labels[4] = labels[3];
        dists[3] = dist; labels[3] = label;
    } else {
        dists[4] = dist; labels[4] = label;
    }
}
