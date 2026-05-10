use std::ffi::CStr;
use std::fs::File;
use std::os::raw::c_char;
use std::slice;
use memmap2::{Mmap, MmapOptions};
use std::arch::x86_64::*;

#[repr(C)]
#[derive(Clone, Copy)]
struct ClusterHeader {
    centroid: [f32; 14],
    bbox_min: [i16; 14],
    bbox_max: [i16; 14],
    radius_sq: f32,
    offset: u32,
    count: u32,
    _pad: [u8; 4],
} // 128 bytes

#[repr(C)]
#[derive(Clone, Copy)]
struct Point {
    vector: [i16; 14],
    _pad_simd: [i16; 2], // Pad to 16 elements (32 bytes) for safe SIMD load
    index: u32,
    label: u8,
    _pad: [u8; 3],
} // 40 bytes

static mut DATASET_MMAP: Option<Mmap> = None;
static mut CLUSTERS: Option<&'static [ClusterHeader]> = None;
static mut POINTS: Option<&'static [Point]> = None;

const SCALE: f32 = 10000.0;

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
        let k = *(mmap.as_ptr() as *const u32) as usize;
        let clusters_ptr = mmap.as_ptr().add(64) as *const ClusterHeader;
        CLUSTERS = Some(slice::from_raw_parts(clusters_ptr, k));
        let points_ptr = mmap.as_ptr().add(64 + k * 128) as *const Point;
        let num_points = (mmap.len() - (64 + k * 128)) / 40;
        POINTS = Some(slice::from_raw_parts(points_ptr, num_points));
        DATASET_MMAP = Some(mmap);
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn search(query_ptr: *const f32) -> i32 {
    let q_f32 = slice::from_raw_parts(query_ptr, 14);
    
    // Scale query once for integer SIMD (Simple fallback for reliability)
    let mut q_i16 = [0i16; 16];
    for i in 0..14 {
        q_i16[i] = (q_f32[i] * SCALE).round() as i16;
    }
    
    let clusters = CLUSTERS.as_ref().unwrap();
    let points = POINTS.as_ref().unwrap();

    let mut top_dists_sq = [f64::MAX; 5];
    let mut top_indices = [u32::MAX; 5];
    let mut top_labels = [0u8; 5];

    // Pre-calculate query registers for candidate filtering (Float SIMD)
    let mut q_padded = [0.0f32; 16];
    q_padded[..14].copy_from_slice(q_f32);
    let q0_f32 = _mm256_loadu_ps(q_padded.as_ptr());
    let q1_f32 = _mm256_loadu_ps(q_padded.as_ptr().add(8));
    let mask1_f32 = _mm256_setr_ps(1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0);

    let q_i16_reg = _mm256_loadu_si256(q_i16.as_ptr() as *const __m256i);

    // 1. Candidate Filtering using Float Centroids
    let mut cluster_candidates = Vec::with_capacity(clusters.len());
    for (i, c) in clusters.iter().enumerate() {
        let mut c_vec = [0.0f32; 16];
        c_vec[..14].copy_from_slice(&c.centroid);
        let n0 = _mm256_loadu_ps(c_vec.as_ptr());
        let n1 = _mm256_loadu_ps(c_vec.as_ptr().add(8));
        let d0 = _mm256_sub_ps(q0_f32, n0);
        let d1 = _mm256_sub_ps(q1_f32, n1);
        let d1m = _mm256_mul_ps(d1, mask1_f32);
        let s0 = _mm256_mul_ps(d0, d0);
        let s1 = _mm256_fmadd_ps(d1m, d1m, s0);
        cluster_candidates.push((hsum_ps_avx(s1).sqrt() as f64, i));
    }
    cluster_candidates.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

    // 2. Intra-cluster search with Integer SIMD (NITRO MODE)
    for (dist_to_centroid, idx) in cluster_candidates {
        let c = &clusters[idx];
        
        // --- Pruning Step ---
        let radius = (c.radius_sq as f64).sqrt();
        let tau = top_dists_sq[4].sqrt();
        
        if dist_to_centroid - radius > tau {
            continue;
        }
        
        // Extra tight check: Bounding Box (Using i64 to prevent overflow)
        if top_dists_sq[4] != f64::MAX {
            let mut outside_bbox = false;
            let thresh_i64 = (top_dists_sq[4].sqrt() * SCALE as f64).round() as i64;
            for i in 0..14 {
                let q_val = q_i16[i] as i64;
                let b_min = c.bbox_min[i] as i64;
                let b_max = c.bbox_max[i] as i64;
                if q_val < b_min - thresh_i64 || q_val > b_max + thresh_i64 {
                    outside_bbox = true;
                    break;
                }
            }
            if outside_bbox {
                continue;
            }
        }
        // --------------------

        let start = c.offset as usize;
        let count = c.count as usize;
        
        for i in 0..count {
            let p = points.get_unchecked(start + i);
            let p_i16_reg = _mm256_loadu_si256(p.vector.as_ptr() as *const __m256i);
            let diff = _mm256_sub_epi16(q_i16_reg, p_i16_reg);
            let sq = _mm256_madd_epi16(diff, diff); 
            let d2 = hsum_epi32_avx(sq) as f64 / (SCALE as f64 * SCALE as f64);

            if d2 < top_dists_sq[4] || (d2 == top_dists_sq[4] && p.index < top_indices[4]) {
                insert_top5(d2, p.index, p.label, &mut top_dists_sq, &mut top_indices, &mut top_labels);
            }
        }
    }
    top_labels.iter().map(|&l| l as i32).sum()
}

#[inline(always)]
unsafe fn hsum_ps_avx(v: __m256) -> f32 {
    let x128 = _mm_add_ps(_mm256_extractf128_ps(v, 1), _mm256_castps256_ps128(v));
    let x64 = _mm_add_ps(x128, _mm_movehl_ps(x128, x128));
    let x32 = _mm_add_ss(x64, _mm_shuffle_ps(x64, x64, 0x55));
    _mm_cvtss_f32(x32)
}

#[inline(always)]
unsafe fn hsum_epi32_avx(v: __m256i) -> i32 {
    let x128 = _mm_add_epi32(_mm256_extractf128_si256(v, 1), _mm256_castsi256_si128(v));
    let x64 = _mm_add_epi32(x128, _mm_shuffle_epi32(x128, 0x0E));
    let x32 = _mm_add_epi32(x64, _mm_shuffle_epi32(x64, 0x01));
    _mm_cvtsi128_si32(x32)
}

#[inline(always)]
fn insert_top5(dist_sq: f64, index: u32, label: u8, dists_sq: &mut [f64; 5], indices: &mut [u32; 5], labels: &mut [u8; 5]) {
    let mut pos = 0;
    while pos < 5 {
        if dist_sq < dists_sq[pos] || (dist_sq == dists_sq[pos] && index < indices[pos]) {
            break;
        }
        pos += 1;
    }
    if pos < 5 {
        for i in (pos+1..5).rev() {
            dists_sq[i] = dists_sq[i-1];
            indices[i] = indices[i-1];
            labels[i] = labels[i-1];
        }
        dists_sq[pos] = dist_sq;
        indices[pos] = index;
        labels[pos] = label;
    }
}
