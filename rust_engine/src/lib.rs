use std::ffi::CStr;
use std::fs::File;
use std::os::raw::c_char;
use std::slice;
use memmap2::{Mmap, MmapOptions};
use std::arch::x86_64::*;

#[repr(C)]
struct ClusterHeader {
    centroid: [f32; 14],
    radius_sq: f32,
    offset: u32,
    count: u32,
    _pad: [u8; 12],
} // 80 bytes

#[repr(C)]
struct Point {
    vector: [f32; 14],
    label: u8,
    _pad: [u8; 7],
} // 64 bytes

static mut DATASET_MMAP: Option<Mmap> = None;
static mut CLUSTERS: Option<&'static [ClusterHeader]> = None;
static mut POINTS: Option<&'static [Point]> = None;

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
        // Align 64
        let clusters_ptr = mmap.as_ptr().add(64) as *const ClusterHeader;
        CLUSTERS = Some(slice::from_raw_parts(clusters_ptr, k));
        
        let points_ptr = mmap.as_ptr().add(64 + k * 80) as *const Point;
        let num_points = (mmap.len() - (64 + k * 80)) / 64;
        POINTS = Some(slice::from_raw_parts(points_ptr, num_points));

        DATASET_MMAP = Some(mmap);
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn search(query_ptr: *const f32) -> i32 {
    let q_f32 = slice::from_raw_parts(query_ptr, 14);
    let mut q_vec = [0.0f32; 16];
    q_vec[..14].copy_from_slice(q_f32);
    
    let clusters = CLUSTERS.as_ref().unwrap();
    let points = POINTS.as_ref().unwrap();

    let mut top_dists_sq = [f64::MAX; 5];
    let mut top_labels = [0u8; 5];
    let mut tau_sq = f64::MAX;

    let q0 = _mm256_loadu_ps(q_vec.as_ptr());
    let q1 = _mm256_loadu_ps(q_vec.as_ptr().add(8));
    let mask1 = _mm256_setr_ps(1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0);

    let mut cluster_dists_sq = Vec::with_capacity(clusters.len());
    for (i, c) in clusters.iter().enumerate() {
        let mut c_vec = [0.0f32; 16];
        c_vec[..14].copy_from_slice(&c.centroid);
        
        let n0 = _mm256_loadu_ps(c_vec.as_ptr());
        let n1 = _mm256_loadu_ps(c_vec.as_ptr().add(8));
        let d0 = _mm256_sub_ps(q0, n0);
        let d1 = _mm256_sub_ps(q1, n1);
        let d1m = _mm256_mul_ps(d1, mask1);
        let s0 = _mm256_mul_ps(d0, d0);
        let s1 = _mm256_fmadd_ps(d1m, d1m, s0);
        let d2 = hsum_ps_avx(s1) as f64;
        
        cluster_dists_sq.push((d2, i));
    }
    cluster_dists_sq.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

    for (dist_to_centroid_sq, idx) in cluster_dists_sq {
        let c = &clusters[idx];
        
        let dist_to_centroid = dist_to_centroid_sq.sqrt();
        let radius = (c.radius_sq as f64).sqrt();
        let tau = tau_sq.sqrt();
        
        if dist_to_centroid - radius >= tau {
            continue;
        }

        let start = c.offset as usize;
        let count = c.count as usize;
        for i in 0..count {
            let p = points.get_unchecked(start + i);
            let n0 = _mm256_loadu_ps(p.vector.as_ptr());
            let n1 = _mm256_loadu_ps(p.vector.as_ptr().add(8));
            let d0 = _mm256_sub_ps(q0, n0);
            let d1 = _mm256_sub_ps(q1, n1);
            let d1m = _mm256_mul_ps(d1, mask1);
            let s0 = _mm256_mul_ps(d0, d0);
            let s1 = _mm256_fmadd_ps(d1m, d1m, s0);
            let d2 = hsum_ps_avx(s1) as f64;

            if d2 < tau_sq {
                insert_top5(d2, p.label, &mut top_dists_sq, &mut top_labels);
                tau_sq = top_dists_sq[4];
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
fn insert_top5(dist_sq: f64, label: u8, dists_sq: &mut [f64; 5], labels: &mut [u8; 5]) {
    if dist_sq < dists_sq[0] {
        dists_sq[4] = dists_sq[3]; labels[4] = labels[3];
        dists_sq[3] = dists_sq[2]; labels[3] = labels[2];
        dists_sq[2] = dists_sq[1]; labels[2] = labels[1];
        dists_sq[1] = dists_sq[0]; labels[1] = labels[0];
        dists_sq[0] = dist_sq; labels[0] = label;
    } else if dist_sq < dists_sq[1] {
        dists_sq[4] = dists_sq[3]; labels[4] = labels[3];
        dists_sq[3] = dists_sq[2]; labels[3] = labels[2];
        dists_sq[2] = dists_sq[1]; labels[2] = labels[1];
        dists_sq[1] = dist_sq; labels[1] = label;
    } else if dist_sq < dists_sq[2] {
        dists_sq[4] = dists_sq[3]; labels[4] = labels[3];
        dists_sq[3] = dists_sq[2]; labels[3] = labels[2];
        dists_sq[2] = dist_sq; labels[2] = label;
    } else if dist_sq < dists_sq[3] {
        dists_sq[4] = dists_sq[3]; labels[4] = labels[3];
        dists_sq[3] = dist_sq; labels[3] = label;
    } else if dist_sq < dists_sq[4] {
        dists_sq[4] = dist_sq; labels[4] = label;
    }
}
